//! Outside-in end-to-end tests for recipe-runner-rs.
//!
//! These tests exercise the COMPILED BINARY via `std::process::Command`,
//! exactly as a user would invoke it from the command line.
//! They validate fixes for issues #2954, #2953, and #2792.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;

static BUILD_ONCE: Once = Once::new();

/// Ensure the binary is built before any test runs.
fn ensure_built() {
    BUILD_ONCE.call_once(|| {
        let status = Command::new("cargo")
            .args(["build", "--quiet"])
            .current_dir(project_root())
            .status()
            .expect("failed to run cargo build");
        assert!(status.success(), "cargo build failed");
    });
}

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn binary_path() -> PathBuf {
    project_root().join("target/debug/recipe-runner-rs")
}

/// Write a YAML recipe file into the given directory and return its path.
fn write_recipe(dir: &Path, filename: &str, content: &str) -> PathBuf {
    let path = dir.join(filename);
    std::fs::write(&path, content).expect("failed to write recipe file");
    path
}

/// Run the binary with the given recipe and extra args, returning (exit_code, stdout, stderr).
fn run_binary(recipe_path: &Path, extra_args: &[&str]) -> (i32, String, String) {
    ensure_built();
    let output = Command::new(binary_path())
        .arg(recipe_path)
        .args(extra_args)
        .output()
        .expect("failed to execute binary");

    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (code, stdout, stderr)
}

/// Run binary and parse stdout as JSON.
fn run_json(recipe_path: &Path, extra_args: &[&str]) -> (i32, Value, String) {
    let mut args = vec!["--output-format", "json"];
    args.extend_from_slice(extra_args);
    let (code, stdout, stderr) = run_binary(recipe_path, &args);
    let json: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("Failed to parse JSON output: {e}\nstdout: {stdout}\nstderr: {stderr}");
    });
    (code, json, stderr)
}

/// Find a step result by id in the JSON output.
fn find_step<'a>(json: &'a Value, step_id: &str) -> Option<&'a Value> {
    json["step_results"]
        .as_array()
        .and_then(|arr| arr.iter().find(|s| s["step_id"].as_str() == Some(step_id)))
}

// ---------------------------------------------------------------------------
// Issue #2954: parse_json + continue_on_error interaction
// ---------------------------------------------------------------------------

#[test]
fn test_parse_json_failure_degrades_by_default() {
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "recipe.yaml",
        r#"
name: test-parse-json-degrade
steps:
  - id: bad-json
    command: "echo 'this is not json at all'"
    parse_json: true
  - id: should-run
    command: "echo 'reached'"
"#,
    );

    let (code, json, _stderr) = run_json(&recipe, &[]);

    // With parse_json_required defaulting to false, the recipe degrades gracefully.
    assert_eq!(code, 0, "recipe should succeed with degraded step");
    let bad = find_step(&json, "bad-json").expect("bad-json step should exist");
    assert_eq!(bad["status"].as_str().unwrap(), "degraded");

    // The second step should have executed.
    let good = find_step(&json, "should-run").expect("should-run step should exist");
    assert_eq!(good["status"].as_str().unwrap(), "completed");
}

#[test]
fn test_parse_json_required_kills_recipe() {
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "recipe.yaml",
        r#"
name: test-parse-json-required
steps:
  - id: bad-json
    command: "echo 'this is not json at all'"
    parse_json: true
    parse_json_required: true
  - id: should-not-run
    command: "echo 'reached'"
"#,
    );

    let (code, json, _stderr) = run_json(&recipe, &[]);

    assert_eq!(
        code, 1,
        "recipe should fail when parse_json_required is true"
    );
    let bad = find_step(&json, "bad-json").expect("bad-json step should exist");
    assert_eq!(bad["status"].as_str().unwrap(), "failed");

    // The second step should not have executed.
    match find_step(&json, "should-not-run") {
        None => {}
        Some(step) => {
            assert_ne!(
                step["status"].as_str().unwrap(),
                "completed",
                "should-not-run must not complete when prior step failed"
            );
        }
    }
}

