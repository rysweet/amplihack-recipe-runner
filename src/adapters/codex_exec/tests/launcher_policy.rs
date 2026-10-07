//! Direct launcher TERM errors survive eventual exit, with one shared deadline.
use super::super::{CleanupFailure, finish_resources, launcher_cleanup::retire_with};
use std::{
    process::Command,
    time::{Duration, Instant},
};
#[test]
fn direct_term_failure_survives_successful_retirement() {
    let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let result = retire_with(
        &mut child,
        |child| {
            assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
            Err(std::io::Error::from_raw_os_error(libc::EPERM))
        },
        |child| child.kill(),
        Instant::now() + Duration::from_secs(1),
    );
    let error =
        finish_resources(Err(anyhow::anyhow!("primary ordinary error")), result).unwrap_err();
    assert!(child.try_wait().unwrap().is_some());
    assert!(error.downcast_ref::<CleanupFailure>().is_some());
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("primary ordinary error") && diagnostic.contains("TERM Codex launcher")
    );
    assert!(diagnostic.contains("Operation not permitted"));
}
#[test]
fn exhausted_direct_retirement_budget_does_not_renew_wait() {
    let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let started = Instant::now();
    let result = retire_with(
        &mut child,
        |_| Err(std::io::Error::from_raw_os_error(libc::EPERM)),
        |_| Err(std::io::Error::from_raw_os_error(libc::EPERM)),
        started,
    );
    let still_live = child.try_wait().unwrap().is_none();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(still_live);
    assert!(started.elapsed() < Duration::from_secs(1));
    let diagnostic = format!("{:#}", result.unwrap_err());
    for text in [
        "TERM Codex launcher",
        "kill Codex launcher",
        "Timed out reaping",
    ] {
        assert!(diagnostic.contains(text));
    }
}

#[test]
fn zero_and_overflowing_budgets_refuse_before_launcher_entry() {
    for timeout in [0, u64::MAX] {
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo invoked > invoked; exec /bin/sleep 30"]);
        let started = Instant::now();
        let error = match super::super::process::OwnedProcess::spawn_with_timeout(
            command,
            root.path(),
            root.path(),
            &[],
            Some(timeout),
        ) {
            Err(error) => error,
            Ok(mut process) => {
                process.shutdown(&mut Vec::new());
                panic!("invalid budget admitted launcher");
            }
        };
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!root.path().join("invoked").exists());
        assert!(format!("{error:#}").contains(if timeout == 0 {
            "timed out"
        } else {
            "overflow"
        }));
    }
}
