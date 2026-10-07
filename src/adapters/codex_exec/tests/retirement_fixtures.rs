//! Subprocess isolation for real signal dispositions and syscall fault gates.
use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

pub(in crate::adapters::codex_exec) fn run(case: &str) {
    run_case(
        case,
        "adapters::codex_exec::tests::retirement_fixtures::lifecycle_worker",
    );
}
pub(in crate::adapters::codex_exec) fn run_case(case: &str, worker: &str) {
    let root = if let Ok(path) = std::env::var("LIFECYCLE_ARTIFACT_ROOT") {
        tempfile::Builder::new()
            .prefix(case)
            .tempdir_in(path)
            .unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let supplied = std::env::var_os("LIFECYCLE_INTERPOSER");
    let library = supplied.map(std::path::PathBuf::from).unwrap_or_else(|| {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lifecycle_interpose.c");
        let library = root.path().join("interpose.so");
        let output = Command::new("/usr/bin/cc")
            .args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror", "-o"])
            .arg(&library)
            .arg(source)
            .arg("-ldl")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        library
    });
    let events = root.path().join("events");
    let stdout = fs::File::create(root.path().join("stdout")).unwrap();
    let stderr = fs::File::create(root.path().join("stderr")).unwrap();
    println!(
        "worker_command={{exe:{:?},selection:{worker},case:{case},root:{:?},events:{:?},interposer:{:?}}}",
        std::env::current_exe().unwrap(),
        root.path(),
        events,
        library
    );
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", worker, "--nocapture"])
        .env("LIFECYCLE_CASE", case)
        .env("LIFECYCLE_ROOT", root.path())
        .env("LIFECYCLE_EVENTS", &events)
        .env("LD_PRELOAD", library)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            let evidence = super::fixture_evidence::observe(
                root,
                case,
                "watchdog-killed/reaped",
                std::env::var_os("LIFECYCLE_ARTIFACT_ROOT").is_some(),
            );
            panic!("{case}: subprocess watchdog expired (not semantic RED); evidence={evidence:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let out = super::fixture_evidence::observe(
        root,
        case,
        status,
        std::env::var_os("LIFECYCLE_ARTIFACT_ROOT").is_some(),
    )
    .unwrap_or_else(|error| panic!("{error:#}"));
    assert!(out.contains("running 1 test"), "worker was not selected");
    assert!(status.success(), "{case}: semantic worker assertion failed");
}

pub(in crate::adapters::codex_exec) fn arm(
    signal: i32,
    boundary: i32,
    restore: i32,
    register: i32,
    group: i32,
) {
    unsafe {
        let symbol = libc::dlsym(libc::RTLD_DEFAULT, c"lifecycle_arm".as_ptr());
        assert!(!symbol.is_null(), "real syscall fixture must be loaded");
        let arm: unsafe extern "C" fn(i32, i32, i32, i32, i32) = std::mem::transmute(symbol);
        let entry = libc::dlsym(libc::RTLD_DEFAULT, c"lifecycle_observed_entry".as_ptr());
        assert!(!entry.is_null());
        ENTRY.store(entry as usize, std::sync::atomic::Ordering::SeqCst);
        arm(signal, boundary, restore, register, group);
    }
}

#[test]
fn lifecycle_worker() {
    let Ok(case) = std::env::var("LIFECYCLE_CASE") else {
        return;
    };
    if case.starts_with("anchor_") {
        super::anchor_lifecycle::worker(&case);
    } else if case.starts_with("race_") {
        super::retirement_concurrency::worker(&case);
    } else if case.starts_with("authority_") {
        super::group_authority::worker(&case);
    } else if case.starts_with("group_") {
        super::launcher_retirement::worker(&case);
    } else {
        super::retirement_signals::worker(&case);
    }
}

// A test-only gate immediately AFTER the production handler's observable publication.
// Resolve before installation; handler path performs only a static atomic load/call.
static ENTRY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub(in crate::adapters::codex_exec) fn observed_entry(signal: i32) {
    let entry = ENTRY.load(std::sync::atomic::Ordering::SeqCst);
    if entry != 0 {
        let callback: unsafe extern "C" fn(i32) = unsafe { std::mem::transmute(entry) };
        unsafe {
            callback(signal);
        }
    }
}
pub(super) fn release_inflight() {
    unsafe {
        let symbol = libc::dlsym(libc::RTLD_DEFAULT, c"lifecycle_finish_inflight".as_ptr());
        assert!(!symbol.is_null());
        let release: unsafe extern "C" fn() = std::mem::transmute(symbol);
        release();
    }
}
