//! Creation evidence checks and genuine observer failure controls.
use anyhow::Context;
use std::{fs, path::Path, process::Command};

pub(super) const FAILURE_MARKER: &str = "FIFO_CREATION_OBSERVER_FAILURE";

const WORKER: &str = "adapters::codex_exec::anchor_fifo::tests::masks::observer::fault_worker";

fn failure_rejected(fault: &str) {
    let path = super::run_observed(0o022, WORKER, "fifo_observer_fault", Some(fault))
        .expect_err("observer failure was accepted as GREEN");
    let out = fs::read_to_string(path.join("stdout")).unwrap();
    let err = fs::read_to_string(path.join("stderr")).unwrap();
    if fault == "report" {
        assert_eq!(
            fs::read_to_string(path.join("exit-status")).unwrap(),
            "exit status: 86"
        );
        assert!(!err.contains(FAILURE_MARKER));
        assert!(!out.contains("1 passed"));
    } else {
        assert!(err.contains(FAILURE_MARKER));
        assert!(out.contains("1 passed; 0 failed"), "worker did not succeed");
    }
    // Valid creation evidence must not mask a later instrumentation failure.
    let log = path.join("creation.tsv");
    if fault != "incomplete" {
        verify_creation(&log);
    }
    assert!(fs::read_to_string(log).unwrap().contains("fifo\t"));
    println!("rejected_observer_fault={fault} evidence={path:?}");
}

macro_rules! failures {
    ($($name:ident => $fault:literal),+ $(,)?) => {$(
        #[test]
        fn $name() { failure_rejected($fault); }
    )+};
}
failures!(
    formatting_failure_rejected => "format",
    oversized_record_rejected => "oversized",
    log_open_failure_rejected => "open",
    log_write_failure_rejected => "write",
    incomplete_record_rejected => "incomplete",
    log_close_failure_rejected => "close",
    failure_reporting_exit_rejected => "report",
);

#[test]
fn interrupted_and_short_writes_complete() {
    let path = super::run_observed(0o022, WORKER, "fifo_observer_retry", Some("retry"))
        .expect("EINTR and short writes lost creation evidence");
    verify_creation(&path.join("creation.tsv"));
    let log = fs::read_to_string(path.join("creation.tsv")).unwrap();
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("fifo\t"))
            .count(),
        2
    );
}

#[test]
fn fault_worker() {
    if std::env::var("FIFO_MASK_CHILD").as_deref() != Ok("1") {
        return;
    }
    // First construction exercises the full original identity/mode/EOF oracle.
    super::production_worker();
    // Only the second FIFO observation is faulted. Production still succeeds.
    let pipe = super::super::super::new().expect("observer changed production result");
    drop(pipe);
}

#[test]
fn delegated_results_and_errno_are_preserved() {
    super::run(
        0o022,
        "adapters::codex_exec::anchor_fifo::tests::masks::observer::errno_worker",
        "fifo_observer_errno",
    );
}

#[test]
fn errno_worker() {
    if std::env::var("FIFO_MASK_CHILD").as_deref() != Ok("1") {
        return;
    }
    use std::{ffi::CString, os::unix::fs::PermissionsExt};
    type Create = unsafe extern "C" fn(*const libc::c_char, libc::mode_t) -> libc::c_int;
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    for (name, call) in [
        ("mkdir", libc::mkdir as Create),
        ("mkdirat", mkdir_at as Create),
        ("fifo", libc::mkfifo as Create),
    ] {
        let path = root.path().join(name);
        let path_c = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        unsafe {
            *libc::__errno_location() = libc::EDOM;
            assert_eq!(
                call(path_c.as_ptr(), if name == "fifo" { 0o600 } else { 0o700 }),
                0
            );
            assert_eq!(*libc::__errno_location(), libc::EDOM);
            *libc::__errno_location() = libc::EDOM;
            assert_eq!(call(path_c.as_ptr(), 0o700), -1);
            assert_eq!(*libc::__errno_location(), libc::EEXIST);
        }
        if name == "fifo" {
            fs::remove_file(path).unwrap();
        } else {
            fs::remove_dir(path).unwrap();
        }
    }
}

unsafe extern "C" fn mkdir_at(path: *const libc::c_char, mode: libc::mode_t) -> libc::c_int {
    unsafe { libc::mkdirat(libc::AT_FDCWD, path, mode) }
}

#[test]
fn compiler_failure_retains_diagnostics_before_worker_launch() {
    for name in ["observer.so", "lifecycle.so"] {
        let path =
            super::run_observed_with_setup(0o022, WORKER, "fifo_compile_failure", None, |root| {
                let source = root.join("invalid-fixture.c");
                fs::write(&source, "#error FIFO_COMPILER_RETENTION_CONTROL\n")?;
                Ok(compile(source.to_str().unwrap(), name, root)?
                    .display()
                    .to_string())
            })
            .expect_err("failed compiler was accepted as GREEN");
        assert!(path.is_dir(), "failed setup root was deleted");
        assert!(path.join(format!("{name}-compile.stdout")).is_file());
        let stderr = fs::read_to_string(path.join(format!("{name}-compile.stderr"))).unwrap();
        assert!(stderr.contains("FIFO_COMPILER_RETENTION_CONTROL"));
        let diagnostic = fs::read_to_string(path.join("setup-error")).unwrap();
        assert!(diagnostic.contains("fixture failed to compile"));
        assert!(diagnostic.contains("FIFO_COMPILER_RETENTION_CONTROL"));
        assert!(
            !path.join("creation.tsv").exists(),
            "worker setup continued after compiler failure"
        );
        assert!(
            !path.join("stdout").exists(),
            "worker launched after compiler failure"
        );
        println!("retained_failed_compile={name} evidence={path:?}");
    }
}

pub(super) fn compile(source: &str, name: &str, root: &Path) -> anyhow::Result<std::path::PathBuf> {
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
    let output = command
        .output()
        .with_context(|| format!("could not launch fixture compiler: {command:?}"))?;
    fs::write(root.join(format!("{name}-compile.stdout")), &output.stdout)
        .context("could not save compiler stdout")?;
    fs::write(root.join(format!("{name}-compile.stderr")), &output.stderr)
        .context("could not save compiler stderr")?;
    fs::write(
        root.join(format!("{name}-compile.command")),
        format!("{command:?}"),
    )
    .context("could not save compiler command")?;
    fs::write(
        root.join(format!("{name}-compile.status")),
        output.status.to_string(),
    )
    .context("could not save compiler status")?;
    println!(
        "fixture_compile command={command:?} exit={:?}",
        output.status.code()
    );
    anyhow::ensure!(
        output.status.success(),
        "fixture failed to compile: {command:?}; status={}; stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(library)
}

pub(super) fn verify_creation(log: &Path) {
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
