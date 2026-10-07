//! Internal portable factory -> authority anchor -> launcher lifecycle contracts.
use super::super::anchor_fifo::tests::masks::run;
use super::super::{anchor_io::TEST_FIFO, process::OwnedProcess};
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn owned_launcher_worker() {
    if std::env::var("FIFO_MASK_CHILD").as_deref() != Ok("1") {
        return;
    }
    TEST_FIFO.set(true);
    let root = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let mut command = Command::new("/bin/sleep");
    command.arg("30");
    let mut process = OwnedProcess::spawn(command, root.path(), root.path(), &[])
        .expect("portable endpoints failed through actual anchor establishment");
    let group = process.child.id() as i32;
    let helpers: Vec<_> = super::inspection::members(group)
        .unwrap()
        .into_iter()
        .filter(|pid| *pid != group)
        .collect();
    let mut failures = Vec::new();
    process.shutdown(&mut failures);
    assert_eq!(helpers.len(), 1, "original group authority not retained");
    assert!(failures.is_empty(), "cleanup diagnostics: {failures:?}");
    assert!(
        process.child.try_wait().unwrap().is_some(),
        "launcher not reaped"
    );
    assert!(super::inspection::members(group).unwrap().is_empty());
    assert!(!super::super::process::group_live(group).unwrap());
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(helpers[0], &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cleanup exceeded bounds"
    );
    TEST_FIFO.set(false);
}

macro_rules! masks {
    ($owned:ident, $parent:ident, $mask:expr) => {
        #[test] fn $owned() { run($mask, "adapters::codex_exec::tests::anchor_fifo_integration::owned_launcher_worker", "fifo_owned"); }
        #[test] fn $parent() { run($mask, "adapters::codex_exec::tests::endpoint_parent_loss::actual_parent_death_exits_untimed_fifo_helper_without_harming_unrelated_launcher", "fifo_parent_loss"); }
    };
}
masks!(owned_000, parent_loss_000, 0o000);
masks!(owned_002, parent_loss_002, 0o002);
masks!(owned_022, parent_loss_022, 0o022);
masks!(owned_077, parent_loss_077, 0o077);

#[test]
fn refused_startup_worker() {
    if std::env::var("FIFO_MASK_CHILD").as_deref() != Ok("1") {
        return;
    }
    TEST_FIFO.set(true);
    let children = format!("/proc/self/task/{}/children", unsafe {
        libc::syscall(libc::SYS_gettid)
    });
    assert!(
        std::fs::read_to_string(&children)
            .unwrap()
            .trim()
            .is_empty()
    );
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new("/bin/sleep");
    command.arg("30");
    let started = Instant::now();
    let failure = match OwnedProcess::spawn(command, root.path(), root.path(), &[]) {
        Err(error) => error,
        Ok(mut process) => {
            process.shutdown(&mut Vec::new());
            panic!("restricted directory unexpectedly admitted owned endpoints");
        }
    };
    let error = super::super::finish_resources(
        Err(anyhow::anyhow!("primary ordinary fixture diagnostic")),
        Err(failure),
    )
    .unwrap_err();
    assert!(
        error
            .downcast_ref::<super::super::CleanupFailure>()
            .is_some()
    );
    let diagnostic = format!("{error:#}");
    for text in [
        "primary ordinary fixture diagnostic",
        "Codex anchor establishment",
        "directory",
    ] {
        assert!(diagnostic.contains(text), "lost {text}: {diagnostic}");
    }
    assert!(error.downcast_ref::<super::super::Interruption>().is_none());
    assert!(
        std::fs::read_to_string(children).unwrap().trim().is_empty(),
        "post-spawn refusal left live or unreaped children"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    TEST_FIFO.set(false);
}
#[test]
fn owner_execute_refusal_unwinds_spawned_launcher() {
    run(
        0o100,
        "adapters::codex_exec::tests::anchor_fifo_integration::refused_startup_worker",
        "fifo_refused",
    );
}
#[test]
fn owner_write_refusal_unwinds_spawned_launcher() {
    run(
        0o200,
        "adapters::codex_exec::tests::anchor_fifo_integration::refused_startup_worker",
        "fifo_refused",
    );
}
