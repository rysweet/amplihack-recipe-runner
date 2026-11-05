use super::*;
use std::time::Instant;

// These variables are injected into a disposable test parent, avoiding any
// process-global mutation in the parallel test harness.
const AMBIENT_SENTINELS: [&str; 7] = [
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GH_TOKEN",
    "AMPLIHACK_AGENT_BINARY",
    "AMPLIHACK_BASH",
    "PYTHONPATH",
    "FIXTURE_AMBIENT_SENTINEL",
];

#[test]
fn ambient_credentials_and_overrides_do_not_reach_either_launch_path() {
    let mut probe = Command::new(std::env::current_exe().unwrap());
    probe.args([
        "--exact",
        "fixtures::environment_tests::environment_probe_worker",
        "--nocapture",
    ]);
    for name in AMBIENT_SENTINELS {
        probe.env(name, "harmless-fixture-sentinel");
    }
    let mut child = ChildGuard::new(probe.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "isolated environment probe failed");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("isolated environment probe exceeded watchdog");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn environment_probe_worker() {
    if std::env::var("FIXTURE_AMBIENT_SENTINEL").as_deref() != Ok("harmless-fixture-sentinel") {
        return;
    }
    for name in AMBIENT_SENTINELS {
        assert_eq!(
            std::env::var(name).as_deref(),
            Ok("harmless-fixture-sentinel")
        );
    }
    let (_, result, record) = run("success", "synthetic environment probe", None, "codex");
    assert!(result.get("ok").is_some(), "{result}");
    assert_eq!(record["ambient_present"], serde_json::json!([]));
    super::super::cancellation::assert_runner_environment_is_private();
}

/// Invoke a trusted interpreter directly. Executing a newly written shebang
/// script can delay interpreter entry beyond the one-second attempt on Darwin.
/// The adapter's provider argument selects a private Python source file.
pub(crate) fn install_launcher(root: &std::path::Path) -> std::path::PathBuf {
    for provider in ["codex", "claude", "copilot"] {
        let source = root.join(provider);
        fs::write(&source, supervised_python(LAUNCHER)).unwrap();
        fs::set_permissions(source, fs::Permissions::from_mode(0o600)).unwrap();
    }
    // Apple's /usr/bin/python3 dispatcher requires this basename.
    let launcher = root.join("python3");
    std::os::unix::fs::symlink("/usr/bin/python3", &launcher).unwrap();
    launcher
}

/// Request private permissions at creation, independently of ordinary umask.
pub(crate) fn private_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

// Each synthetic launcher owns its Popen handles before any record is trusted.
// A vanished spawning worker or the outer safety bound stops these children.
// The bound also covers a worker dying before the interpreter starts.
pub(crate) fn supervised_python(source: &str) -> String {
    format!("{SUPERVISOR}\n{source}")
}

const SUPERVISOR: &str = r#"
import os, pathlib, subprocess, threading, time
_release = pathlib.Path(os.environ["CODEX_TEST_ROOT"])/"supervisor-release.json"
_owner = os.getppid()
_children = []
_children_lock = threading.Lock()
def spawn_owned(*args, **kwargs):
    with _children_lock:
        child = subprocess.Popen(*args, **kwargs)
        _children.append(child)
        return child
def supervise():
    deadline = time.monotonic() + 15
    while os.getppid() == _owner and _owner > 1 and time.monotonic() < deadline:
        if _release.exists(): return
        time.sleep(0.005)
    if _release.exists(): return
    with _children_lock:
        for child in _children:
            # Popen retains the unreaped direct child, never a record-supplied PID.
            if child.poll() is None:
                child.kill()
                child.wait()
    os._exit(98)
threading.Thread(target=supervise, daemon=True).start()
"#;
