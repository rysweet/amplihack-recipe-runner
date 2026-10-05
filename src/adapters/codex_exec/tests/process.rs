use super::super::process::{reap_launcher, shutdown};
use std::{
    cell::RefCell,
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn launcher_kill_failure_has_bounded_reaping() {
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let started = Instant::now();
    let result = reap_launcher(
        &mut child,
        |_| Err(std::io::Error::from_raw_os_error(libc::EPERM)),
        Duration::from_millis(30),
    );
    let elapsed = started.elapsed();
    // Always clean up the fixture before asserting the injected failure.
    let still_live = child.try_wait().unwrap().is_none();
    child.kill().unwrap();
    child.wait().unwrap();
    let error = result.unwrap_err().to_string();
    assert!(still_live);
    assert!(elapsed < Duration::from_secs(1));
    assert!(error.contains("Failed to kill Codex launcher"));
    assert!(error.contains("Timed out reaping Codex launcher"));
}

#[test]
fn already_exited_launcher_does_not_require_kill() {
    let mut child = Command::new("true").spawn().unwrap();
    child.wait().unwrap();
    reap_launcher(
        &mut child,
        |_| panic!("must not kill reaped child"),
        Duration::ZERO,
    )
    .unwrap();
}

#[test]
fn signal_and_observation_errors_do_not_skip_cleanup() {
    let operations = RefCell::new(Vec::new());
    let mut failures = vec!["original execution failure".into()];
    shutdown(
        |signal| {
            operations.borrow_mut().push(signal);
            anyhow::bail!("injected signal failure")
        },
        || {
            operations.borrow_mut().push(0);
            anyhow::bail!("injected observation failure")
        },
        || {
            operations.borrow_mut().push(-1);
            anyhow::bail!("injected reap failure")
        },
        &mut failures,
    );
    assert_eq!(
        *operations.borrow(),
        [libc::SIGTERM, 0, libc::SIGKILL, 0, -1, 0]
    );
    assert_eq!(failures.len(), 7);
    assert!(failures.join("; ").contains("original execution failure"));
}
