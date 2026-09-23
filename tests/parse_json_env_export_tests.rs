//! Issue rysweet/amplihack-rs#1468: a `parse_json` step output must reach a
//! later bash step's environment.
//!
//! These tests drive the COMPILED BINARY, as a user does. Before the fix the
//! runner exported a plain string output under its upper-cased name but left an
//! Object/Array output out of the environment entirely, so every
//! `auto-drive-to-merge` sub-recipe read an empty `${CRUSTY_LOOP_PREFLIGHT:-}`
//! and refused to run.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;

static BUILD_ONCE: Once = Once::new();

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

fn autodrive_fixture() -> PathBuf {
    project_root().join("tests/fixtures/autodrive")
}

fn write_recipe(dir: &Path, filename: &str, content: &str) -> PathBuf {
    let path = dir.join(filename);
    std::fs::write(&path, content).expect("failed to write recipe file");
    path
}

/// Run the binary with `--output-format json`, returning (exit code, parsed
/// stdout, stderr). `envs` are added to the child environment and `cwd` becomes
/// its working directory.
fn run_json_in(
    recipe_path: &Path,
    extra_args: &[&str],
    cwd: &Path,
    envs: &[(&str, &str)],
) -> (i32, Value, String) {
    ensure_built();
    let mut cmd = Command::new(binary_path());
    cmd.arg(recipe_path)
        .args(["--output-format", "json"])
        .args(extra_args)
        .current_dir(cwd);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("failed to execute binary");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let json: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("failed to parse JSON output: {e}\nstdout: {stdout}\nstderr: {stderr}");
    });
    (code, json, stderr)
}

fn find_step<'a>(json: &'a Value, step_id: &str) -> Option<&'a Value> {
    json["step_results"]
        .as_array()
        .and_then(|arr| arr.iter().find(|s| s["step_id"].as_str() == Some(step_id)))
}

fn step_output<'a>(json: &'a Value, step_id: &str) -> &'a str {
    find_step(json, step_id)
        .unwrap_or_else(|| panic!("step '{step_id}' missing from results: {json}"))["output"]
        .as_str()
        .unwrap_or_else(|| panic!("step '{step_id}' has no string output: {json}"))
}

// ---------------------------------------------------------------------------
// The reproduction from the issue, verbatim.
// ---------------------------------------------------------------------------

