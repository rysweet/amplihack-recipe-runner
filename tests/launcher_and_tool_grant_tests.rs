//! `--launcher`, `--no-allow-all-tools` and `--no-add-dir` change what the
//! runner hands to the agent launcher. Each test runs the built binary against
//! a launcher stand-in that prints its arguments; no agent is started.
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn stand_in(dir: &Path, name: &str, label: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\nprintf '{label}:%s ' \"$@\"\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn run(dir: &Path, env_launcher: Option<&Path>, args: &[&str]) -> Output {
    let recipe = dir.join("agent.yaml");
    std::fs::write(
        &recipe,
        "name: agent\nsteps:\n  - id: one\n    agent: none\n    prompt: hello\n",
    )
    .unwrap();
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
    if let Some(launcher) = env_launcher {
        cmd.env("AMPLIHACK_LAUNCHER_BINARY", launcher);
    }
    cmd.output().unwrap()
}

fn step_output(out: &Output) -> String {
    assert!(
        out.status.success(),
        "run failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let result: Value = serde_json::from_str(&stdout[stdout.find('{').unwrap()..]).unwrap();
    result["step_results"][0]["output"]
        .as_str()
        .unwrap_or_else(|| panic!("no step output in {result}"))
        .to_string()
}

#[test]
fn launcher_option_beats_the_environment_variable() {
    let dir = tempfile::tempdir().unwrap();
    let from_env = stand_in(dir.path(), "env-launcher", "ENV");
    let explicit = stand_in(dir.path(), "explicit-launcher", "EXPLICIT");

    let out = step_output(&run(dir.path(), Some(&from_env), &[]));
    assert!(out.starts_with("ENV:"), "{out}");

    let out = step_output(&run(
        dir.path(),
        Some(&from_env),
        &["--launcher", explicit.to_str().unwrap()],
    ));
    assert!(out.starts_with("EXPLICIT:"), "{out}");
}

#[test]
fn relative_launcher_path_exits_with_bad_args() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(dir.path(), None, &["--launcher", "bin/launcher"]);
    assert_eq!(out.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&out.stderr).contains("must be an absolute path"));
}

#[test]
fn relative_launcher_path_is_rejected_with_validate_only_too() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        dir.path(),
        None,
        &["--validate-only", "--launcher", "bin/launcher"],
    );
    assert_eq!(out.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&out.stderr).contains("must be an absolute path"));
}

#[test]
fn no_add_dir_help_mentions_the_overflow_prompt_directory() {
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .arg("--help")
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    let at = help.find("--no-add-dir").expect("--no-add-dir in help");
    assert!(
        help[at..].contains("overflow prompt file is still granted"),
        "{}",
        &help[at..]
    );
}

#[test]
fn no_add_dir_stops_the_working_directory_grant() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = stand_in(dir.path(), "launcher", "L");

    let granted = step_output(&run(dir.path(), Some(&launcher), &[]));
    assert!(granted.contains("L:--add-dir"), "{granted}");

    let without = step_output(&run(dir.path(), Some(&launcher), &["--no-add-dir"]));
    assert!(!without.contains("--add-dir"), "{without}");
}

#[test]
fn no_allow_all_tools_omits_the_copilot_flag() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = stand_in(dir.path(), "launcher", "L");

    let default = step_output(&run(
        dir.path(),
        Some(&launcher),
        &["--agent-binary", "copilot"],
    ));
    assert!(default.contains("L:--allow-all-tools"), "{default}");

    let without = step_output(&run(
        dir.path(),
        Some(&launcher),
        &["--agent-binary", "copilot", "--no-allow-all-tools"],
    ));
    assert!(!without.contains("--allow-all-tools"), "{without}");
}
