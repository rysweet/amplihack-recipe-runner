use super::super::*;
use super::super::{
    diagnostics::drain_diagnostics, final_output::read_final_output, process::group_live,
};

#[test]
fn delayed_stderr_reader_preserves_terminal_rate_limit() {
    let resources = tempfile::tempdir().unwrap();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "cat >/dev/null; echo 'rate limit' >&2; exit 1"]);
    let error = execute_with_readers(
        command,
        resources.path(),
        resources.path(),
        &[("PATH".into(), "/usr/bin:/bin".into())],
        "task",
        Some(5),
        |stdout, stderr, stop| {
            let stdout = drain_diagnostics(stdout, stop.clone());
            let stderr = std::thread::spawn(move || {
                // Force the scheduling order that previously lost queued bytes.
                while !stop.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                drain_diagnostics(stderr, stop).join().unwrap()
            });
            (stdout, stderr)
        },
    )
    .unwrap_err();
    let failure = error.downcast_ref::<ExitFailure>().unwrap();
    assert!(
        failure.rate_limited,
        "terminal diagnostic must enable retries"
    );
    assert!(failure.classification.contains("rate limit"));
}

#[test]
fn reader_failure_triggers_cleanup_without_execution_timeout() {
    let resources = tempfile::tempdir().unwrap();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "echo $$ > launcher.pid; exec sleep 30"]);
    let started = Instant::now();
    let error = execute_with_readers(
        command,
        resources.path(),
        resources.path(),
        &[],
        "task",
        None,
        |stdout, stderr, stop| {
            let failed = stop.clone();
            let reader = std::thread::spawn(move || {
                drop(stdout);
                failed.store(true, std::sync::atomic::Ordering::Release);
                Err(std::io::Error::other("injected reader error"))
            });
            let stderr = drain_diagnostics(stderr, stop);
            (reader, stderr)
        },
    )
    .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(format!("{error:#}").contains("diagnostic reader failed"));
    if let Ok(pid) = std::fs::read_to_string(resources.path().join("launcher.pid")) {
        assert!(!group_live(pid.trim().parse().unwrap()).unwrap());
    }
}

#[test]
fn deletion_failure_preserves_execution_failure() {
    let error = finish_resources(
        Err(anyhow::anyhow!("execution failure")),
        Err(anyhow::anyhow!("resource deletion failure")),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "execution failure; cleanup: resource deletion failure"
    );
    assert!(finish_resources(Ok("output".into()), Err(anyhow::anyhow!("delete"))).is_err());
}

#[test]
fn spawn_and_final_read_failures_are_contextual() {
    let resources = tempfile::tempdir().unwrap();
    let error = execute(
        Command::new("/nonexistent/codex-launcher"),
        resources.path(),
        resources.path(),
        &[],
        "task",
        None,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("Failed to spawn"));
    assert!(read_final_output(&resources.path().join("missing")).is_err());
}

#[test]
fn nonzero_exit_and_reader_failure_preserve_status_and_safe_category() {
    let resources = tempfile::tempdir().unwrap();
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        "cat >/dev/null; echo false-success > final-message; echo 'rate limit SECRET' >&2; exit 7",
    ]);
    let error = execute_with_readers(
        command,
        resources.path(),
        resources.path(),
        &[],
        "task",
        Some(5),
        |stdout, stderr, stop| {
            let reader_stop = stop.clone();
            let stdout = std::thread::spawn(move || {
                while !reader_stop.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                drop(stdout);
                Err(std::io::Error::other("injected teardown error"))
            });
            (stdout, drain_diagnostics(stderr, stop))
        },
    )
    .unwrap_err();
    let failure = error.downcast_ref::<ExitFailure>().unwrap();
    assert_eq!(failure.status.code(), Some(7));
    assert!(failure.rate_limited);
    assert!(error.downcast_ref::<CleanupFailure>().is_some());
    assert!(!retryable(&error));
    let detail = format!("{error:#}");
    assert!(detail.contains("rate limit"));
    assert!(detail.contains("Failed to drain Codex stdout diagnostics"));
    assert!(detail.contains("injected teardown error"));
    assert!(!detail.contains("SECRET"));
    let error =
        combine_result::<()>(Err(error), Err(Interruption { signal: 15 }.into())).unwrap_err();
    assert!(error.downcast_ref::<Interruption>().is_some());
    assert!(error.downcast_ref::<CleanupFailure>().is_some());
    assert!(!retryable(&error));
    assert!(format!("{error:#}").contains("exit status: 7"));
}

#[test]
fn combined_cancellation_blocks_continuation_json_repair_and_recovery() {
    use crate::{adapters::Adapter, parser::RecipeParser, runner::RecipeRunner};
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    struct Cancelled(Arc<AtomicUsize>);
    impl Adapter for Cancelled {
        fn execute_agent_step(
            &self,
            _: &str,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&str>,
            _: &str,
            _: Option<&str>,
            _: Option<u64>,
        ) -> anyhow::Result<String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let status = Command::new("/bin/sh")
                .args(["-c", "exit 7"])
                .status()
                .unwrap();
            let result = finish_resources(
                Err(ExitFailure {
                    status,
                    rate_limited: true,
                    classification: "service rate limit",
                }
                .into()),
                Err(anyhow::anyhow!("reader teardown failed")),
            );
            let result = finish_resources(result, Err(Interruption { signal: 15 }.into()));
            finish_resources(result, Err(anyhow::anyhow!("resource deletion failed")))
        }
        fn execute_bash_step(
            &self,
            _: &str,
            _: &str,
            _: Option<u64>,
            _: &HashMap<String, String>,
        ) -> anyhow::Result<String> {
            panic!("cancellation must prevent subsequent Bash execution")
        }
        fn is_available(&self) -> bool {
            true
        }
        fn name(&self) -> &str {
            "codex"
        }
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("child.yaml"), "name: child\nsteps:\n  - id: cancel\n    prompt: task\n    continue_on_error: true\n    parse_json: true\n    auto_stage: false\n").unwrap();
    for step in [
        "    prompt: task\n    parse_json: true\n    continue_on_error: true\n",
        "    type: recipe\n    recipe: child\n    recovery_on_failure: true\n    continue_on_error: true\n",
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let recipe = RecipeParser::new().parse(&format!("name: cancellation\nsteps:\n  - id: cancel\n{step}    auto_stage: false\n  - id: after\n    type: bash\n    command: forbidden\n")).unwrap();
        let result = RecipeRunner::new(Cancelled(calls.clone()))
            .with_working_dir(root.path().to_str().unwrap())
            .with_auto_stage(false)
            .execute(&recipe, None);
        assert!(!result.success);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "must not repair JSON or recover cancellation"
        );
        let diagnostic = format!("{result:?}");
        for detail in [
            "cancelled by signal",
            "reader teardown failed",
            "resource deletion failed",
            "exit status: 7",
        ] {
            assert!(diagnostic.contains(detail), "lost {detail}: {diagnostic}");
        }
    }
}
