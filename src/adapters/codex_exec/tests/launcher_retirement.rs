//! Conditional Apple-source semantics exercised against actual Linux-owned groups.
use super::super::{
    CleanupFailure, finish_resources,
    process::{OwnedProcess, reap_launcher},
};
use std::{
    process::Command,
    time::{Duration, Instant},
};

pub(super) fn worker(case: &str) {
    let mode = match case {
        "group_zombie" => 1,
        "group_kill" => 2,
        "group_term" => 3,
        _ => panic!("unknown group case"),
    };
    super::retirement_fixtures::arm(0, 0, 0, 0, mode);
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new("/bin/sleep");
    command.arg("30");
    let mut owned = OwnedProcess::spawn(command, root.path(), root.path(), &[]).unwrap();
    let pid = owned.child.id();
    let mut failures = Vec::new();
    let started = Instant::now();
    owned.shutdown(&mut failures);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cleanup budget renewed"
    );
    assert!(
        owned.child.try_wait().unwrap().is_some(),
        "launcher was not reaped"
    );
    assert!(
        !super::super::process::group_live(pid as i32).unwrap(),
        "descendant survived"
    );
    let cleanup = if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(failures.join("; ")))
    };
    let error =
        finish_resources(Err(anyhow::anyhow!("primary ordinary timeout")), cleanup).unwrap_err();
    assert!(format!("{error:#}").contains("primary ordinary timeout"));
    if mode == 1 {
        assert!(
            error.downcast_ref::<CleanupFailure>().is_none(),
            "resolved proven zombie-only state became terminal: {error:#}"
        );
    } else {
        assert!(
            error.downcast_ref::<CleanupFailure>().is_some(),
            "genuine earlier permission failure disappeared after final absence"
        );
        assert!(format!("{error:#}").contains("Operation not permitted"));
    }
}

#[test]
fn resolved_zombie_only_group_retains_ordinary_error() {
    super::retirement_fixtures::run("group_zombie");
}
#[test]
fn genuine_kill_permission_failure_survives_final_absence() {
    super::retirement_fixtures::run("group_kill");
}
#[test]
fn genuine_term_permission_failure_survives_final_absence() {
    super::retirement_fixtures::run("group_term");
}

#[test]
fn direct_kill_error_survives_successful_reap() {
    let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let result = reap_launcher(
        &mut child,
        |child| {
            child.kill()?;
            Err(std::io::Error::from_raw_os_error(libc::EPERM))
        },
        Duration::from_secs(1),
    );
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    let error = result.expect_err("genuine direct kill failure must survive eventual reap");
    assert!(format!("{error:#}").contains("Operation not permitted"));
}
