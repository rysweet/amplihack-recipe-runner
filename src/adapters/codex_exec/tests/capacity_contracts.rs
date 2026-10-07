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

fn under_child_limit(test: &str, constrained_hard: bool) {
    let parent = nofile();
    assert!(
        parent.rlim_max >= 65536,
        "hard limit cannot admit this case"
    );
    let child_limit = libc::rlimit {
        rlim_cur: 65536,
        rlim_max: if constrained_hard {
            65536
        } else {
            parent.rlim_max
        },
    };
    let root = match std::env::var_os("LIFECYCLE_ARTIFACT_ROOT") {
        Some(path) => tempfile::Builder::new()
            .prefix("capacity-")
            .tempdir_in(path)
            .unwrap(),
        None => tempfile::tempdir().unwrap(),
    };
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
        "capacity_command={command:?} child_soft=65536 child_hard={} parent_soft={}",
        child_limit.rlim_max, parent.rlim_cur
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
            if std::env::var_os("LIFECYCLE_ARTIFACT_ROOT").is_some() {
                println!("preserved_capacity_fixture={}", root.keep().display());
            }
            panic!("capacity watchdog expired (not semantic RED)");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let after = nofile();
    assert_eq!(after.rlim_cur, parent.rlim_cur, "parent soft limit changed");
    assert_eq!(after.rlim_max, parent.rlim_max, "parent hard limit changed");
    // Preserve diagnostics before any evidence read or semantic assertion can fail.
    if std::env::var_os("LIFECYCLE_ARTIFACT_ROOT").is_some() {
        println!("preserved_capacity_fixture={}", root.keep().display());
    }
    let text = fs::read_to_string(log).unwrap();
    println!("capacity_child_exit={status}\n{text}");
    assert!(text.contains("running 1 test"), "contract was not selected");
    assert!(text.contains(test), "wrong authority contract selected");
    assert!(
        status.success(),
        "authority contract failed at child-only soft/hard capacity"
    );
    assert!(
        text.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"),
        "authority assertion was skipped or did not complete"
    );
    let expected = if child_limit.rlim_max > 65536 {
        65536
    } else {
        65535
    };
    assert!(text.contains(&format!("target={expected}")));
    assert!(text.contains(&format!("high={expected} inheritable=true")));
    assert!(text.contains("original_soft=65536"));
    assert!(text.contains(&format!("hard={}", child_limit.rlim_max)));
    if test.ends_with("anchor_closes_high_descriptors_above_lowered_soft_limit") {
        assert!(text.contains("spawn_soft=4096"));
    }
}

#[test]
fn lowered_limit_descriptor_isolation_with_initial_soft_65536() {
    under_child_limit(
        "adapters::codex_exec::tests::group_authority::anchor_closes_high_descriptors_above_lowered_soft_limit",
        false,
    );
}

#[test]
fn retained_authority_with_initial_soft_65536() {
    under_child_limit(
        "adapters::codex_exec::tests::group_authority::original_group_authority_survives_launcher_reaping",
        false,
    );
}

#[test]
fn lowered_limit_descriptor_isolation_with_hard_65536() {
    under_child_limit(
        "adapters::codex_exec::tests::group_authority::anchor_closes_high_descriptors_above_lowered_soft_limit",
        true,
    );
}

#[test]
fn retained_authority_with_hard_65536() {
    under_child_limit(
        "adapters::codex_exec::tests::group_authority::original_group_authority_survives_launcher_reaping",
        true,
    );
}
