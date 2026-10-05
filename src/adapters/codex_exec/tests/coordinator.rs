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
