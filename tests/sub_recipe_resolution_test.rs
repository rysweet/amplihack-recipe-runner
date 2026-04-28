//! Regression tests for sub-recipe resolution.
//!
//! Issue: rysweet/amplihack-rs#480 — `smart-orchestrator` and `default-workflow`
//! could not locate the sub-recipes they reference (`smart-classify-route`,
//! `workflow-prep`, …) because the runner only consulted
//! [`recipe_runner_rs::discovery`]'s default search dirs, which are interpreted
//! relative to the runner subprocess's actual `cwd`. When the CLI launcher
//! invokes `recipe-runner-rs` from a directory other than the repo root, the
//! `amplifier-bundle/recipes` sub-directory is not found and resolution fails
//! with the unhelpful message `Sub-recipe '<name>' not found`.
//!
//! The fix anchors sub-recipe search to:
//!   1. the directory containing the top-level recipe file (recipe origin),
//!   2. the runner's `--working-dir` (`-C`) argument,
//!   3. ancestors of the working dir up to a `.git` marker.
//!
//! These tests cover positive resolution paths from each anchor as well as
//! negative cases that defend against path-traversal and symlink-escape.

use recipe_runner_rs::adapters::Adapter;
use recipe_runner_rs::parser::RecipeParser;
use recipe_runner_rs::runner::RecipeRunner;

/// Minimal adapter that records nothing — sub-recipe resolution happens at
/// parse/dispatch time, before any agent or bash adapter call is made.
struct NoopAdapter;

impl Adapter for NoopAdapter {
    fn execute_agent_step(
        &self,
        _prompt: &str,
        _agent_name: Option<&str>,
        _system_prompt: Option<&str>,
        _mode: Option<&str>,
        _working_dir: &str,
        _model: Option<&str>,
        _timeout: Option<u64>,
    ) -> Result<String, anyhow::Error> {
        Ok(String::new())
    }
    fn execute_bash_step(
        &self,
        _command: &str,
        _working_dir: &str,
        _timeout: Option<u64>,
        _extra_env: &std::collections::HashMap<String, String>,
    ) -> Result<String, anyhow::Error> {
        Ok(String::new())
    }
    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "noop"
    }
}

const PARENT_RECIPE: &str = r#"
name: parent-orchestrator
description: A top-level recipe that references a sibling sub-recipe by name.
steps:
  - id: dispatch
    recipe: "child-step"
"#;

const CHILD_RECIPE: &str = r#"
name: child-step
description: A sibling sub-recipe referenced from parent-orchestrator.
steps:
  - id: noop
    command: "echo child-ran"
"#;

/// (a) Sub-recipe co-located with its parent must resolve via the
/// recipe-origin anchor — even when the runner's working directory is
/// somewhere completely different.
#[test]
fn sub_recipe_resolves_from_recipe_origin_dir() {
    let recipes_dir = tempfile::tempdir().unwrap();
    let parent_path = recipes_dir.path().join("parent-orchestrator.yaml");
    std::fs::write(&parent_path, PARENT_RECIPE).unwrap();
    std::fs::write(recipes_dir.path().join("child-step.yaml"), CHILD_RECIPE).unwrap();

    let unrelated_cwd = tempfile::tempdir().unwrap();

    let parser = RecipeParser::new();
    let recipe = parser.parse_file(&parent_path).unwrap();
    let runner = RecipeRunner::new(NoopAdapter)
        .with_working_dir(unrelated_cwd.path().to_str().unwrap())
        .with_recipe_origin_dir(recipes_dir.path().to_path_buf());
    let result = runner.execute(&recipe, None);

    assert!(
        result.success,
        "sub-recipe must resolve from recipe-origin dir; got: {:?}",
        result.step_results.iter().map(|s| (&s.step_id, &s.status)).collect::<Vec<_>>()
    );
}

/// (b) Sub-recipe under `<working_dir>/amplifier-bundle/recipes` must resolve
/// even when the runner's subprocess `cwd` differs from `working_dir`.
#[test]
fn sub_recipe_resolves_from_working_dir_bundle() {
    let project_root = tempfile::tempdir().unwrap();
    let bundle = project_root.path().join("amplifier-bundle").join("recipes");
    std::fs::create_dir_all(&bundle).unwrap();
    let parent_path = bundle.join("parent-orchestrator.yaml");
    std::fs::write(&parent_path, PARENT_RECIPE).unwrap();
    std::fs::write(bundle.join("child-step.yaml"), CHILD_RECIPE).unwrap();

    let parser = RecipeParser::new();
    let recipe = parser.parse_file(&parent_path).unwrap();
    let runner = RecipeRunner::new(NoopAdapter)
        .with_working_dir(project_root.path().to_str().unwrap());
    let result = runner.execute(&recipe, None);
    assert!(result.success, "sub-recipe must resolve via working_dir/amplifier-bundle/recipes");
}

