use super::super::fixtures::private_tempdir;
use super::records::RECORD_LIMIT;
use super::*;
use std::{
    ffi::CString,
    os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
};

pub(super) fn private_record(root: &Path, name: &str, data: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join(name))
        .unwrap();
    file.write_all(data).unwrap();
}

#[test]
fn record_reader_rejects_symlinks_fifos_permissions_and_large_files_without_waiting() {
    // A missing O_NONBLOCK must fail this test within a watchdog, rather than
    // hanging the entire harness on a FIFO with no writer.
    if std::env::var_os("CODEX_RECORD_READ_PROBE").is_none() {
        let root = private_tempdir();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        super::super::fixtures::fixture_environment(&mut command, root.path());
        let mut child = ChildGuard::new(command
            .args(["--exact", "process_observation::record_tests::record_reader_rejects_symlinks_fifos_permissions_and_large_files_without_waiting", "--nocapture"])
            .env("CODEX_RECORD_READ_PROBE", "1").spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "record reader probe failed");
                return;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("record reader stalled on an untrusted record");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    for name in ["launcher.pid", "descendant.pid", "descendant-live.json"] {
        for kind in [
            "symlink",
            "fifo",
            "directory",
            "permissions",
            "executable",
            "oversized",
            "hardlink",
        ] {
            let root = private_tempdir();
            let path = root.path().join(name);
            match kind {
                "symlink" => {
                    private_record(root.path(), "other", b"123");
                    symlink(root.path().join("other"), &path).unwrap();
                }
                "fifo" => {
                    use std::os::unix::ffi::OsStrExt;
                    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
                }
                "directory" => fs::create_dir(&path).unwrap(),
                "permissions" | "executable" => {
                    private_record(root.path(), name, b"123");
                    fs::set_permissions(
                        &path,
                        fs::Permissions::from_mode(if kind == "permissions" {
                            0o644
                        } else {
                            0o700
                        }),
                    )
                    .unwrap();
                }
                "oversized" => {
                    private_record(root.path(), name, &vec![b'x'; RECORD_LIMIT as usize + 1])
                }
                "hardlink" => {
                    private_record(root.path(), "other", b"123");
                    fs::hard_link(root.path().join("other"), &path).unwrap();
                }
                _ => unreachable!(),
            }
            let records =
                PrivateRecords::open(root.path(), Instant::now() + Duration::from_secs(1)).unwrap();
            let start = Instant::now();
            assert!(records.read(name).is_err(), "accepted {kind} for {name}");
            assert!(
                start.elapsed() < Duration::from_secs(1),
                "blocked on {kind}"
            );
            // Exercise the callers too, including capture_return's JSON path.
            if name == "descendant-live.json" {
                assert!(capture_return(root.path()).is_err());
            } else {
                assert!(records.pid_file(name).is_err());
            }
        }
    }
}

#[test]
fn record_reads_are_relative_to_the_opened_directory_and_obey_deadlines() {
    let root = private_tempdir();
    let original = root.path().join("original");
    let moved = root.path().join("moved");
    fs::DirBuilder::new().mode(0o700).create(&original).unwrap();
    private_record(&original, "launcher.pid", b"123");
    let records = PrivateRecords::open(&original, Instant::now() + Duration::from_secs(1)).unwrap();
    fs::rename(&original, moved).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&original).unwrap();
    private_record(&original, "launcher.pid", b"456");
    assert_eq!(records.pid_file("launcher.pid").unwrap(), 123);
    for name in [
        "../launcher.pid",
        "/launcher.pid",
        ".",
        "..",
        "",
        "bad\0name",
    ] {
        assert!(records.read(name).is_err());
    }
    let expired = PrivateRecords::open(&original, Instant::now()).unwrap();
    assert_eq!(
        expired.read("launcher.pid").unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn malformed_or_missing_records_are_errors_and_never_publish_return_success() {
    for value in [
        b"".as_slice(),
        b"0",
        b"1",
        b"-2",
        b"123\n",
        b"2147483648",
        b"12345678901",
        b"\xff",
    ] {
        let root = private_tempdir();
        private_record(root.path(), "launcher.pid", value);
        let records =
            PrivateRecords::open(root.path(), Instant::now() + Duration::from_secs(1)).unwrap();
        assert!(records.pid_file("launcher.pid").is_err());
    }
    for value in [b"".as_slice(), b"not json", b"{}", b"{\"identity\":{}}"] {
        let root = private_tempdir();
        private_record(root.path(), "descendant-live.json", value);
        assert!(capture_return(root.path()).is_err());
        assert!(!root.path().join("descendant-return.json").exists());
    }
    let root = private_tempdir();
    assert_eq!(
        capture_return(root.path()).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn private_directories_are_created_under_ordinary_umask() {
    use std::os::unix::fs::MetadataExt;
    if std::env::var_os("CODEX_PRIVATE_MODE_PROBE").is_none() {
        let root = private_tempdir();
        let mut command = std::process::Command::new("/usr/bin/python3");
        super::super::fixtures::fixture_environment(&mut command, root.path());
        let mut child = ChildGuard::new(command
            .args(["-c", "import os,sys; os.umask(0o022); os.execv(sys.argv[1],sys.argv[1:])"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "process_observation::record_tests::private_directories_are_created_under_ordinary_umask", "--nocapture"])
            .env("CODEX_PRIVATE_MODE_PROBE", "1").spawn().unwrap());
        let status = finish_observed(
            &mut child,
            root.path(),
            Arc::new(AtomicBool::new(false)),
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(status.success(), "ordinary umask control failed");
        return;
    }
    let root = private_tempdir();
    let metadata = fs::symlink_metadata(root.path()).unwrap();
    assert_eq!(metadata.mode() & 0o7777, 0o700);
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    PrivateRecords::open(root.path(), Instant::now() + Duration::from_secs(1)).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        PrivateRecords::open(root.path(), Instant::now())
            .err()
            .unwrap()
            .to_string(),
        "fixture directory is not private"
    );
}

#[test]
fn watchdog_joins_observer_and_cleans_retained_tree_before_returning_error() {
    use super::security_tests::{Tree, assert_eventually_stopped};
    let mut tree = Tree::new();
    let done = Arc::new(AtomicBool::new(false));
    let observer = watch_ready(
        tree.root.path(),
        SpawnedWorker(tree.anchor.0.clone()),
        done.clone(),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while !tree.root.path().join("descendant-live.json").exists() {
        assert!(
            Instant::now() < deadline,
            "observer did not publish readiness"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let error = finish_observed(
        &mut tree.worker,
        tree.root.path(),
        done.clone(),
        Some(observer),
        Duration::ZERO,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(error.to_string(), "adapter exceeded watchdog");
    assert!(done.load(Ordering::Acquire));
    super::security_tests::assert_child_reaped(tree.anchor.0.pid);
    assert_eventually_stopped(&[
        tree.anchor.0.clone(),
        tree.ownership.launcher.clone(),
        tree.ownership.descendant.clone(),
    ]);
}

#[test]
fn observation_and_publication_errors_release_verified_ownership() {
    use super::security_tests::{Tree, assert_eventually_stopped};
    for stage in ["observation", "publication"] {
        let tree = Tree::new();
        if stage == "publication" {
            private_record(tree.root.path(), "descendant-live.json.tmp", b"obstruction");
        }
        let result = complete_ready(tree.root.path(), tree.ownership.clone(), |pid| {
            if stage == "observation" {
                Err(io::Error::other("injected observation failure"))
            } else {
                observe(pid)
            }
        });
        let error = result
            .err()
            .expect("injected observer failure must remain an error");
        if stage == "observation" {
            assert_eq!(error.to_string(), "injected observation failure");
        } else {
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        }
        assert_eventually_stopped(&[
            tree.ownership.launcher.clone(),
            tree.ownership.descendant.clone(),
        ]);
    }
}