#[test]
fn test_parse_json_failure_with_continue_on_error() {
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "recipe.yaml",
        r#"
name: test-parse-json-continue
steps:
  - id: bad-json
    command: "echo 'this is not json at all'"
    parse_json: true
    continue_on_error: true
  - id: should-run
    command: "echo 'reached second step'"
"#,
    );

    let (code, json, _stderr) = run_json(&recipe, &[]);

    // parse_json failure degrades gracefully (parse_json_required defaults to false).
    assert_eq!(code, 0, "recipe should succeed with degraded step");
    let bad = find_step(&json, "bad-json").expect("bad-json step should exist");
    assert_eq!(bad["status"].as_str().unwrap(), "degraded");

    let good = find_step(&json, "should-run").expect("should-run step should exist");
    assert_eq!(
        good["status"].as_str().unwrap(),
        "completed",
        "second step must execute when parse_json degrades"
    );
}

#[test]
fn test_parse_json_success_produces_parsed_output() {
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "recipe.yaml",
        r#"
name: test-parse-json-success
steps:
  - id: good-json
    command: "echo '{\"key\": \"value\", \"count\": 42}'"
    parse_json: true
    output: json_result
"#,
    );

    let (code, json, _stderr) = run_json(&recipe, &[]);

    assert_eq!(code, 0, "recipe should succeed");
    let step = find_step(&json, "good-json").expect("good-json step should exist");
    assert_eq!(step["status"].as_str().unwrap(), "completed");

    // The context should contain the parsed JSON under "json_result"
    let ctx = &json["context"];
    assert!(
        !ctx.is_null(),
        "context should be present with parsed JSON output"
    );
    let result = &ctx["json_result"];
    assert!(!result.is_null(), "json_result should be in context");
    // Verify parsed structure
    if result.is_object() {
        assert_eq!(result["key"].as_str().unwrap(), "value");
        assert_eq!(result["count"].as_i64().unwrap(), 42);
    }
}

// ---------------------------------------------------------------------------
// Issue #2953: recovery_on_failure for sub-recipes
// ---------------------------------------------------------------------------

#[test]
fn test_sub_recipe_failure_without_recovery_fails() {
    let dir = TempDir::new().unwrap();

    write_recipe(
        dir.path(),
        "sub-recipe-fail.yaml",
        r#"
name: sub-recipe-fail
steps:
  - id: will-fail
    command: "exit 1"
"#,
    );

    let recipe = write_recipe(
        dir.path(),
        "parent-no-recovery.yaml",
        r#"
name: parent-no-recovery
steps:
  - id: run-sub
    type: recipe
    recipe: sub-recipe-fail
  - id: after-sub
    command: "echo 'should not run'"
"#,
    );

    let (code, json, _stderr) = run_json(&recipe, &["-R", dir.path().to_str().unwrap()]);

    assert_eq!(code, 1, "parent recipe should fail when sub-recipe fails");

    let sub = find_step(&json, "run-sub").expect("run-sub step should exist");
    assert_eq!(sub["status"].as_str().unwrap(), "failed");

    // after-sub should not have executed
    match find_step(&json, "after-sub") {
        None => {}
        Some(step) => {
            assert_ne!(
                step["status"].as_str().unwrap(),
                "completed",
                "after-sub must not complete after sub-recipe failure"
            );
        }
    }
}

#[test]
fn test_recovery_on_failure_field_accepted_in_yaml() {
    let dir = TempDir::new().unwrap();

    write_recipe(
        dir.path(),
        "sub-recipe-fail.yaml",
        r#"
name: sub-recipe-fail
steps:
  - id: will-fail
    command: "exit 1"
"#,
    );

    let recipe = write_recipe(
        dir.path(),
        "parent-with-recovery.yaml",
        r#"
name: parent-with-recovery
steps:
  - id: run-sub
    type: recipe
    recipe: sub-recipe-fail
    recovery_on_failure: true
  - id: after-sub
    command: "echo 'after recovery'"
"#,
    );

    // Validate-only should succeed — the YAML field is recognized.
    let (code, _stdout, stderr) = run_binary(
        &recipe,
        &["--validate-only", "-R", dir.path().to_str().unwrap()],
    );

    assert_eq!(
        code, 0,
        "validate-only should pass when recovery_on_failure is used.\nstderr: {stderr}"
    );
    // No "unknown field" errors
    assert!(
        !stderr.contains("unknown field"),
        "should not warn about unknown field recovery_on_failure"
    );
}

// ---------------------------------------------------------------------------
// Issue #2792: per-step model field
// ---------------------------------------------------------------------------

