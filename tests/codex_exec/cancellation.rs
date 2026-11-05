use super::fixtures::{fixture_environment, install_launcher};
#[cfg(target_os = "linux")]
use super::process_observation::{CancellationObservation, SpawnedWorker};
use serde_json::Value;
use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};

fn runner_command(root: &std::path::Path, launcher: &std::path::Path, scenario: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"));
    fixture_environment(&mut command, root)
        .env("TEST_SCENARIO", scenario)
        .env("AMPLIHACK_LAUNCHER_BINARY", launcher)
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1");
    command
}

/// Exercise the same command constructor on both platforms without sending a
/// signal. The Linux signal scenarios below retain their original coverage.
pub(super) fn assert_runner_environment_is_private() {
    let root = tempfile::tempdir().unwrap();
    let launcher = install_launcher(root.path());
    let recipe = root.path().join("environment.yaml");
    fs::write(&recipe, "name: environment\nsteps:\n  - id: agent\n    type: agent\n    prompt: synthetic environment probe\n").unwrap();
    let mut runner = runner_command(root.path(), &launcher, "success")
        .arg(recipe)
        .args(["--agent-binary", "codex", "--working-dir"])
        .arg(root.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = runner.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            runner.kill().unwrap();
            runner.wait().unwrap();
            panic!("environment probe exceeded watchdog");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "synthetic runner environment probe failed"
    );
    let record: Value =
        serde_json::from_slice(&fs::read(root.path().join("record.json")).unwrap()).unwrap();
    assert_eq!(record["ambient_present"], serde_json::json!([]));
}

#[cfg(target_os = "linux")]
fn assert_runner_cancellation(signal: i32, variant: &str) {
    let root = tempfile::tempdir().unwrap();
    let launcher = install_launcher(root.path());
    let recipe = root.path().join("recipe.yaml");
    let agent = "  - id: agent\n    type: agent\n    prompt: safe fixture\n";
    let marker = "  - id: after\n    type: bash\n    command: touch continued\n";
    let steps = match variant {
        "json" => format!("{agent}    parse_json: true\n    continue_on_error: true\n{marker}"),
        "nonfatal" => format!("{agent}    fatal: false\n{marker}"),
        "continue" => format!("{agent}    continue_on_error: true\n{marker}"),
        "parallel" => format!(
            "{agent}    continue_on_error: true\n    parallel_group: group\n{marker}    parallel_group: group\n  - id: later-agent\n    type: agent\n    prompt: must never run\n"
        ),
        "nested" | "nested_recovery" | "recovery_cancel" => {
            fs::write(
                root.path().join("child.yaml"),
                if variant == "recovery_cancel" {
                    "name: child\nsteps:\n  - id: fail\n    type: bash\n    command: exit 1\n"
                        .to_string()
                } else {
                    format!("name: child\nsteps:\n{agent}    fatal: false\n{marker}")
                },
            )
            .unwrap();
            let recovery = if matches!(variant, "nested_recovery" | "recovery_cancel") {
                "    recovery_on_failure: true\n"
            } else {
                ""
            };
            format!(
                "  - id: child\n    type: recipe\n    recipe: child\n    continue_on_error: true\n{recovery}{marker}"
            )
        }
        _ => agent.to_string(),
    };
    fs::write(&recipe, format!("name: cancellation\nsteps:\n{steps}")).unwrap();
    let mut runner = runner_command(
        root.path(),
        &launcher,
        if variant == "json" {
            "cancel_json"
        } else {
            "cancel"
        },
    )
    .arg(&recipe)
    .args(["--agent-binary", "codex", "--working-dir"])
    .arg(root.path())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
    let worker = SpawnedWorker::capture(runner.id()).expect("spawned cancellation runner identity");
    let start = Instant::now();
    while !root.path().join("launcher.pid").exists() {
        if start.elapsed() > Duration::from_secs(5) {
            runner.kill().unwrap();
            let output = runner.wait_with_output().unwrap();
            panic!(
                "fixture failed to start: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let processes =
        CancellationObservation::capture(root.path(), &worker, start + Duration::from_secs(5))
            .expect("known live cancellation processes must be observed");
    let launcher = processes.launcher_pid();
    assert_eq!(unsafe { libc::getpgid(launcher) }, launcher);
    assert_ne!(unsafe { libc::getpgid(runner.id() as i32) }, launcher);
    assert_eq!(unsafe { libc::kill(runner.id() as i32, signal) }, 0);
    let deadline = Instant::now() + Duration::from_secs(6);
    while runner.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            processes
                .cleanup()
                .expect("verified cancellation fallback cleanup failed");
            runner.kill().unwrap();
            runner.wait().unwrap();
            panic!("cancellation exceeded cleanup bounds");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    processes.assert_stopped();
    let output = runner.wait_with_output().unwrap();
    assert!(
        !output.status.success(),
        "{variant}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !root.path().join("continued").exists(),
        "{variant} continued"
    );
    assert!(
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .contains("cancelled by signal"),
        "{variant}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: Value =
        serde_json::from_slice(&fs::read(root.path().join("record.json")).unwrap()).unwrap();
    assert!(
        !std::path::Path::new(record["final"].as_str().unwrap())
            .parent()
            .unwrap()
            .exists()
    );
    assert_eq!(
        fs::read_to_string(root.path().join("attempts.jsonl"))
            .unwrap()
            .lines()
            .count(),
        if variant == "json" { 2 } else { 1 }
    );
}

#[test]
#[cfg(target_os = "linux")]
fn runner_sigint_cleans_owned_group_and_resources() {
    for variant in [
        "fatal",
        "continue",
        "nonfatal",
        "nested",
        "nested_recovery",
        "recovery_cancel",
        "parallel",
        "json",
    ] {
        assert_runner_cancellation(libc::SIGINT, variant);
    }
}
#[test]
#[cfg(target_os = "linux")]
fn runner_sigterm_cleans_owned_group_and_resources() {
    for variant in [
        "fatal",
        "continue",
        "nonfatal",
        "nested",
        "nested_recovery",
        "recovery_cancel",
        "parallel",
        "json",
    ] {
        assert_runner_cancellation(libc::SIGTERM, variant);
    }
}
