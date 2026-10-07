//! Observable original-group authority and inherited-descriptor isolation.
use super::super::{
    CleanupFailure,
    process::{OwnedProcess, group_live},
};
use std::{fs, os::fd::AsRawFd, process::Command, sync::atomic::AtomicBool};

fn members(group: i32) -> Vec<i32> {
    fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let pid = path.file_name()?.to_str()?.parse::<i32>().ok()?;
            let stat = fs::read_to_string(path.join("stat")).ok()?;
            let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
            (fields.get(2)?.parse::<i32>().ok()? == group).then_some(pid)
        })
        .collect()
}
pub(super) fn worker(case: &str) {
    let root = tempfile::tempdir().unwrap();
    if case == "authority_unrelated" {
        use std::os::unix::process::CommandExt;
        let mut unrelated = unsafe {
            Command::new("/bin/sleep")
                .arg("30")
                .pre_exec(|| {
                    if libc::setpgid(0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                })
                .spawn()
                .unwrap()
        };
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let mut owned = OwnedProcess::spawn(command, root.path(), root.path(), &[]).unwrap();
        assert_ne!(owned.child.id(), unrelated.id());
        let mut failures = Vec::new();
        owned.shutdown(&mut failures);
        let survived = unrelated.try_wait().unwrap().is_none();
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
        assert!(survived, "owned teardown signaled an unrelated group");
        assert!(failures.is_empty(), "{failures:?}");
        return;
    }
    if case.starts_with("authority_reaper") {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_sigaction = if case.ends_with("ignore") {
                libc::SIG_IGN
            } else {
                libc::SIG_DFL
            };
            if case.ends_with("no_cldwait") {
                action.sa_flags = libc::SA_NOCLDWAIT;
            }
            assert_eq!(
                libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()),
                0
            );
        }
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo invoked > invoked; exec /bin/sleep 0.02"]);
        let result = OwnedProcess::spawn(command, root.path(), root.path(), &[]);
        let error = match result {
            Err(error) => error,
            Ok(mut process) => {
                process.shutdown(&mut Vec::new());
                panic!("unsafe automatic reaping policy accepted before spawn");
            }
        };
        assert!(error.downcast_ref::<CleanupFailure>().is_some());
        assert!(
            !root.path().join("invoked").exists(),
            "reaper policy was rejected after user entry"
        );
        return;
    }
    let unrelated = root.path().join("unrelated-private-handle");
    let handle = fs::File::create(&unrelated).unwrap();
    let high = unsafe { libc::fcntl(handle.as_raw_fd(), libc::F_DUPFD, 65536) };
    assert!(high >= 65536);
    let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    let original = limit;
    if case == "authority_fd_lowered_limit" {
        limit.rlim_cur = 4096;
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    }
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "cat >/dev/null"]);
    let mut process = OwnedProcess::spawn(command, root.path(), root.path(), &[]).unwrap();
    let group = process.child.id() as i32;
    let delivery = process.deliver_and_wait("safe task", Some(3), &AtomicBool::new(false));
    let retained = members(group);
    let leaked: Vec<_> = retained
        .iter()
        .filter(|pid| {
            fs::read_dir(format!("/proc/{pid}/fd"))
                .unwrap()
                .any(|entry| {
                    entry
                        .ok()
                        .and_then(|e| fs::read_link(e.path()).ok())
                        .is_some_and(|p| p == unrelated)
                })
        })
        .copied()
        .collect();
    let mut failures = Vec::new();
    process.shutdown(&mut failures);
    assert_eq!(unsafe { libc::close(high) }, 0);
    assert_eq!(
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &original) },
        0
    );
    delivery.unwrap();
    assert!(
        !retained.is_empty(),
        "launcher reaping released group identity before destructive operations"
    );
    assert!(
        !retained.contains(&group),
        "launcher was not reaped before observation"
    );
    assert!(
        leaked.is_empty(),
        "anchor inherited private unrelated descriptors: {leaked:?}"
    );
    assert!(failures.is_empty(), "{failures:?}");
    assert!(!group_live(group).unwrap());
}
#[test]
fn original_group_authority_survives_launcher_reaping() {
    super::retirement_fixtures::run("authority_held");
}
#[test]
fn anchor_closes_high_descriptors_above_lowered_soft_limit() {
    super::retirement_fixtures::run("authority_fd_lowered_limit");
}
#[test]
fn explicit_sigchld_ignore_refused_before_launcher_spawn() {
    super::retirement_fixtures::run("authority_reaper_ignore");
}
#[test]
fn automatic_no_cldwait_refused_before_launcher_spawn() {
    super::retirement_fixtures::run("authority_reaper_no_cldwait");
}

#[test]
fn owned_shutdown_preserves_unrelated_live_group() {
    super::retirement_fixtures::run("authority_unrelated");
}
