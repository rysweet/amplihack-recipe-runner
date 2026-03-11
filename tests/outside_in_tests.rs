//! Outside-in end-to-end tests for recipe-runner-rs.
//!
//! These tests exercise the COMPILED BINARY via `std::process::Command`,
//! exactly as a user would invoke it from the command line.
//! They validate fixes for issues #2954 and #2792.

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
// Sub-recipe failure propagation
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
