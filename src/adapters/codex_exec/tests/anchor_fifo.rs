//! Linux mechanism controls for the identical portable FIFO constructor.
use super::{construct, stat};
use std::{
    fs,
    os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    process::{Child, Command},
    time::{Duration, Instant},
};

struct Reaped(Child);
impl Drop for Reaped {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}
fn wait_file(path: &std::path::Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !path.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "endpoint worker exited before readiness"
        );
        assert!(
            Instant::now() < deadline,
            "endpoint worker watchdog expired"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn exec_without_endpoints(ids: Vec<(u64, u64)>, root: &std::path::Path) -> Reaped {
    let ready = root.join(format!("ready-{}", uuid::Uuid::new_v4()));
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "adapters::codex_exec::anchor_fifo::tests::exec_worker",
            "--nocapture",
        ])
        .env("FIFO_EXEC_IDENTITIES", serde_json::to_string(&ids).unwrap())
        .env("FIFO_EXEC_READY", &ready);
    // Force the fork/exec route also used by real provider launchers.
    unsafe {
        command.pre_exec(|| Ok(()));
    }
    let mut child = Reaped(command.spawn().unwrap());
    wait_file(&ready, &mut child.0);
    child
}
#[test]
fn exec_worker() {
    let Ok(ids) = std::env::var("FIFO_EXEC_IDENTITIES") else {
        return;
    };
    let ids: Vec<(u64, u64)> = serde_json::from_str(&ids).unwrap();
    for entry in fs::read_dir("/proc/self/fd").unwrap() {
        let fd: i32 = entry
            .unwrap()
            .file_name()
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        if let Ok(info) = stat(fd) {
            assert!(
                !ids.contains(&(info.st_dev, info.st_ino)),
                "private endpoint survived exec"
            );
        }
    }
    fs::write(std::env::var_os("FIFO_EXEC_READY").unwrap(), "ready").unwrap();
    std::thread::sleep(Duration::from_secs(30));
}
#[test]
fn concurrent_exec_at_each_open_cannot_retain_fifo_or_mask_eof() {
    if std::env::var("LIFECYCLE_CASE").as_deref() != Ok("fifo_isolation") {
        crate::adapters::codex_exec::tests::retirement_fixtures::run_case(
            "fifo_isolation",
            "adapters::codex_exec::anchor_fifo::tests::concurrent_exec_at_each_open_cannot_retain_fifo_or_mask_eof",
        );
        return;
    }
    let parent = tempfile::tempdir().unwrap();
    let root = tempfile::Builder::new()
        .prefix("fifo-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(parent.path())
        .unwrap();
    let removed = root.path().to_owned();
    let mut launchers = Vec::new();
    let pipe = construct(root, |fds| {
        let ids = fds
            .iter()
            .map(|fd| {
                let s = stat(*fd).unwrap();
                (s.st_dev, s.st_ino)
            })
            .collect();
        // Join a concurrent launcher at a deterministic boundary inside creation.
        let child = std::thread::scope(|scope| {
            scope
                .spawn(|| exec_without_endpoints(ids, parent.path()))
                .join()
                .unwrap()
        });
        launchers.push(child);
        Ok(())
    })
    .unwrap();
    assert_eq!(launchers.len(), 2);
    assert!(
        !removed.exists(),
        "private FIFO path remained after construction"
    );
    let byte = 0xA7u8;
    assert_eq!(
        unsafe { libc::write(pipe.write.as_raw_fd(), (&byte as *const u8).cast(), 1) },
        1
    );
    let mut received = 0u8;
    assert_eq!(
        unsafe { libc::read(pipe.read.as_raw_fd(), (&mut received as *mut u8).cast(), 1) },
        1
    );
    assert_eq!(received, byte);
    drop(pipe.write);
    assert_eq!(
        unsafe { libc::read(pipe.read.as_raw_fd(), (&mut received as *mut u8).cast(), 1) },
        0,
        "unrelated execed launcher retained writer and masked EOF"
    );
    for child in &mut launchers {
        assert!(child.0.try_wait().unwrap().is_none());
    }
}
#[test]
fn partial_construction_closes_each_endpoint_and_removes_paths() {
    if std::env::var("LIFECYCLE_CASE").as_deref() != Ok("fifo_partial") {
        crate::adapters::codex_exec::tests::retirement_fixtures::run_case(
            "fifo_partial",
            "adapters::codex_exec::anchor_fifo::tests::partial_construction_closes_each_endpoint_and_removes_paths",
        );
        return;
    }
    for boundary in [1, 2] {
        let parent = tempfile::tempdir().unwrap();
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(parent.path())
            .unwrap();
        let path = root.path().to_owned();
        let mut seen = Vec::new();
        let mut calls = 0;
        let error = construct(root, |fds| {
            seen.extend_from_slice(fds);
            calls += 1;
            if calls == boundary && boundary == 2 {
                // A genuine unlink cleanup failure must retain the primary error.
                fs::remove_file(path.join("endpoint")).unwrap();
            }
            anyhow::ensure!(
                calls != boundary,
                "injected construction failure {boundary}"
            );
            Ok(())
        })
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains("injected construction failure"));
        if boundary == 2 {
            assert!(format!("{error:#}").contains("Unlink private Codex FIFO"));
            assert!(
                error
                    .downcast_ref::<crate::adapters::codex_exec::CleanupFailure>()
                    .is_some()
            );
        }
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
        for fd in seen {
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
        }
    }
}

#[path = "anchor_fifo_masks.rs"]
pub(in crate::adapters::codex_exec) mod masks;