#[test]
fn test_model_field_accepted_in_dry_run() {
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "recipe.yaml",
        r#"
name: test-model-field
steps:
  - id: classify
    type: agent
    prompt: "Classify this task"
    model: "haiku"
  - id: implement
    type: agent
    prompt: "Implement this"
    model: "sonnet"
"#,
    );

    let (code, json, stderr) = run_json(&recipe, &["--dry-run"]);

    assert_eq!(
        code, 0,
        "dry-run should succeed with model field.\nstderr: {stderr}"
    );

    // Both steps should appear in output
    assert!(
        find_step(&json, "classify").is_some(),
        "classify step should exist"
    );
    assert!(
        find_step(&json, "implement").is_some(),
        "implement step should exist"
    );

    // No errors about unknown "model" field
    assert!(
        !stderr.contains("unknown field"),
        "should not warn about unknown field model"
    );
}

#[test]
fn test_model_field_validate_only() {
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "recipe.yaml",
        r#"
name: test-model-validate
steps:
  - id: classify
    type: agent
    prompt: "Classify this"
    model: "haiku"
  - id: implement
    type: agent
    prompt: "Implement"
    model: "sonnet"
"#,
    );

    let (code, _stdout, stderr) = run_binary(&recipe, &["--validate-only"]);

    assert_eq!(
        code, 0,
        "validate-only should pass with model field.\nstderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Quality audit fixes: circular extends, on_error hooks, parallel groups
// ---------------------------------------------------------------------------

#[test]
fn test_circular_extends_detected_and_rejected() {
    let dir = TempDir::new().unwrap();
    write_recipe(
        dir.path(),
        "recipe-a.yaml",
        r#"
name: recipe-a
extends: recipe-b
steps:
  - id: a-step
    command: "echo a"
"#,
    );
    write_recipe(
        dir.path(),
        "recipe-b.yaml",
        r#"
name: recipe-b
extends: recipe-a
steps:
  - id: b-step
    command: "echo b"
"#,
    );

    let recipe_a = dir.path().join("recipe-a.yaml");
    let (code, _stdout, stderr) = run_binary(&recipe_a, &["-R", dir.path().to_str().unwrap()]);

    assert_eq!(code, 1, "circular extends should fail the recipe");
    assert!(
        stderr.contains("Circular")
            || stderr.contains("circular")
            || stderr.contains("already visited"),
        "error should mention circular extends detection, got stderr: {stderr}"
    );
}

#[test]
fn test_on_error_hook_executes_on_step_failure() {
    let dir = TempDir::new().unwrap();
    let marker = dir.path().join("on_error_marker.txt");

    let recipe = write_recipe(
        dir.path(),
        "recipe.yaml",
        &format!(
            r#"
name: on-error-hook-e2e
hooks:
  on_error: "touch {}"
steps:
  - id: failing-step
    command: "exit 1"
    continue_on_error: true
  - id: after-failure
    command: "echo done"
"#,
            marker.display()
        ),
    );

    let (code, json, stderr) = run_json(&recipe, &[]);

    assert_eq!(
        code, 0,
        "recipe should succeed (continue_on_error). stderr: {stderr}"
    );

    // The on_error hook should have created the marker file
    assert!(
        marker.exists(),
        "on_error hook should have created marker file at {}",
        marker.display()
    );

    // The step after the failure should have completed
    let after = find_step(&json, "after-failure").expect("after-failure step should exist");
    assert_eq!(after["status"].as_str().unwrap(), "completed");
}

#[test]
fn test_parallel_group_execution_via_binary() {
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "recipe.yaml",
        r#"
name: parallel-e2e
steps:
  - id: par-a
    command: "echo a"
    parallel_group: grp1
  - id: par-b
    command: "echo b"
    parallel_group: grp1
  - id: par-c
    command: "echo c"
    parallel_group: grp1
  - id: after-parallel
    command: "echo 'after parallel'"
"#,
    );

    let (code, json, stderr) = run_json(&recipe, &[]);

    assert_eq!(
        code, 0,
        "parallel group recipe should succeed. stderr: {stderr}"
    );

    // All parallel steps should have completed
    for id in &["par-a", "par-b", "par-c", "after-parallel"] {
        let step = find_step(&json, id).unwrap_or_else(|| panic!("step {id} should exist"));
        assert_eq!(
            step["status"].as_str().unwrap(),
            "completed",
            "step {id} should be completed"
        );
    }
}

