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

#[test]
fn agent_step_receives_only_the_named_variable() {
    let dir = tempfile::tempdir().unwrap();
    // `amplihack <cli> ...` is the agent command line; this stand-in records
    // the two marker variables it was given.
    let launcher = dir.path().join("amplihack");
    std::fs::write(
        &launcher,
        format!("#!/bin/sh\nprintf '%s|%s' \"${{{NAMED}-unset}}\" \"${{{UNNAMED}-unset}}\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let recipe = dir.path().join("agent.yaml");
    std::fs::write(
        &recipe,
        "name: agent\nsteps:\n  - id: one\n    agent: none\n    prompt: hello\n",
    )
    .unwrap();
    let recipe = recipe.to_str().unwrap();
    let launcher = launcher.to_str().unwrap();

    let launch_env = |extra: &[&str]| {
        let mut args = vec![recipe, "--output-format", "json", "--env-allow", "PATH"];
        args.extend_from_slice(extra);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"));
        cmd.args(&args)
            .current_dir(dir.path())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TMPDIR", std::env::temp_dir())
            .env("HOME", dir.path())
            .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
            .env("AMPLIHACK_LAUNCHER_BINARY", launcher)
            .env(NAMED, "visible")
            .env(UNNAMED, "hidden");
        cmd.output().unwrap()
    };

    let restricted = launch_env(&["--env-allow", NAMED]);
    assert!(step_output(&restricted).contains("visible|unset"));
}
