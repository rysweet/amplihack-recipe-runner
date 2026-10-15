//! Actual parent death releases an untimed helper using the portable endpoints.
use super::super::{
    anchor_io::TEST_FIFO,
    process::{OwnedProcess, group_live},
};
use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};

fn wait_file(path: &std::path::Path, child: &mut std::process::Child) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !path.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "parent-loss worker exited early"
        );
        assert!(Instant::now() < deadline, "parent-loss readiness watchdog");
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn owner_worker() {
    let Some(root) = std::env::var_os("FIFO_PARENT_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    TEST_FIFO.set(true);
    let mut command = Command::new("/bin/sleep");
    command.arg("30");
    let mut owned = OwnedProcess::spawn(command, &root, &root, &[]).unwrap();
    let launcher = owned.child.id() as i32;
    let members = super::inspection::members(launcher).unwrap();
    let helpers: Vec<_> = members.into_iter().filter(|pid| *pid != launcher).collect();
    assert_eq!(helpers.len(), 1);
    let mut ids = Vec::new();
    use std::os::unix::fs::MetadataExt;
    for entry in fs::read_dir("/proc/self/fd").unwrap() {
        let path = entry.unwrap().path();
        if fs::read_link(&path).is_ok_and(|p| p.to_string_lossy().contains("codex-anchor-")) {
            let info = fs::metadata(path).unwrap();
            ids.push((info.dev(), info.ino()));
        }
    }
    assert_eq!(
        ids.len(),
        1,
        "untimed attempt did not retain its private FIFO control writer"
    );
    let ready = root.join("unrelated-ready");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "adapters::codex_exec::anchor_fifo::tests::exec_worker",
            "--nocapture",
        ])
        .env("FIFO_EXEC_IDENTITIES", serde_json::to_string(&ids).unwrap())
        .env("FIFO_EXEC_READY", &ready);
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| Ok(()));
    }
    let mut unrelated = command.spawn().unwrap();
    wait_file(&ready, &mut unrelated);
    fs::write(
        root.join("owned-pids"),
        format!("{launcher} {} {}", helpers[0], unrelated.id()),
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(30));
    // Only used if the supervisor fails to deliver real parent death.
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    owned.shutdown(&mut Vec::new());
}
#[test]
fn actual_parent_death_exits_untimed_fifo_helper_without_harming_unrelated_launcher() {
    if std::env::var("LIFECYCLE_CASE").as_deref() != Ok("fifo_parent_loss") {
        super::retirement_fixtures::run_case(
            "fifo_parent_loss",
            "adapters::codex_exec::tests::endpoint_parent_loss::actual_parent_death_exits_untimed_fifo_helper_without_harming_unrelated_launcher",
        );
        return;
    }
    // Child-only adoption makes the orphan exit/reap observable and leak-free.
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
        0
    );
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "adapters::codex_exec::tests::endpoint_parent_loss::owner_worker",
            "--nocapture",
        ])
        .env("FIFO_PARENT_ROOT", root.path());
    let mut owner = command.spawn().unwrap();
    let witness = root.path().join("owned-pids");
    wait_file(&witness, &mut owner);
    let ids: Vec<i32> = fs::read_to_string(witness)
        .unwrap()
        .split_whitespace()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(ids.len(), 3);
    owner.kill().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(owner.wait().unwrap().signal(), Some(libc::SIGKILL));
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut status = 0;
    let (helper_reaped, natural_exit) = loop {
        let rc = unsafe { libc::waitpid(ids[1], &mut status, libc::WNOHANG) };
        if rc == ids[1] {
            break (
                true,
                libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            );
        }
        assert_eq!(rc, 0, "lost adopted helper authority");
        if Instant::now() >= deadline {
            break (false, false);
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let unrelated_survived = unsafe {
        libc::waitid(
            libc::P_PID,
            ids[2] as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        ) == 0
            && info.si_pid() == 0
    };
    // Keep direct child identity until each last cleanup operation; no post-reap group signal.
    for pid in [ids[0], ids[2]] {
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    }
    if !helper_reaped {
        unsafe {
            libc::kill(ids[1], libc::SIGKILL);
            libc::waitpid(ids[1], &mut status, 0);
        }
    }
    println!(
        "parent_loss actual_SIGKILL=true launcher={} helper={} unrelated={} helper_exit0={natural_exit} unrelated_survived={unrelated_survived}",
        ids[0], ids[1], ids[2]
    );
    assert!(
        natural_exit,
        "parent loss did not release unlimited helper through EOF"
    );
    assert!(unrelated_survived);
    assert!(!group_live(ids[0]).unwrap());
}