/// (c) Walk-up resolution: parent invoked from a subdir of a repo whose root
/// holds `amplifier-bundle/recipes`. The resolver must climb to the `.git`
/// marker and find the bundle there.
#[test]
fn sub_recipe_resolves_via_walk_up_to_git() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join(".git")).unwrap();
    let bundle = repo.path().join("amplifier-bundle").join("recipes");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("child-step.yaml"), CHILD_RECIPE).unwrap();

    // Parent recipe lives elsewhere — we are testing repo-root discovery,
    // not recipe-origin discovery. The runner's working dir is a deep subdir.
    let recipe_dir = tempfile::tempdir().unwrap();
    let parent_path = recipe_dir.path().join("parent.yaml");
    std::fs::write(&parent_path, PARENT_RECIPE).unwrap();

    let nested = repo.path().join("crates").join("subproject");
    std::fs::create_dir_all(&nested).unwrap();

    let parser = RecipeParser::new();
    let recipe = parser.parse_file(&parent_path).unwrap();
    let runner = RecipeRunner::new(NoopAdapter)
        .with_working_dir(nested.to_str().unwrap())
        // Recipe origin intentionally NOT set — exercise the walk-up path.
        ;
    let result = runner.execute(&recipe, None);
    assert!(
        result.success,
        "sub-recipe must resolve by walking up from working_dir to .git root"
    );
}

/// (d) Resolution failure must produce a Zero-BS diagnostic that lists every
/// directory the runner consulted. No silent fallback.
#[test]
fn missing_sub_recipe_diagnostic_lists_all_search_dirs() {
    let recipes_dir = tempfile::tempdir().unwrap();
    let parent_path = recipes_dir.path().join("parent.yaml");
    std::fs::write(&parent_path, PARENT_RECIPE).unwrap();
    // Intentionally do NOT write child-step.yaml.

    let parser = RecipeParser::new();
    let recipe = parser.parse_file(&parent_path).unwrap();
    let runner = RecipeRunner::new(NoopAdapter)
        .with_working_dir(recipes_dir.path().to_str().unwrap())
        .with_recipe_origin_dir(recipes_dir.path().to_path_buf());
    let result = runner.execute(&recipe, None);

    assert!(!result.success, "missing sub-recipe must fail the recipe");
    let dispatch = result
        .step_results
        .iter()
        .find(|s| s.step_id == "dispatch")
        .expect("dispatch step result missing");
    let err = dispatch.error.as_str();
    assert!(
        err.contains("Sub-recipe 'child-step' not found"),
        "diagnostic must name the missing sub-recipe; got: {err}"
    );
    assert!(
        err.contains("Searched the following directories"),
        "diagnostic must enumerate searched dirs; got: {err}"
    );
    assert!(
        err.contains(recipes_dir.path().to_str().unwrap()),
        "diagnostic must include the recipe-origin dir; got: {err}"
    );
}

/// (e) Negative — a sub-recipe name with traversal segments must be rejected
/// at the validation layer, before any filesystem resolution. The error must
/// not silently fall through to a different recipe.
#[test]
fn sub_recipe_name_with_traversal_is_rejected() {
    let recipes_dir = tempfile::tempdir().unwrap();
    let parent = r#"
name: parent
steps:
  - id: dispatch
    recipe: "../escape"
"#;
    let parent_path = recipes_dir.path().join("parent.yaml");
    std::fs::write(&parent_path, parent).unwrap();

    let parser = RecipeParser::new();
    // The parser may either reject this at parse time (preferred) or accept
    // it and let the runner reject it at dispatch time. Both outcomes are
    // acceptable for the security guarantee — what is NOT acceptable is for
    // resolution to succeed and read a file outside the search roots.
    match parser.parse_file(&parent_path) {
        Err(_) => {
            // Parser-level rejection is fine.
        }
        Ok(recipe) => {
            let runner = RecipeRunner::new(NoopAdapter)
                .with_working_dir(recipes_dir.path().to_str().unwrap())
                .with_recipe_origin_dir(recipes_dir.path().to_path_buf());
            let result = runner.execute(&recipe, None);
            assert!(!result.success, "name with '..' must not resolve");
        }
    }
}

/// (f) Negative (Unix only) — a symlink inside an anchored search dir that
/// points to a file outside any anchored root must NOT resolve. This defends
/// against an attacker placing a symlink in `amplifier-bundle/recipes/` to
/// trick the runner into reading an arbitrary file.
#[test]
#[cfg(unix)]
fn sub_recipe_symlink_escape_is_rejected() {
    use std::os::unix::fs::symlink;

    // Use a unique recipe name that cannot exist in any global default
    // search dir (e.g. ~/.amplihack/amplifier-bundle/recipes), so that
    // resolution failure unambiguously tests the containment check.
    let unique = format!("sym-escape-{}", std::process::id());
    let parent_yaml = format!(
        "name: parent\nsteps:\n  - id: dispatch\n    recipe: \"{unique}\"\n"
    );
    let child_yaml = format!(
        "name: {unique}\nsteps:\n  - id: noop\n    command: \"echo\"\n"
    );

    let bundle_dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();

    // Place a "real" recipe outside the search root.
    let secret = outside.path().join(format!("{unique}.yaml"));
    std::fs::write(&secret, child_yaml).unwrap();

    // And a symlink inside the search root pointing at it.
    symlink(&secret, bundle_dir.path().join(format!("{unique}.yaml"))).unwrap();

    let parent_path = bundle_dir.path().join("parent.yaml");
    std::fs::write(&parent_path, parent_yaml).unwrap();

    let parser = RecipeParser::new();
    let recipe = parser.parse_file(&parent_path).unwrap();
    let runner = RecipeRunner::new(NoopAdapter)
        .with_working_dir(bundle_dir.path().to_str().unwrap())
        .with_recipe_origin_dir(bundle_dir.path().to_path_buf());
    let result = runner.execute(&recipe, None);

    // The containment check rejects the symlink; resolution falls through to
    // discovery defaults (which won't find it either) and fails with the
    // Zero-BS diagnostic.
    assert!(
        !result.success,
        "symlink that escapes anchored roots must be rejected"
    );
}
