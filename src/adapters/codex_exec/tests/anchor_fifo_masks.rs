//! Child-only common-mask contracts at existing production and fixture boundaries.
use std::{
    fs,
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, process::CommandExt},
    },
    process::Command,
    time::{Duration, Instant},
};

#[path = "anchor_fifo_observer.rs"]
mod observer;
use observer::{compile, verify_creation};

pub(in crate::adapters::codex_exec) fn run(mask: u32, selector: &str, case: &str) {
    if let Err(path) = run_observed(mask, selector, case, None) {
        panic!("mask {mask:03o} contract failed: evidence={path:?}");
    }
}

fn run_observed(
    mask: u32,
    selector: &str,
    case: &str,
    fault: Option<&str>,
) -> Result<std::path::PathBuf, std::path::PathBuf> {
    run_observed_with_setup(mask, selector, case, fault, |root| {
        let observer = std::env::var_os("FIFO_CREATION_OBSERVER")
            .map(|path| Ok(std::path::PathBuf::from(path)))
            .unwrap_or_else(|| compile("fifo_creation_observer.c", "observer.so", root))?;
        if case.starts_with("adapter_") {
            let interposer = std::env::var_os("LIFECYCLE_INTERPOSER")
                .map(|path| Ok(std::path::PathBuf::from(path)))
                .unwrap_or_else(|| compile("lifecycle_interpose.c", "lifecycle.so", root))?;
            Ok(format!("{}:{}", observer.display(), interposer.display()))
        } else {
            Ok(observer.display().to_string())
        }
    })
}