#[test]
fn test_binary_runs_without_which_crate() {
    // Validates that removing the `which` crate didn't break the binary.
    // The binary should start, parse a recipe, and execute bash steps.
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "recipe.yaml",
        r#"
name: post-cleanup-smoke
steps:
  - id: smoke
    command: "echo 'binary works without which crate'"
    output: result
"#,
    );

    let (code, json, stderr) = run_json(&recipe, &[]);

    assert_eq!(
        code, 0,
        "binary should work after removing which crate. stderr: {stderr}"
    );
    let step = find_step(&json, "smoke").expect("smoke step should exist");
    assert_eq!(step["status"].as_str().unwrap(), "completed");

    let ctx = &json["context"];
    let result = ctx["result"].as_str().unwrap_or("");
    assert!(
        result.contains("binary works without which crate"),
        "output should contain expected text, got: {result}"
    );
}

#[test]
fn test_validate_only_detects_no_regressions() {
    // Run validate-only on ALL example recipes to catch field parsing regressions
    let examples_dir = project_root().join("examples");
    if !examples_dir.exists() {
        return; // Skip if examples directory doesn't exist
    }

    ensure_built();
    for entry in std::fs::read_dir(&examples_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "yaml") {
            let output = Command::new(binary_path())
                .arg(&path)
                .arg("--validate-only")
                .args(["-R", examples_dir.to_str().unwrap()])
                .output()
                .expect("failed to execute binary");

            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                code,
                0,
                "validate-only failed for {}: stderr: {}",
                path.display(),
                stderr
            );
        }
    }
}

#[test]
fn test_recipe_name_resolution() {
    let dir = TempDir::new().unwrap();
    write_recipe(
        dir.path(),
        "my-test-recipe.yaml",
        r#"
name: my-test-recipe
steps:
  - id: hello
    command: "echo 'found by name'"
    output: greeting
"#,
    );

    ensure_built();
    let output = Command::new(binary_path())
        .arg("my-test-recipe") // name, not path
        .args(["-R", dir.path().to_str().unwrap()])
        .args(["--output-format", "json"])
        .output()
        .expect("failed to execute binary");

    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        code, 0,
        "recipe should be found by name via -R search path.\nstdout: {stdout}\nstderr: {stderr}"
    );

    let json: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("Failed to parse JSON: {e}\nstdout: {stdout}");
    });
    let step = find_step(&json, "hello").expect("hello step should exist");
    assert_eq!(step["status"].as_str().unwrap(), "completed");
}

#[test]
fn test_context_serialization_in_json_output() {
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "recipe.yaml",
        r#"
name: test-context-output
steps:
  - id: set-value
    command: "echo 'hello world'"
    output: my_value
"#,
    );

    let (code, json, _stderr) = run_json(&recipe, &[]);

    assert_eq!(code, 0, "recipe should succeed");

    let ctx = &json["context"];
    assert!(
        !ctx.is_null(),
        "context field should be present in JSON output"
    );
    let val = &ctx["my_value"];
    assert!(!val.is_null(), "my_value should appear in context");
    // The value should contain "hello world" (may have trailing newline)
    let s = val.as_str().unwrap_or_default();
    assert!(
        s.contains("hello world"),
        "my_value should contain 'hello world', got: {s}"
    );
}

#[test]
fn test_dry_run_returns_consistent_skipped_status() {
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "recipe.yaml",
        r#"
name: test-dry-run-status
steps:
  - id: step-a
    command: "echo 'a'"
  - id: step-b
    command: "echo 'b'"
  - id: step-c-par
    command: "echo 'c'"
    parallel_group: par
  - id: step-d-par
    command: "echo 'd'"
    parallel_group: par
  - id: step-e
    command: "echo 'e'"
"#,
    );

    let (code, json, _stderr) = run_json(&recipe, &["--dry-run"]);

    assert_eq!(code, 0, "dry-run should succeed");

    let steps = json["step_results"]
        .as_array()
        .expect("step_results should be an array");

    assert!(!steps.is_empty(), "dry-run should still list steps");

    for step in steps {
        let id = step["step_id"].as_str().unwrap_or("unknown");
        let status = step["status"].as_str().unwrap_or("missing");
        assert_eq!(
            status, "skipped",
            "dry-run step '{id}' should have status 'skipped', got '{status}'"
        );
    }
}

