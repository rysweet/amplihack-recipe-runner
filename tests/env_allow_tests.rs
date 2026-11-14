//! `--env-allow NAME` passes only the named parent variables to shell and agent
//! children.
//!
//! Each test runs the built binary with two marker variables in its
//! environment, first without the option (both reach the child, as before) and
//! then with it (only the named one does). Agent steps go to a launcher script
//! that records its environment; no agent is started.
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const NAMED: &str = "ENV_ALLOW_TEST_NAMED";
const UNNAMED: &str = "ENV_ALLOW_TEST_UNNAMED";

fn run(work: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .args(args)
        .current_dir(work)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TMPDIR", std::env::temp_dir())
        .env("HOME", work)
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
        .env(NAMED, "visible")
        .env(UNNAMED, "hidden")
        .output()
        .unwrap()
}

fn step_output(out: &Output) -> String {
    assert!(
        out.status.success(),
        "run failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout.find('{').expect("JSON result on stdout");
    let result: Value = serde_json::from_str(&stdout[json_start..]).unwrap();
    result["step_results"][0]["output"]
        .as_str()
        .unwrap_or_else(|| panic!("no step output in {result}"))
        .to_string()
}

#[test]
fn shell_step_receives_only_the_named_variable() {
    let dir = tempfile::tempdir().unwrap();
    let recipe = dir.path().join("probe.yaml");
    std::fs::write(
        &recipe,
        format!(
            "name: probe\nsteps:\n  - id: one\n    command: \"printf '%s|%s' \\\"${{{NAMED}-unset}}\\\" \\\"${{{UNNAMED}-unset}}\\\"\"\n"
        ),
    )
    .unwrap();
    let recipe = recipe.to_str().unwrap();

    let default = run(dir.path(), &[recipe, "--output-format", "json"]);
    assert_eq!(step_output(&default), "visible|hidden");

    let restricted = run(
        dir.path(),
        &[
            recipe,
            "--output-format",
            "json",
            "--env-allow",
            "PATH",
            "--env-allow",
            NAMED,
        ],
    );
    assert_eq!(step_output(&restricted), "visible|unset");
}

/// Run an agent recipe through `launcher` with both marker variables in the
/// environment, and return the step output.
fn agent_run(dir: &Path, launcher: &Path, extra: &[&str]) -> String {
    let recipe = dir.join("agent.yaml");
    std::fs::write(
        &recipe,
        "name: agent\nsteps:\n  - id: one\n    agent: none\n    prompt: hello\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .arg(&recipe)
        .args(["--output-format", "json"])
        .args(extra)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TMPDIR", std::env::temp_dir())
        .env("HOME", dir)
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
        .env("AMPLIHACK_LAUNCHER_BINARY", launcher)
        .env(NAMED, "visible")
        .env(UNNAMED, "hidden")
        .output()
        .unwrap();
    step_output(&out)
}

fn write_executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn agent_step_receives_only_the_named_variable() {
    let dir = tempfile::tempdir().unwrap();
    // `amplihack <cli> ...` is the agent command line; this stand-in prints
    // the two marker variables it was given.
    let launcher = dir.path().join("amplihack");
    write_executable(
        &launcher,
        &format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s|%s' \"${{{NAMED}-unset}}\" \"${{{UNNAMED}-unset}}\"\n"
        ),
    );

    // Default: every parent variable reaches the child, as before.
    let default = agent_run(dir.path(), &launcher, &[]);
    assert!(default.contains("visible|hidden"), "{default}");

    let restricted = agent_run(
        dir.path(),
        &launcher,
        &["--env-allow", "PATH", "--env-allow", NAMED],
    );
    assert!(restricted.contains("visible|unset"), "{restricted}");
}

#[test]
fn codex_agent_step_receives_only_the_named_variable() {
    let dir = tempfile::tempdir().unwrap();
    // The Codex path hands the prompt on stdin and reads the final message
    // from the file named by --output-last-message.
    let launcher = dir.path().join("amplihack");
    write_executable(
        &launcher,
        &format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do\n if [ \"$1\" = --output-last-message ]; then shift; final=$1; fi\n shift\ndone\ncat >/dev/null\nprintf '%s|%s' \"${{{NAMED}-unset}}\" \"${{{UNNAMED}-unset}}\" > \"$final\"\n"
        ),
    );

    let default = agent_run(dir.path(), &launcher, &["--agent-binary", "codex"]);
    assert_eq!(default, "visible|hidden");

    let restricted = agent_run(
        dir.path(),
        &launcher,
        &[
            "--agent-binary",
            "codex",
            "--env-allow",
            "PATH",
            "--env-allow",
            NAMED,
        ],
    );
    assert_eq!(restricted, "visible|unset");
}
