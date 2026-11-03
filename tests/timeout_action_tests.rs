//! `--timeout-action report` reports a step that passes its timeout and keeps
//! waiting; the default kills it. `--log-dir` keeps agent logs.
//!
//! Each test runs the built binary. Steps are short shell commands or a
//! launcher stand-in; no agent is started.
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn run(dir: &Path, recipe_text: &str, launcher: Option<&Path>, args: &[&str]) -> Output {
    let recipe = dir.join("recipe.yaml");
    std::fs::write(&recipe, recipe_text).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"));
    cmd.arg(&recipe)
        .args(["--output-format", "json", "-C"])
        .arg(dir)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TMPDIR", std::env::temp_dir())
        .env("HOME", dir)
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1");
    if let Some(launcher) = launcher {
        cmd.env("AMPLIHACK_LAUNCHER_BINARY", launcher);
    }
    cmd.output().unwrap()
}

fn result(out: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout[stdout.find('{').unwrap()..]).unwrap()
}

const SLOW_SHELL: &str =
    "name: slow\nsteps:\n  - id: one\n    command: \"sleep 2; echo finished\"\n    timeout: 1\n";

#[test]
fn default_kills_a_shell_step_past_its_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(dir.path(), SLOW_SHELL, None, &[]);
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("finished"));
}

#[test]
fn report_waits_for_a_shell_step_and_reports_once() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        dir.path(),
        SLOW_SHELL,
        None,
        &["--timeout-action", "report"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(result(&out)["step_results"][0]["output"], "finished");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr.matches("exceeded its 1s timeout").count(),
        1,
        "{stderr}"
    );
}

#[test]
fn log_dir_keeps_the_agent_logs() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = dir.path().join("launcher");
    std::fs::write(
        &launcher,
        "#!/bin/sh\necho agent-said-this\necho agent-err-said-this >&2\n",
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let logs = dir.path().join("logs");
    let recipe = "name: agent\nsteps:\n  - id: one\n    agent: none\n    prompt: hello\n";
    let out = run(
        dir.path(),
        recipe,
        Some(&launcher),
        &["--log-dir", logs.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut all = String::new();
    for entry in std::fs::read_dir(&logs).unwrap() {
        all.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    assert!(all.contains("agent-said-this"), "{all}");
    assert!(all.contains("agent-err-said-this"), "{all}");
}