// ---------------------------------------------------------------------------
// Outside-In Tests — Quality Audit Cycles 3 & 4 coverage
// ---------------------------------------------------------------------------

/// User runs a recipe with a bash step whose error message includes full context.
/// (err-9: error chain context preserved in map_err)
#[test]
fn test_error_chain_context_in_step_failure() {
    let tmp = TempDir::new().unwrap();
    let recipe = write_recipe(
        tmp.path(),
        "fail.yaml",
        r#"
name: error-chain-test
steps:
  - id: will-fail
    command: "exit 42"
"#,
    );
    let (code, json, _stderr) = run_json(&recipe, &[]);
    assert_eq!(code, 1);
    let step = find_step(&json, "will-fail").expect("step not found");
    assert_eq!(step["status"].as_str().unwrap(), "failed");
    let error = step["error"].as_str().unwrap();
    assert!(
        error.contains("bash step failed"),
        "error should include 'bash step failed' prefix for chain context, got: {error}"
    );
}

/// User runs --explain to see recipe structure without executing.
#[test]
fn test_explain_mode_shows_recipe_structure() {
    let tmp = TempDir::new().unwrap();
    let recipe = write_recipe(
        tmp.path(),
        "explainable.yaml",
        r#"
name: explainable-recipe
version: "2.0"
description: "A recipe to explain"
steps:
  - id: step-alpha
    command: "echo alpha"
  - id: step-beta
    prompt: "do something"
    agent: "test-agent"
"#,
    );
    let (code, stdout, _stderr) = run_binary(&recipe, &["--explain"]);
    assert_eq!(code, 0, "explain should exit 0");
    assert!(
        stdout.contains("explainable-recipe"),
        "explain output should contain recipe name"
    );
    assert!(
        stdout.contains("step-alpha"),
        "explain output should list step IDs"
    );
    assert!(
        stdout.contains("step-beta"),
        "explain output should list all step IDs"
    );
}

/// User passes --version and gets a version string.
#[test]
fn test_version_flag() {
    ensure_built();
    let output = Command::new(binary_path())
        .arg("--version")
        .output()
        .expect("failed to run --version");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("recipe-runner"),
        "version output should contain binary name, got: {stdout}"
    );
}

/// User filters steps by --include-tags.
#[test]
fn test_include_tags_filters_steps() {
    let tmp = TempDir::new().unwrap();
    let recipe = write_recipe(
        tmp.path(),
        "tagged.yaml",
        r#"
name: tagged-recipe
steps:
  - id: fast-step
    command: "echo fast"
    when_tags: ["fast"]
  - id: slow-step
    command: "echo slow"
    when_tags: ["slow"]
  - id: untagged
    command: "echo always"
"#,
    );
    let (code, json, _stderr) = run_json(&recipe, &["--include-tags", "fast"]);
    assert_eq!(code, 0);
    let fast = find_step(&json, "fast-step").expect("fast-step not found");
    assert_eq!(fast["status"].as_str().unwrap(), "completed");
    let slow = find_step(&json, "slow-step").expect("slow-step not found");
    assert_eq!(
        slow["status"].as_str().unwrap(),
        "skipped",
        "slow-step should be skipped when only 'fast' tag is included"
    );
}

/// User filters steps by --exclude-tags.
#[test]
fn test_exclude_tags_skips_steps() {
    let tmp = TempDir::new().unwrap();
    let recipe = write_recipe(
        tmp.path(),
        "excluded.yaml",
        r#"
name: excluded-recipe
steps:
  - id: keep-me
    command: "echo kept"
    when_tags: ["keep"]
  - id: drop-me
    command: "echo dropped"
    when_tags: ["drop"]
"#,
    );
    let (code, json, _stderr) = run_json(&recipe, &["--exclude-tags", "drop"]);
    assert_eq!(code, 0);
    let kept = find_step(&json, "keep-me").expect("keep-me not found");
    assert_eq!(kept["status"].as_str().unwrap(), "completed");
    let dropped = find_step(&json, "drop-me").expect("drop-me not found");
    assert_eq!(dropped["status"].as_str().unwrap(), "skipped");
}

