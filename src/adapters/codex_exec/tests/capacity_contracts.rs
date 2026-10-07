//! Run the unchanged authority contracts under the recorded triggering capacity.
//! The limit belongs only to the subprocess, never to parallel tests in this process.
use std::{
    fs,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn nofile() -> libc::rlimit {
    let mut limit = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0,
        "cannot inspect descriptor capacity"
    );
    limit
}

fn under_recorded_soft_limit(test: &str) {
    let parent = nofile();
    assert!(
        parent.rlim_max >= 65536,
        "hard limit cannot admit this case"
    );
    let child_limit = libc::rlimit {
        rlim_cur: 65536,
        rlim_max: parent.rlim_max,
    };
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("capacity.log");
    let output = fs::File::create(&log).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .stdout(Stdio::from(output.try_clone().unwrap()))
        .stderr(Stdio::from(output));
    // Only an async-signal-safe syscall occurs between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::setrlimit(libc::RLIMIT_NOFILE, &child_limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    println!(
        "capacity_command={command:?} child_soft=65536 inherited_hard={} parent_soft={}",
        parent.rlim_max, parent.rlim_cur
    );
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("capacity watchdog expired (not semantic RED)");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let after = nofile();
    assert_eq!(after.rlim_cur, parent.rlim_cur, "parent soft limit changed");
    assert_eq!(after.rlim_max, parent.rlim_max, "parent hard limit changed");
    let text = fs::read_to_string(log).unwrap();
    println!("capacity_child_exit={status}\n{text}");
    assert!(text.contains("running 1 test"), "contract was not selected");
    assert!(text.contains(test), "wrong authority contract selected");
    assert!(
        status.success(),
        "authority contract failed at child-only soft limit 65536"
    );
    assert!(
        text.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"),
        "authority assertion was skipped or did not complete"
    );
}

#[test]
fn lowered_limit_descriptor_isolation_with_initial_soft_65536() {
    under_recorded_soft_limit(
        "adapters::codex_exec::tests::group_authority::anchor_closes_high_descriptors_above_lowered_soft_limit",
    );
}

#[test]
fn retained_authority_with_initial_soft_65536() {
    under_recorded_soft_limit(
        "adapters::codex_exec::tests::group_authority::original_group_authority_survives_launcher_reaping",
    );
}