fn run_observed_with_setup(
    mask: u32,
    selector: &str,
    case: &str,
    fault: Option<&str>,
    setup: impl FnOnce(&std::path::Path) -> anyhow::Result<String>,
) -> Result<std::path::PathBuf, std::path::PathBuf> {
    // Read Linux's published mask without ever changing the parent process mask.
    let parent_mask = || {
        fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find(|line| line.starts_with("Umask:"))
            .unwrap()
            .to_owned()
    };
    let before = parent_mask();
    let root = if let Some(path) = std::env::var_os("FIFO_MASK_ARTIFACT_ROOT") {
        tempfile::Builder::new()
            .prefix("mask-")
            .tempdir_in(path)
            .unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let tmp = root.path().join("tmp");
    fs::create_dir(&tmp).unwrap();
    let preload = match setup(root.path()) {
        Ok(preload) => preload,
        Err(error) => {
            let path = root.keep();
            let diagnostic = format!("fixture setup failed: {error:#}");
            eprintln!("{diagnostic}; evidence={path:?}");
            if let Err(write_error) = fs::write(path.join("setup-error"), &diagnostic) {
                eprintln!("could not save setup error: {write_error}; evidence={path:?}");
            }
            return Err(path);
        }
    };
    let creation_log = root.path().join("creation.tsv");
    // The parent owns the log: an owner-write-removing child mask must only
    // restrict the objects under observation, not subsequent log appends.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&creation_log)
        .unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", selector, "--nocapture", "--test-threads=1"])
        .env("FIFO_MASK_CHILD", "1")
        .env("FIFO_MASK_VALUE", format!("{mask:o}"))
        .env("LIFECYCLE_CASE", case)
        .env("TMPDIR", &tmp)
        .env("FIFO_CREATION_SCOPE", &tmp)
        .env("FIFO_CREATION_LOG", &creation_log)
        .env("LD_PRELOAD", preload)
        .env("LIFECYCLE_ROOT", root.path())
        .env("LIFECYCLE_EVENTS", root.path().join("events"))
        .stdout(fs::File::create(root.path().join("stdout")).unwrap())
        .stderr(fs::File::create(root.path().join("stderr")).unwrap());
    // Ambient instrumentation faults must never affect ordinary observations.
    command.env_remove("FIFO_CREATION_FAULT");
    if let Some(fault) = fault {
        command.env("FIFO_CREATION_FAULT", fault);
    }
    // This closure runs only after fork, immediately before the disposable exec.
    unsafe {
        command.pre_exec(move || {
            libc::umask(mask);
            Ok(())
        });
    }
    println!("mask_command={command:?} artifacts={:?}", root.path());
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            let path = root.keep();
            panic!("mask watchdog (not semantic RED), evidence={path:?}");
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let out = fs::read_to_string(root.path().join("stdout")).unwrap();
    let err = fs::read_to_string(root.path().join("stderr")).unwrap();
    fs::write(root.path().join("exit-status"), status.to_string()).unwrap();
    println!(
        "child_mask={mask:03o} exit={:?}\n{out}\n{err}",
        status.code()
    );
    // Keep artifacts for every validation failure, including a marker with exit 0
    // and otherwise valid rows. Existing semantic assertions stay in this gate.
    let checked = std::panic::catch_unwind(|| {
        assert_eq!(parent_mask(), before, "parent mask changed");
        assert!(out.contains("running 1 test"), "zero worker selection");
        assert!(status.success(), "child contract failed: {status}");
        assert!(
            !err.contains(observer::FAILURE_MARKER),
            "creation observer failed"
        );
        if mask & 0o700 == 0 {
            verify_creation(&creation_log);
        }
        assert_eq!(
            fs::read_dir(tmp).unwrap().count(),
            0,
            "private paths leaked"
        );
    });
    let preserve = checked.is_err()
        || fault.is_some()
        || std::env::var_os("FIFO_MASK_ARTIFACT_ROOT").is_some();
    let path = if preserve {
        root.keep()
    } else {
        root.path().to_owned()
    };
    if checked.is_ok() { Ok(path) } else { Err(path) }
}

#[test]
fn production_worker() {
    if std::env::var("FIFO_MASK_CHILD").as_deref() != Ok("1") {
        return;
    }
    let mask = u32::from_str_radix(&std::env::var("FIFO_MASK_VALUE").unwrap(), 8).unwrap();
    let result = super::super::new();
    if mask & 0o700 != 0 {
        let error = result
            .err()
            .expect("owner-bit-removing mask admitted endpoints");
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("directory") || diagnostic.contains("Permission denied"));
        assert!(
            error
                .downcast_ref::<crate::adapters::codex_exec::CleanupFailure>()
                .is_none(),
            "ordinary privacy refusal gained false cleanup classification: {error:#}"
        );
        return;
    }
    let pipe = result.expect("production portable constructor refused common mask");
    let read = super::stat(pipe.read.as_raw_fd()).unwrap();
    let write = super::stat(pipe.write.as_raw_fd()).unwrap();
    assert_eq!((read.st_dev, read.st_ino), (write.st_dev, write.st_ino));
    let observations = fs::read_to_string(std::env::var_os("FIFO_CREATION_LOG").unwrap()).unwrap();
    let creation: Vec<_> = observations
        .lines()
        .find(|line| line.starts_with("fifo\t"))
        .expect("missing FIFO identity observation")
        .split('\t')
        .collect();
    assert_eq!(read.st_dev, creation[5].parse::<u64>().unwrap());
    assert_eq!(read.st_ino, creation[6].parse::<u64>().unwrap());
    for (fd, access) in [
        (pipe.read.as_raw_fd(), libc::O_RDONLY),
        (pipe.write.as_raw_fd(), libc::O_WRONLY),
    ] {
        let info = super::stat(fd).unwrap();
        assert_eq!(info.st_mode & 0o777, 0o600);
        assert_eq!(info.st_uid, unsafe { libc::geteuid() });
        assert_eq!(info.st_mode & libc::S_IFMT, libc::S_IFIFO);
        let status = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(status >= 0);
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
        assert_eq!(status & libc::O_ACCMODE, access);
        assert_ne!(status & libc::O_NONBLOCK, 0);
        assert_ne!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }
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
    let writer = pipe.write.as_raw_fd();
    drop(pipe.write);
    assert_eq!(
        unsafe { libc::read(pipe.read.as_raw_fd(), (&mut received as *mut u8).cast(), 1) },
        0
    );
    assert_eq!(unsafe { libc::fcntl(writer, libc::F_GETFD) }, -1);
    let reader = pipe.read.as_raw_fd();
    drop(pipe.read);
    assert_eq!(unsafe { libc::fcntl(reader, libc::F_GETFD) }, -1);
}

macro_rules! masks {
    ($production:ident, $isolation:ident, $partial:ident, $mask:expr) => {
        #[test] fn $production() { run($mask, "adapters::codex_exec::anchor_fifo::tests::masks::production_worker", "fifo_production"); }
        #[test] fn $isolation() { run($mask, "adapters::codex_exec::anchor_fifo::tests::concurrent_exec_at_each_open_cannot_retain_fifo_or_mask_eof", "fifo_isolation"); }
        #[test] fn $partial() { run($mask, "adapters::codex_exec::anchor_fifo::tests::partial_construction_closes_each_endpoint_and_removes_paths", "fifo_partial"); }
    };
}
masks!(production_000, fixture_exec_000, fixture_partial_000, 0o000);
masks!(production_002, fixture_exec_002, fixture_partial_002, 0o002);
masks!(production_022, fixture_exec_022, fixture_partial_022, 0o022);
masks!(production_077, fixture_exec_077, fixture_partial_077, 0o077);
#[test]
fn owner_execute_bit_removed_refuses_safely() {
    run(
        0o100,
        "adapters::codex_exec::anchor_fifo::tests::masks::production_worker",
        "fifo_owner_bits",
    );
}
#[test]
fn owner_write_bit_removed_refuses_safely() {
    run(
        0o200,
        "adapters::codex_exec::anchor_fifo::tests::masks::production_worker",
        "fifo_owner_bits",
    );
}
