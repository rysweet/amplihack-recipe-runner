use recipe_runner_rs::runner::MAX_STEP_OUTPUT_BYTES;
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::Command,
    time::{Duration, Instant},
};

pub(super) const LAUNCHER: &str = r#"#!/usr/bin/python3
import sys, os, json, pathlib, time, stat, subprocess, threading
root = pathlib.Path(os.environ['CODEX_TEST_ROOT'])
args = sys.argv[1:]
mode = os.environ['TEST_SCENARIO']
final = pathlib.Path(args[args.index('--output-last-message')+1]) if '--output-last-message' in args else None
record = {'args':args, 'final':str(final) if final else None, 'absent':not final.exists() if final else False,
          'directory_mode':stat.S_IMODE(final.parent.stat().st_mode) if final else None}
(root/'record.json').write_text(json.dumps(record))
with (root/'attempts.jsonl').open('a') as log: log.write(json.dumps(record)+'\n')
if mode == 'cancel_json':
    if len((root/'attempts.jsonl').read_text().splitlines()) == 1:
        sys.stdin.read()
        final.write_text('invalid JSON')
        sys.exit(0)
    mode = 'cancel'
if mode in ('tree_timeout', 'tree_success', 'cancel'):
    descendant = subprocess.Popen(['/usr/bin/python3', '-c', 'import time, signal; signal.signal(signal.SIGTERM, signal.SIG_IGN); print("ready", flush=True); time.sleep(30)'], stdout=subprocess.PIPE)
    descendant.stdout.readline()
    (root/'descendant.pid').write_text(str(descendant.pid))
    (root/'launcher.pid').write_text(str(os.getpid()))
    if mode in ('tree_timeout', 'cancel'): time.sleep(30)
if mode == 'writer':
    code = 'import sys, time, signal; signal.signal(signal.SIGTERM, signal.SIG_IGN); f=open(sys.argv[1], "wb", buffering=0); print("ready", flush=True)\nwhile True: f.write(b"x"); time.sleep(0.001)'
    descendant = subprocess.Popen(['/usr/bin/python3', '-c', code, str(final)], stdout=subprocess.PIPE)
    descendant.stdout.readline()
    (root/'descendant.pid').write_text(str(descendant.pid))
if mode == 'blocked': time.sleep(30)
if mode == 'partial':
    if final: final.write_text('false success')
    sys.exit(0)
data = sys.stdin.buffer.read() if final else b''
(root/'stdin').write_bytes(data)
print('PROGRESS_ONLY')
if mode == 'flood':
    def flood(fd):
        for _ in range(512): os.write(fd, b'x'*8192)
    threads = [threading.Thread(target=flood, args=(fd,)) for fd in (1,2)]
    for thread in threads: thread.start()
    for thread in threads: thread.join()
if mode == 'cleanup_failure':
    final.parent.rmdir()
    final.parent.write_text('injected resource obstruction')
    print('rate limit SECRET', file=sys.stderr)
    sys.exit(7)
if mode == 'retry' and len((root/'attempts.jsonl').read_text().splitlines()) == 1:
    print('rate limit', file=sys.stderr); sys.exit(1)
if final:
    if mode in ('missing', 'writer'): pass
    elif mode == 'directory': final.mkdir()
    elif mode == 'symlink':
        (root/'foreign').write_text('foreign'); final.symlink_to(root/'foreign')
    elif mode == 'fifo': os.mkfifo(final)
    elif mode == 'invalid': final.write_bytes(b'\xff')
    elif mode == 'empty': final.write_bytes(b'')
    elif mode in ('limit','oversize'): final.write_bytes(b'x'*(int(os.environ['TEST_OUTPUT_LIMIT'])+(mode=='oversize')))
    else: final.write_bytes(b'  FINAL\n\x00Unicode: \xce\xbb\n\n')
    if final.exists() and final.is_file() and mode != 'symlink':
        record['file_mode'] = stat.S_IMODE(final.stat().st_mode)
        (root/'record.json').write_text(json.dumps(record))
        if mode == 'unsafe_mode': final.chmod(0o644)
if mode == 'auth_failure':
    print('authentication rejected token=SECRET_TOKEN prompt=PRIVATE_PROMPT', file=sys.stderr)
sys.exit(7 if mode in ('nonzero', 'auth_failure') else 0)
"#;

pub(super) fn run(
    scenario: &str,
    prompt: &str,
    model: Option<&str>,
    provider: &str,
) -> (tempfile::TempDir, Value, Value) {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("prompt"), prompt).unwrap();
    let launcher = root.path().join("launcher");
    fs::write(&launcher, LAUNCHER).unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "adapter_worker", "--nocapture"])
        .env("CODEX_TEST_ROOT", root.path())
        .env("AMPLIHACK_LAUNCHER_BINARY", launcher)
        .env("AMPLIHACK_SESSION_DEPTH", "0")
        .env("AMPLIHACK_MAX_DEPTH", "10")
        .env("TEST_OUTPUT_LIMIT", MAX_STEP_OUTPUT_BYTES.to_string())
        .env("TEST_SCENARIO", scenario)
        .env("TEST_PROVIDER", provider)
        .env(
            "AMPLIHACK_RATELIMIT_MAX_RETRIES",
            if matches!(scenario, "retry" | "cleanup_failure") {
                "1"
            } else {
                "0"
            },
        )
        .env("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS", "0")
        .env("AMPLIHACK_RATELIMIT_MAX_DELAY_SECS", "0")
        .env(
            "AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL",
            if scenario == "retry" { "1" } else { "" },
        )
        .env_remove("TEST_MODEL");
    if let Some(model) = model {
        command.env("TEST_MODEL", model);
    }
    let mut child = command.spawn().unwrap();
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if start.elapsed() > Duration::from_secs(8) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("adapter exceeded watchdog");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let result =
        serde_json::from_slice(&fs::read(root.path().join("result.json")).unwrap()).unwrap();
    let record = fs::read(root.path().join("record.json"))
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
        .unwrap_or(Value::Null);
    (root, result, record)
}
