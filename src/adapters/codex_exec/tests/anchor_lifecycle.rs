//! New helper startup and authority-loss contracts via real production attempts.
use super::super::{CleanupFailure, finish_resources, process::OwnedProcess};
use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};

pub(super) fn worker(case: &str) {
    let root = tempfile::tempdir().unwrap();
    let mode = match case {
        "anchor_fork_failure" => 5,
        "anchor_join_failure" => 6,
        "anchor_setup_exit" => 7,
        "anchor_unexpected_exit" => 0,
        _ => panic!("unknown anchor case"),
    };
    super::retirement_fixtures::arm(0, 0, 0, 0, mode);
    let mut command = Command::new("/bin/sleep");
    command.arg("30");
    let started = Instant::now();
    let spawned = OwnedProcess::spawn(command, root.path(), root.path(), &[]);
    if mode != 0 {
        let error = match spawned {
            Err(error) => error,
            Ok(mut process) => {
                process.shutdown(&mut Vec::new());
                panic!("anchor establishment failure did not refuse the attempt");
            }
        };
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "startup unwind exceeded shared bounds"
        );
        assert!(
            error.downcast_ref::<CleanupFailure>().is_some(),
            "post-spawn establishment fault lost cleanup classification: {error:#}"
        );
        assert!(format!("{error:#}").to_lowercase().contains("anchor"));
        let events = fs::read_to_string(std::env::var("LIFECYCLE_EVENTS").unwrap()).unwrap();
        assert!(
            events.contains("anchor_fault"),
            "the intended real syscall gate was not reached"
        );
    } else {
        let mut process = spawned.unwrap();
        let group = process.child.id() as i32;
        let helpers: Vec<_> = super::inspection::members(group)
            .unwrap()
            .into_iter()
            .filter(|pid| *pid != group)
            .collect();
        // Collect state and clean the launcher even on a RED assertion.
        for pid in &helpers {
            assert_eq!(unsafe { libc::kill(*pid, libc::SIGKILL) }, 0);
        }
        let mut failures = Vec::new();
        process.shutdown(&mut failures);
        assert_eq!(
            helpers.len(),
            1,
            "no positively owned original-group helper established"
        );
        let cleanup = if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(failures.join("; ")))
        };
        let error = finish_resources(Err(anyhow::anyhow!("primary ordinary failure")), cleanup)
            .unwrap_err();
        assert!(
            error.downcast_ref::<CleanupFailure>().is_some(),
            "unexpected helper death disappeared after absence"
        );
        assert!(format!("{error:#}").contains("primary ordinary failure"));
        assert!(
            super::inspection::members(group).unwrap().is_empty(),
            "owned member not reaped at return"
        );
    }
}
#[test]
fn anchor_fork_failure_is_bounded_and_terminal() {
    super::retirement_fixtures::run("anchor_fork_failure");
}
#[test]
fn anchor_join_failure_is_bounded_and_terminal() {
    super::retirement_fixtures::run("anchor_join_failure");
}
#[test]
fn anchor_setup_exit_unwinds_owned_launcher() {
    super::retirement_fixtures::run("anchor_setup_exit");
}
#[test]
fn unexpected_anchor_exit_retains_typed_failure() {
    super::retirement_fixtures::run("anchor_unexpected_exit");
}
