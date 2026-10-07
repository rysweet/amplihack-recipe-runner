//! Child-only common-mask contracts at existing production and fixture boundaries.
use std::{
    fs,
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

pub(in crate::adapters::codex_exec) fn run(mask: u32, selector: &str, case: &str) {
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
    let observer = std::env::var_os("FIFO_CREATION_OBSERVER")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| compile("fifo_creation_observer.c", "observer.so", root.path()));
    let preload = if case.starts_with("adapter_") {
        let interposer = std::env::var_os("LIFECYCLE_INTERPOSER")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| compile("lifecycle_interpose.c", "lifecycle.so", root.path()));
        format!("{}:{}", observer.display(), interposer.display())
    } else {
        observer.display().to_string()
    };
    let creation_log = root.path().join("creation.tsv");
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
    println!(
        "child_mask={mask:03o} exit={:?}\n{out}\n{err}",
        status.code()
    );
    assert_eq!(parent_mask(), before, "parent mask changed");
    // Keep failure evidence as well as optional externally requested success evidence.
    let preserve = !status.success() || std::env::var_os("FIFO_MASK_ARTIFACT_ROOT").is_some();
    let path = if preserve {
        root.keep()
    } else {
        root.path().to_owned()
    };
    assert!(
        out.contains("running 1 test"),
        "zero worker selection: {path:?}"
    );
    assert!(
        status.success(),
        "mask {mask:03o} contract failed: {path:?}"
    );
    if mask & 0o700 == 0 {
        verify_creation(&creation_log);
    }
    assert_eq!(
        fs::read_dir(tmp).unwrap().count(),
        0,
        "private paths leaked"
    );
}

fn compile(source: &str, name: &str, root: &Path) -> std::path::PathBuf {
    let library = root.join(name);
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(source);
    let mut command = Command::new("/usr/bin/cc");
    command
        .args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror", "-o"])
        .arg(&library)
        .arg(source)
        .arg("-ldl");
    let output = command.output().unwrap();
    fs::write(root.join(format!("{name}-compile.stdout")), &output.stdout).unwrap();
    fs::write(root.join(format!("{name}-compile.stderr")), &output.stderr).unwrap();
    println!(
        "fixture_compile command={command:?} exit={:?}",
        output.status.code()
    );
    assert!(output.status.success(), "fixture failed to compile");
    library
}

fn verify_creation(log: &Path) {
    let text = fs::read_to_string(log).expect("missing creation observations");
    let rows: Vec<Vec<&str>> = text
        .lines()
        .map(|line| line.split('\t').collect())
        .collect();
    let fifos: Vec<_> = rows.iter().filter(|row| row[0] == "fifo").collect();
    assert!(!fifos.is_empty(), "portable factory never created a FIFO");
    for fifo in fifos {
        assert_eq!(fifo.len(), 8);
        assert_eq!(fifo[1].parse::<u32>().unwrap(), 0o600);
        assert_eq!(fifo[2], "0", "FIFO observation failed");
        let mode = fifo[3].parse::<u32>().unwrap();
        assert_eq!(mode & libc::S_IFMT, libc::S_IFIFO);
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(fifo[4].parse::<u32>().unwrap(), unsafe { libc::geteuid() });
        assert_ne!(fifo[6], "0", "missing FIFO inode identity");
        let parent = Path::new(fifo[7]).parent().unwrap();
        let directory = rows
            .iter()
            .find(|row| row[0] == "directory" && Path::new(row[7]) == parent)
            .expect("missing permission-at-creation observation");
        assert_eq!(directory[2], "0", "directory observation failed");
        let mode = directory[3].parse::<u32>().unwrap();
        assert_eq!(mode & libc::S_IFMT, libc::S_IFDIR);
        assert_eq!(mode & 0o777, 0o700, "directory was public at creation");
        assert_eq!(directory[4], fifo[4]);
        assert_eq!(directory[5], fifo[5]);
        assert_ne!(directory[6], "0");
        assert!(!parent.exists(), "FIFO root not removed");
    }
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