/// The recipe in rysweet/amplihack-rs#1468, unchanged. The issue reports:
///
/// ```text
/// ENV MY_PREFLIGHT=[<unset>]
/// TEMPLATE=[{"a":"x","state_dir":"/tmp/sd"}]
/// ENV PLAIN_OUT=[plain string output]
/// ```
///
/// All three lines must show the value.
#[test]
fn test_issue_1468_parse_json_output_reaches_a_later_bash_step_env() {
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "envtest.yaml",
        r#"
name: envtest
description: does a parse_json output reach a later bash step's environment
version: "1"
steps:
  - id: one
    type: bash
    parse_json: true
    command: |
      printf '{"a":"x","state_dir":"/tmp/sd"}\n'
    output: my_preflight
  - id: two
    type: bash
    command: |
      echo "ENV MY_PREFLIGHT=[${MY_PREFLIGHT:-<unset>}]"
      echo "TEMPLATE=[{{my_preflight}}]"
  - id: three
    type: bash
    command: |
      printf 'plain string output\n'
    output: plain_out
  - id: four
    type: bash
    command: |
      echo "ENV PLAIN_OUT=[${PLAIN_OUT:-<unset>}]"
"#,
    );

    let (code, json, stderr) = run_json_in(&recipe, &[], dir.path(), &[]);
    assert_eq!(code, 0, "recipe should succeed; stderr: {stderr}");

    let two = step_output(&json, "two");
    assert!(
        two.contains(r#"ENV MY_PREFLIGHT=[{"a":"x","state_dir":"/tmp/sd"}]"#),
        "a parse_json output must be exported to a later step as its compact \
         JSON, under the same upper-cased name a string output gets. Got:\n{two}"
    );

    // The template path already worked; it must keep working, and must agree
    // with the environment byte for byte.
    assert!(
        two.contains(r#"TEMPLATE=[{"a":"x","state_dir":"/tmp/sd"}]"#),
        "template substitution must still render the same JSON. Got:\n{two}"
    );

    // A plain string output was never broken; guard against regressing it.
    let four = step_output(&json, "four");
    assert!(
        four.contains("ENV PLAIN_OUT=[plain string output]"),
        "a string output must still be exported. Got:\n{four}"
    );
}

/// A top-level JSON *array* output, which the issue's reproduction does not
/// cover but the runner produces from the same code path.
#[test]
fn test_issue_1468_json_array_output_reaches_a_later_bash_step_env() {
    let dir = TempDir::new().unwrap();
    let recipe = write_recipe(
        dir.path(),
        "arraytest.yaml",
        r#"
name: arraytest
version: "1"
steps:
  - id: emit
    type: bash
    parse_json: true
    command: |
      printf '[{"state_dir":"/tmp/sd"},{"n":2}]\n'
    output: rounds
  - id: consume
    type: bash
    command: |
      echo "ENV ROUNDS=[${ROUNDS:-<unset>}]"
"#,
    );

    let (code, json, stderr) = run_json_in(&recipe, &[], dir.path(), &[]);
    assert_eq!(code, 0, "recipe should succeed; stderr: {stderr}");

    let consume = step_output(&json, "consume");
    assert!(
        consume.contains(r#"ENV ROUNDS=[[{"state_dir":"/tmp/sd"},{"n":2}]]"#),
        "an array output must be exported as its compact JSON. Got:\n{consume}"
    );
}

// ---------------------------------------------------------------------------
// The real auto-drive preflight, through the step that consumes it.
// ---------------------------------------------------------------------------

/// Runs `step-01-crusty-loop-preflight` from `autodrive-crusty-loop.yaml`
/// (copied verbatim into `tests/fixtures/autodrive/`) and feeds it to the
/// consuming step's own `${CRUSTY_LOOP_PREFLIGHT:-}` read, extraction pipeline
/// and guard.
///
/// This is the gap the issue names: the recipes' own guard tests did not catch
/// that the skill could not run at all. Before the fix this fails exactly as
/// reported in the field — "ERROR: preflight produced no state_dir; refusing to
/// run an unrecorded loop."
#[test]
fn test_issue_1468_autodrive_crusty_loop_preflight_state_dir_resolves() {
    let fixture = autodrive_fixture();
    let recipe = fixture.join("crusty-loop-preflight.yaml");

    // Somewhere that is not a git repository and has no PR, so the preflight
    // takes its offline path: no `gh`, no network, no ambient state.
    let work = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo_path = work.path().to_string_lossy().to_string();
    let state_dir = state.path().to_string_lossy().to_string();

    // The fixture's `amplihack` shim goes first on PATH, unconditionally, so a
    // machine with the real binary installed and a CI runner without it take
    // the same path. See tests/fixtures/autodrive/PROVENANCE.md.
    let path = format!(
        "{}:{}",
        fixture.join("bin").to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );

    let (code, json, stderr) = run_json_in(
        &recipe,
        &["-c", &format!("repo_path={repo_path}"), "-c", "pr_number="],
        work.path(),
        &[
            ("PATH", path.as_str()),
            ("AMPLIHACK_HOME", &fixture.to_string_lossy()),
            ("AMPLIHACK_STATE_DIR", &state_dir),
        ],
    );

    assert!(
        !stderr.contains("refusing to run an unrecorded loop"),
        "the consuming step hit the #1468 failure: the preflight's parse_json \
         output never reached its environment.\nstderr:\n{stderr}"
    );
    assert_eq!(code, 0, "recipe should succeed; stderr: {stderr}");

    let consume = step_output(&json, "step-02-crusty-loop");
    let resolved = consume
        .lines()
        .find_map(|l| l.strip_prefix("RESOLVED_STATE_DIR=["))
        .and_then(|l| l.strip_suffix(']'))
        .unwrap_or_else(|| panic!("consuming step printed no state dir:\n{consume}"));

    assert!(
        !resolved.is_empty(),
        "state_dir must resolve in the consuming step, not come back empty"
    );
    assert!(
        resolved.starts_with(&state_dir),
        "resolved state_dir {resolved:?} should sit under the state root \
         {state_dir:?} the preflight was given"
    );
    assert!(
        Path::new(resolved).is_dir(),
        "the preflight should have created {resolved:?}"
    );
}