/// User passes --audit-dir and gets JSONL audit log written.
#[test]
fn test_audit_dir_creates_log_file() {
    let tmp = TempDir::new().unwrap();
    let audit_dir = tmp.path().join("audit");
    std::fs::create_dir_all(&audit_dir).unwrap();
    let recipe = write_recipe(
        tmp.path(),
        "auditable.yaml",
        r#"
name: auditable-recipe
steps:
  - id: logged-step
    command: "echo audited"
"#,
    );
    let (code, _stdout, _stderr) = run_binary(
        &recipe,
        &["--audit-dir", audit_dir.to_str().unwrap()],
    );
    assert_eq!(code, 0);
    // Audit dir should contain a .jsonl file
    let entries: Vec<_> = std::fs::read_dir(&audit_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == "jsonl")
        })
        .collect();
    assert!(
        !entries.is_empty(),
        "audit dir should contain at least one .jsonl file"
    );
    // Verify JSONL content is valid
    let content = std::fs::read_to_string(entries[0].path()).unwrap();
    for line in content.lines() {
        let parsed: Result<Value, _> = serde_json::from_str(line);
        assert!(parsed.is_ok(), "each JSONL line should be valid JSON: {line}");
    }
}

/// User sets context variable via --set and uses it in a condition.
#[test]
fn test_context_override_with_condition() {
    let tmp = TempDir::new().unwrap();
    let recipe = write_recipe(
        tmp.path(),
        "conditional.yaml",
        r#"
name: context-condition
context:
  mode: "default"
steps:
  - id: guarded
    command: "echo custom mode"
    condition: "mode == 'custom'"
  - id: default-step
    command: "echo default mode"
    condition: "mode == 'default'"
"#,
    );
    // Without override — default-step runs, guarded skips
    let (code, json, _) = run_json(&recipe, &[]);
    assert_eq!(code, 0);
    assert_eq!(
        find_step(&json, "guarded").unwrap()["status"].as_str().unwrap(),
        "skipped"
    );
    assert_eq!(
        find_step(&json, "default-step").unwrap()["status"].as_str().unwrap(),
        "completed"
    );

    // With override — guarded runs, default-step skips
    let (code, json, _) = run_json(&recipe, &["--set", "mode=custom"]);
    assert_eq!(code, 0);
    assert_eq!(
        find_step(&json, "guarded").unwrap()["status"].as_str().unwrap(),
        "completed"
    );
    assert_eq!(
        find_step(&json, "default-step").unwrap()["status"].as_str().unwrap(),
        "skipped"
    );
}

/// User uses condition with len() function and string methods in a recipe.
#[test]
fn test_condition_with_builtin_functions() {
    let tmp = TempDir::new().unwrap();
    let recipe = write_recipe(
        tmp.path(),
        "functions.yaml",
        r#"
name: function-conditions
context:
  items: ["a", "b", "c"]
  greeting: "  Hello World  "
steps:
  - id: len-check
    command: "echo len-ok"
    condition: "len(items) == 3"
  - id: strip-check
    command: "echo strip-ok"
    condition: "greeting.strip() == 'Hello World'"
  - id: lower-check
    command: "echo lower-ok"
    condition: "greeting.strip().lower() == 'hello world'"
"#,
    );
    let (code, json, _stderr) = run_json(&recipe, &[]);
    assert_eq!(code, 0);
    for step_id in &["len-check", "strip-check", "lower-check"] {
        let step = find_step(&json, step_id).unwrap_or_else(|| panic!("{step_id} not found"));
        assert_eq!(
            step["status"].as_str().unwrap(),
            "completed",
            "step {step_id} should be completed (condition should be true)"
        );
    }
}

