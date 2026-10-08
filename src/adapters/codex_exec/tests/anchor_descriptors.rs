//! Concurrent anchors close other attempts' stdin and diagnostic pipe writers.
use super::super::process::OwnedProcess;
use std::{fs, os::fd::AsRawFd, process::Command, sync::atomic::AtomicBool};

pub(super) fn worker() {
    super::retirement_fixtures::arm(0, 0, 0, 0, 0);
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new("/bin/cat");
    command.arg("-");
    let mut first = OwnedProcess::spawn(command, root.path(), root.path(), &[]).unwrap();
    let pipes: Vec<_> = [
        first.child.stdin.as_ref().unwrap().as_raw_fd(),
        first.child.stdout.as_ref().unwrap().as_raw_fd(),
        first.child.stderr.as_ref().unwrap().as_raw_fd(),
    ]
    .into_iter()
    .map(|fd| fs::read_link(format!("/proc/self/fd/{fd}")).unwrap())
    .collect();
    let mut second =
        OwnedProcess::spawn(Command::new("/bin/cat"), root.path(), root.path(), &[]).unwrap();
    let helpers: Vec<_> = super::inspection::members(second.child.id() as i32)
        .unwrap()
        .into_iter()
        .filter(|pid| *pid != second.child.id() as i32)
        .collect();
    let mut leaked = Vec::new();
    for pid in &helpers {
        for path in super::inspection::descriptors(*pid).unwrap() {
            if pipes.contains(&path) {
                leaked.push((pid, path));
            }
        }
    }
    let result1 = first.deliver_and_wait("one", Some(3), &AtomicBool::new(false));
    let mut failures = Vec::new();
    first.shutdown(&mut failures);
    let result2 = second.deliver_and_wait("two", Some(3), &AtomicBool::new(false));
    second.shutdown(&mut failures);
    result1.unwrap();
    result2.unwrap();
    assert_eq!(helpers.len(), 1);
    assert!(
        leaked.is_empty(),
        "cross-attempt descriptor leak: {leaked:?}"
    );
    assert!(failures.is_empty(), "{failures:?}");
}
#[test]
fn concurrent_helpers_isolate_each_attempts_transport() {
    super::extended_fixtures::run("descriptors_concurrent");
}
