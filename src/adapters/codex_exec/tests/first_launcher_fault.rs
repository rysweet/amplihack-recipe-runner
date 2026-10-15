//! The FIRST launcher fork failure remains an ordinary error, in both epochs.
use super::super::{CleanupFailure, Interruption, process::OwnedProcess};
use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};
#[test]
fn first_launcher_eagain_preserves_ordinary_spawn_failure() {
    super::retirement_fixtures::run_case(
        "launcher_eagain",
        "adapters::codex_exec::tests::first_launcher_fault::launcher_fault_worker",
    );
}
#[test]
fn launcher_fault_worker() {
    if std::env::var("LIFECYCLE_CASE").as_deref() != Ok("launcher_eagain") {
        return;
    }
    super::retirement_fixtures::arm(0, 0, 0, 0, 12);
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "echo invoked > invoked; exec /bin/sleep 30"]);
    let started = Instant::now();
    let error = match OwnedProcess::spawn(command, root.path(), root.path(), &[]) {
        Err(error) => error,
        Ok(mut child) => {
            child.shutdown(&mut Vec::new());
            panic!("first launcher fork fault was not exercised");
        }
    };
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(error.downcast_ref::<CleanupFailure>().is_none());
    assert!(error.downcast_ref::<Interruption>().is_none());
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.contains("Failed to spawn Codex exec launcher"));
    assert!(diagnostic.contains("Resource temporarily unavailable"));
    assert!(!root.path().join("invoked").exists());
    let events = fs::read_to_string(std::env::var("LIFECYCLE_EVENTS").unwrap()).unwrap();
    assert_eq!(events.matches("launcher_fault_fork").count(), 1);
    assert!(!events.contains("anchor_fault"));
}