/// User runs a recipe where a condition evaluates to empty/falsy — step is skipped.
#[test]
fn test_falsy_condition_skips_step() {
    let tmp = TempDir::new().unwrap();
    let recipe = write_recipe(
        tmp.path(),
        "falsy.yaml",
        r#"
name: falsy-conditions
context:
  empty_str: ""
  zero: 0
  missing_var_doesnt_exist: null
steps:
  - id: empty-string-guard
    command: "echo should-not-run"
    condition: "empty_str"
  - id: zero-guard
    command: "echo should-not-run"
    condition: "zero"
  - id: null-guard
    command: "echo should-not-run"
    condition: "missing_var_doesnt_exist"
  - id: always-runs
    command: "echo ok"
"#,
    );
    let (code, json, _stderr) = run_json(&recipe, &[]);
    assert_eq!(code, 0);
    for step_id in &["empty-string-guard", "zero-guard", "null-guard"] {
        let step = find_step(&json, step_id).unwrap_or_else(|| panic!("{step_id} not found"));
        assert_eq!(
            step["status"].as_str().unwrap(),
            "skipped",
            "step {step_id} should be skipped (falsy condition)"
        );
    }
    let always = find_step(&json, "always-runs").expect("always-runs not found");
    assert_eq!(always["status"].as_str().unwrap(), "completed");
}

/// User sets RECIPE_RUNNER_CACHE_TTL env var and the binary uses it without error.
#[test]
fn test_cache_ttl_env_var_accepted() {
    let tmp = TempDir::new().unwrap();
    let recipe = write_recipe(
        tmp.path(),
        "cache-test.yaml",
        r#"
name: cache-test
steps:
  - id: simple
    command: "echo cached"
"#,
    );
    ensure_built();
    let output = Command::new(binary_path())
        .arg(&recipe)
        .args(["--output-format", "json"])
        .env("RECIPE_RUNNER_CACHE_TTL", "5")
        .output()
        .expect("failed to execute");
    assert!(output.status.success(), "binary should accept RECIPE_RUNNER_CACHE_TTL env var");
}

/// User lists recipes via the `list` subcommand.
#[test]
fn test_list_subcommand() {
    let tmp = TempDir::new().unwrap();
    write_recipe(
        tmp.path(),
        "alpha.yaml",
        r#"
name: alpha-recipe
steps:
  - id: s1
    command: "echo a"
"#,
    );
    write_recipe(
        tmp.path(),
        "beta.yaml",
        r#"
name: beta-recipe
steps:
  - id: s1
    command: "echo b"
"#,
    );
    ensure_built();
    let output = Command::new(binary_path())
        .args(["list", "-R", tmp.path().to_str().unwrap()])
        .output()
        .expect("failed to run list");
    assert!(output.status.success(), "list subcommand should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("alpha-recipe"),
        "list should show alpha-recipe, got: {stdout}"
    );
    assert!(
        stdout.contains("beta-recipe"),
        "list should show beta-recipe, got: {stdout}"
    );
}

/// User runs a recipe with output chaining — step B uses step A's output.
#[test]
fn test_output_chaining_between_steps() {
    let tmp = TempDir::new().unwrap();
    let recipe = write_recipe(
        tmp.path(),
        "chain.yaml",
        r#"
name: output-chain
steps:
  - id: producer
    command: "echo hello-from-producer"
    output: produced_value
  - id: consumer
    command: "echo got-{{produced_value}}"
    output: consumed_value
"#,
    );
    let (code, json, _stderr) = run_json(&recipe, &[]);
    assert_eq!(code, 0);
    let consumer = find_step(&json, "consumer").expect("consumer not found");
    assert_eq!(consumer["status"].as_str().unwrap(), "completed");
    let output = consumer["output"].as_str().unwrap();
    assert!(
        output.contains("hello-from-producer"),
        "consumer should receive producer's output via template, got: {output}"
    );
}

/// User runs a recipe with hooks — pre_step and post_step execute around each step.
#[test]
fn test_hooks_pre_and_post_step() {
    let tmp = TempDir::new().unwrap();
    let pre_marker = tmp.path().join("pre-marker");
    let post_marker = tmp.path().join("post-marker");
    let recipe = write_recipe(
        tmp.path(),
        "hooked.yaml",
        &format!(
            r#"
name: hooked-recipe
hooks:
  pre_step: "touch {pre}"
  post_step: "touch {post}"
steps:
  - id: hook-target
    command: "echo hooked"
"#,
            pre = pre_marker.display(),
            post = post_marker.display(),
        ),
    );
    let (code, _stdout, _stderr) = run_binary(&recipe, &[]);
    assert_eq!(code, 0);
    assert!(
        pre_marker.exists(),
        "pre_step hook should have created marker file"
    );
    assert!(
        post_marker.exists(),
        "post_step hook should have created marker file"
    );
}
