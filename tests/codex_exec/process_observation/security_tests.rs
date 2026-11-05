use super::super::fixtures::{private_tempdir, supervised_python};
use super::record_tests::private_record;
use super::*;
use std::{
    os::unix::process::CommandExt,
    process::{Command, Stdio},
};

/// All signal targets in these tests come from this disposable process tree.
/// The worker spawns a group-leading launcher, which spawns one descendant.
pub(super) struct Tree {
    pub(super) root: tempfile::TempDir,
    pub(super) worker: ChildGuard,
    pub(super) anchor: SpawnedWorker,
    pub(super) ownership: Ownership,
}

impl Tree {
    pub(super) fn new() -> Self {
        Self::try_new(|_| {}).unwrap()
    }

    fn try_new(before_capture: impl FnOnce(&Path)) -> io::Result<Self> {
        let root = private_tempdir();
        let launcher = r#"
import os, pathlib, subprocess, sys, time
root = pathlib.Path(sys.argv[1])
child = spawn_owned(['/usr/bin/python3', '-c', 'import time; time.sleep(30)'])
for name, pid in [('descendant.pid', child.pid), ('launcher.pid', os.getpid())]:
    tmp = root/(name+'.tmp')
    with tmp.open('x') as f:
        os.chmod(tmp, 0o600)
        f.write(str(pid))
    os.replace(tmp, root/name)
time.sleep(30)
"#;
        let code = "import subprocess, sys, time; subprocess.Popen(['/usr/bin/python3', '-c', sys.argv[1], sys.argv[2]], start_new_session=True); time.sleep(30)";
        let mut worker = Command::new("/usr/bin/python3");
        super::super::fixtures::fixture_environment(&mut worker, root.path());
        let mut worker = ChildGuard::new(
            worker
                .args(["-c", code, &supervised_python(launcher)])
                .arg(root.path())
                .process_group(0)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let anchor = SpawnedWorker::capture(worker.id())?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !root.path().join("launcher.pid").exists() {
            if Instant::now() >= deadline {
                worker.kill().unwrap();
                worker.wait().unwrap();
                panic!("disposable ownership tree did not start");
            }
            thread::sleep(Duration::from_millis(1));
        }
        before_capture(root.path());
        let records = PrivateRecords::open(root.path(), deadline)?;
        let ownership = Ownership::capture(&records, &anchor)?;
        let tree = Self {
            root,
            worker,
            anchor,
            ownership,
        };
        publish(
            tree.root.path(),
            "supervisor-release.json",
            &json!({"ownership_retained":true}),
        )?;
        Ok(tree)
    }

    fn assert_alive(&self) {
        for identity in [&self.ownership.launcher, &self.ownership.descendant] {
            let process = observe(identity.pid).unwrap().unwrap();
            assert_eq!(process.identity, *identity);
            assert_eq!(
                process.state, "running",
                "unauthorised signal reached disposable process"
            );
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let result = self.ownership.cleanup();
        let _ = self.worker.kill();
        let _ = self.worker.wait();
        if !std::thread::panicking() {
            result.expect("disposable tree cleanup failed");
        }
    }
}

#[test]
fn unrelated_launcher_tree_and_missing_records_never_establish_ownership() {
    let tree = Tree::new();
    let unrelated = Tree::new();
    let forged = private_tempdir();
    private_record(
        forged.path(),
        "launcher.pid",
        unrelated.ownership.launcher.pid.to_string().as_bytes(),
    );
    private_record(
        forged.path(),
        "descendant.pid",
        unrelated.ownership.descendant.pid.to_string().as_bytes(),
    );
    let records =
        PrivateRecords::open(forged.path(), Instant::now() + Duration::from_secs(1)).unwrap();
    assert!(Ownership::capture(&records, &tree.anchor).is_err());
    fs::remove_file(forged.path().join("launcher.pid")).unwrap();
    private_record(
        forged.path(),
        "launcher.pid",
        tree.ownership.launcher.pid.to_string().as_bytes(),
    );
    assert!(
        Ownership::capture(&records, &tree.anchor).is_err(),
        "unrelated descendant accepted"
    );
    fs::remove_file(forged.path().join("launcher.pid")).unwrap();
    assert!(Ownership::capture(&records, &tree.anchor).is_err());
    tree.assert_alive();
    unrelated.assert_alive();
}

#[test]
fn stale_worker_or_target_birth_and_contradictory_evidence_prevent_all_signals() {
    let tree = Tree::new();
    let records =
        PrivateRecords::open(tree.root.path(), Instant::now() + Duration::from_secs(1)).unwrap();
    let mut stale_worker = SpawnedWorker(tree.anchor.0.clone());
    stale_worker.0.birth[0] += 1;
    assert!(Ownership::capture(&records, &stale_worker).is_err());
    for target in ["worker", "launcher", "descendant", "contradiction"] {
        let mut stale = tree.ownership.clone();
        match target {
            "worker" => stale.worker.birth[0] += 1,
            "launcher" => stale.launcher.birth[0] += 1,
            "descendant" => stale.descendant.birth[0] += 1,
            _ => stale.descendant.pgid += 1,
        }
        assert!(stale.cleanup().is_err(), "accepted {target}");
        tree.assert_alive();
    }
}

#[test]
fn records_after_worker_exit_cannot_recreate_ownership_but_retained_evidence_can_cleanup() {
    let mut tree = Tree::new();
    tree.worker.kill().unwrap();
    tree.worker.wait().unwrap();
    let records =
        PrivateRecords::open(tree.root.path(), Instant::now() + Duration::from_secs(1)).unwrap();
    assert!(Ownership::capture(&records, &tree.anchor).is_err());
    tree.assert_alive();
    tree.ownership.cleanup().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let running = [&tree.ownership.launcher, &tree.ownership.descendant]
            .iter()
            .any(|identity| {
                observe(identity.pid)
                    .unwrap()
                    .is_some_and(|process| process.state == "running")
            });
        if !running {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "verified fallback failed to stop disposable processes"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn untrusted_return_receipts_cannot_replace_retained_ownership_or_authorise_signals() {
    let tree = Tree::new();
    assert!(tree.ownership.validate_return(tree.root.path()).is_err());
    tree.assert_alive();
    let process = observe(tree.ownership.descendant.pid).unwrap();
    let live = record(&tree.ownership.descendant, "descendant_ready", process);
    publish(tree.root.path(), "descendant-live.json", &live).unwrap();
    let mut forged = record(&tree.ownership.descendant, "adapter_return", None);
    forged["identity"]["birth"][0] = json!(tree.ownership.descendant.birth[0] + 1);
    publish(tree.root.path(), "descendant-return.json", &forged).unwrap();
    assert!(tree.ownership.validate_return(tree.root.path()).is_err());
    tree.assert_alive();
}

pub(super) fn assert_eventually_stopped(identities: &[Identity]) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if identities.iter().all(|identity| {
            observe(identity.pid).unwrap().is_none_or(|process| {
                assert_eq!(process.identity, *identity);
                process.state != "running"
            })
        }) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "failure abandoned a disposable process"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn assert_child_reaped(pid: i32) {
    // A zombie is stopped but has not been reaped. The spawning parent must
    // have consumed the direct child's wait status before returning an error.
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn setup_failures_stop_children_without_trusting_failed_records() {
    use std::os::unix::fs::PermissionsExt;
    let unrelated = Tree::new();
    for stage in ["private_directory", "unrelated_launcher"] {
        let mut identities = Vec::new();
        let result = Tree::try_new(|root| {
            let records =
                PrivateRecords::open(root, Instant::now() + Duration::from_secs(1)).unwrap();
            let launcher = observe(records.pid_file("launcher.pid").unwrap())
                .unwrap()
                .unwrap();
            identities.push(observe(launcher.ppid).unwrap().unwrap().identity);
            identities.push(launcher.identity);
            identities.push(
                observe(records.pid_file("descendant.pid").unwrap())
                    .unwrap()
                    .unwrap()
                    .identity,
            );
            if stage == "private_directory" {
                fs::set_permissions(root, fs::Permissions::from_mode(0o755)).unwrap();
            } else {
                fs::write(
                    root.join("launcher.pid"),
                    unrelated.ownership.launcher.pid.to_string(),
                )
                .unwrap();
            }
        });
        let error = result
            .err()
            .expect("injected setup failure must remain an error");
        assert_eq!(
            error.to_string(),
            if stage == "private_directory" {
                "fixture directory is not private"
            } else {
                "fixture ancestry or state mismatch"
            }
        );
        assert_eventually_stopped(&identities);
        assert_child_reaped(identities[0].pid);
        unrelated.assert_alive();
    }
}
