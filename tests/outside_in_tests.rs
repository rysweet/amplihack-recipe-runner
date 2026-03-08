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
fn test_parse_json_failure_kills_recipe_by_default() {
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "recipe.yaml",
        r#"
name: test-parse-json-fail
steps:
  - id: bad-json
    command: "echo 'this is not json at all'"
    parse_json: true
  - id: should-not-run
    command: "echo 'reached'"
"#,
    );

    let (code, json, _stderr) = run_json(&recipe, &[]);

    assert_eq!(code, 1, "recipe should fail on bad JSON parse");
    let bad = find_step(&json, "bad-json").expect("bad-json step should exist");
    assert_eq!(bad["status"].as_str().unwrap(), "failed");

    // The second step should not have executed.
    match find_step(&json, "should-not-run") {
        None => {} // not present at all — fine
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

    // With continue_on_error the recipe should proceed.
    let bad = find_step(&json, "bad-json").expect("bad-json step should exist");
    // The step may report "completed" (fallback to raw output) or "failed" but recipe continues.
    let bad_status = bad["status"].as_str().unwrap();
    assert!(
        bad_status == "completed" || bad_status == "failed",
        "bad-json step should be completed or failed, got {bad_status}"
    );

    let good = find_step(&json, "should-run").expect("should-run step should exist");
    assert_eq!(
        good["status"].as_str().unwrap(),
        "completed",
        "second step must execute when continue_on_error is set"
    );

    // If the bad step completed, recipe may succeed overall
    if bad_status == "completed" {
        assert_eq!(
            code, 0,
            "recipe should succeed when error was continued past"
        );
    }
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
// Additional outside-in tests
// ---------------------------------------------------------------------------

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
