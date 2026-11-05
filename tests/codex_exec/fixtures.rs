use super::process_observation::{ChildGuard, SpawnedWorker, finish_observed, watch_ready};
pub(super) use environment_tests::{install_launcher, private_tempdir, supervised_python};
use recipe_runner_rs::runner::MAX_STEP_OUTPUT_BYTES;
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, process::Command, time::Duration};

pub(super) const LAUNCHER: &str = r#"#!/usr/bin/python3
import sys, os, json, pathlib, time, stat, subprocess, threading
root = pathlib.Path(os.environ['CODEX_TEST_ROOT'])
# The provider is the interpreter's script argument. Restore the launcher argv
# contract rather than exposing Python's invocation details to the assertions.
args = [os.path.basename(sys.argv[0])] + sys.argv[1:]
mode = os.environ['TEST_SCENARIO']
if mode not in {
    'success', 'git_checkout', 'trust_failure', 'trust_failure_early',
    'cancel_json', 'cancel', 'tree_timeout', 'tree_success', 'writer',
    'blocked', 'partial', 'flood', 'cleanup_failure', 'retry', 'missing',
    'directory', 'symlink', 'fifo', 'invalid', 'empty', 'limit', 'oversize',
    'unsafe_mode', 'auth_failure', 'nonzero',
}: raise ValueError('unknown synthetic fixture scenario')
scenario = mode
attempt_id = str(os.getpid()) + '-' + str(time.monotonic_ns())
def publish(name, text):
    tmp = root/(name+'.tmp')
    with tmp.open('x') as stream:
        os.chmod(tmp, 0o600)
        stream.write(text)
    os.replace(tmp, root/name)
def completed(operation):
    publish('scenario-'+attempt_id+'.json', json.dumps({
        'schema_version':1, 'attempt_id':attempt_id,
        'scenario':scenario, 'operation':operation}))
def ready(descendant):
    publish('descendant.pid', str(descendant.pid))
    publish('launcher.pid', str(os.getpid()))
    # The worker observes a known live descendant before this launcher may exit.
    # This handshake is inside the unchanged adapter deadline. Cancellation
    # fixtures run the real runner and retain their existing readiness contract.
    if os.environ.get('CODEX_TEST_OBSERVE_DESCENDANT') == '1':
        while not (root/'descendant-live.json').exists(): time.sleep(0.001)
final = pathlib.Path(args[args.index('--output-last-message')+1]) if '--output-last-message' in args else None
record = {'attempt_id':attempt_id, 'args':args, 'cwd':str(pathlib.Path.cwd()), 'final':str(final) if final else None, 'absent':not final.exists() if final else False,
          'directory_mode':stat.S_IMODE(final.parent.stat().st_mode) if final else None}
record['ambient_present'] = [name for name in ('OPENAI_API_KEY', 'ANTHROPIC_API_KEY', 'GH_TOKEN', 'AMPLIHACK_AGENT_BINARY', 'AMPLIHACK_BASH', 'PYTHONPATH', 'FIXTURE_AMBIENT_SENTINEL') if name in os.environ and (name != 'AMPLIHACK_AGENT_BINARY' or os.environ[name] != args[0])]
(root/'record.json').write_text(json.dumps(record))
with (root/'attempts.jsonl').open('a') as log: log.write(json.dumps(record)+'\n')
def reject_trusted_directory():
    print('Not inside a trusted directory and --skip-git-repo-check was not specified.', file=sys.stderr)
    print('authentication network rate limit token=SECRET_TOKEN prompt=PRIVATE_PROMPT /synthetic/private/PATH_CANARY', file=sys.stderr)
    sys.exit(7)
if mode == 'trust_failure_early':
    final.write_text('false success')
    reject_trusted_directory()
if mode == 'cancel_json':
    if len((root/'attempts.jsonl').read_text().splitlines()) == 1:
        sys.stdin.read()
        final.write_text('invalid JSON')
        sys.exit(0)
    mode = 'cancel'
if mode in ('tree_timeout', 'tree_success', 'cancel'):
    descendant = spawn_owned(['/usr/bin/python3', '-c', 'import time, signal; signal.signal(signal.SIGTERM, signal.SIG_IGN); print("ready", flush=True); time.sleep(30)'], stdout=subprocess.PIPE)
    descendant.stdout.readline()
    ready(descendant)
    if mode in ('tree_timeout', 'cancel'): time.sleep(30)
if mode == 'writer':
    code = 'import sys, time, signal; signal.signal(signal.SIGTERM, signal.SIG_IGN); f=open(sys.argv[1], "wb", buffering=0); print("ready", flush=True)\nwhile True: f.write(b"x"); time.sleep(0.001)'
    descendant = spawn_owned(['/usr/bin/python3', '-c', code, str(final)], stdout=subprocess.PIPE)
    descendant.stdout.readline()
    ready(descendant)
if mode == 'blocked':
    completed('blocking_without_stdin_read')
    time.sleep(30)
if mode == 'partial':
    if final: final.write_text('false success')
    completed('exit_before_stdin_read')
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
    completed('rate_limit_exit_selected')
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
if mode == 'trust_failure': reject_trusted_directory()
completed({
    'missing':'final_deliberately_omitted', 'directory':'directory_created',
    'symlink':'symlink_created', 'fifo':'fifo_created',
    'invalid':'invalid_utf8_written', 'oversize':'oversized_final_written',
    'unsafe_mode':'unsafe_permissions_applied', 'nonzero':'nonzero_exit_selected',
}.get(mode, 'final_written'))
sys.exit(7 if mode in ('nonzero', 'auth_failure') else 0)
"#;

// This assertion specifies the launcher contract. The launcher must publish the
// receipt after the selected operation, rather than merely after process entry.
pub(super) fn assert_scenario_completed(
    root: &std::path::Path,
    scenario: &str,
    operation: &str,
    attempt: &Value,
) -> String {
    let id = attempt["attempt_id"]
        .as_str()
        .expect("launcher must publish an attempt identifier");
    assert!(
        !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "attempt identifier must be a single private filename component"
    );
    let name = format!("scenario-{id}.json");
    let path = root.join(&name);
    let metadata = fs::symlink_metadata(&path).expect("scenario completion receipt is required");
    assert!(
        metadata.file_type().is_file(),
        "receipt must be a regular file"
    );
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    let bytes = fs::read(&path).unwrap();
    let receipt: Value = serde_json::from_slice(&bytes).expect("complete JSON receipt");
    assert_eq!(receipt["schema_version"], 1);
    assert_eq!(receipt["attempt_id"], id);
    assert_eq!(receipt["scenario"], scenario);
    assert_eq!(receipt["operation"], operation);
    // Receipts have a fixed public shape. Prompts, stdin and diagnostics belong
    // in neither their contents nor the failure messages from these assertions.
    assert_eq!(receipt.as_object().unwrap().len(), 4);
    assert!(!root.join(format!("{name}.tmp")).exists());
    name
}

/// Both launch paths clear the environment, use trusted system executables and
/// private writable paths. Synthetic fixtures need no ambient configuration.
pub(super) fn fixture_environment<'a>(
    command: &'a mut Command,
    root: &std::path::Path,
) -> &'a mut Command {
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("HOME", root)
        .env("TMPDIR", root)
        .env("CODEX_HOME", root.join("codex-home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("PYTHONNOUSERSITE", "1")
        .env("CODEX_TEST_ROOT", root)
        .env("AMPLIHACK_SESSION_DEPTH", "0")
        .env("AMPLIHACK_MAX_DEPTH", "10")
        .env("AMPLIHACK_RATELIMIT_MAX_RETRIES", "0")
        .env("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS", "0")
        .env("AMPLIHACK_RATELIMIT_MAX_DELAY_SECS", "0")
}

pub(super) fn run(
    scenario: &str,
    prompt: &str,
    model: Option<&str>,
    provider: &str,
) -> (tempfile::TempDir, Value, Value) {
    let root = private_tempdir();
    if scenario == "git_checkout" {
        assert!(
            Command::new("/usr/bin/git")
                .args(["init", "--quiet"])
                .arg(root.path())
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", root.path())
                .status()
                .unwrap()
                .success()
        );
    }
    fs::write(root.path().join("prompt"), prompt).unwrap();
    let launcher = install_launcher(root.path());
    let mut command = Command::new(std::env::current_exe().unwrap());
    fixture_environment(&mut command, root.path())
        .args(["--exact", "adapter_worker", "--nocapture"])
        .env("AMPLIHACK_LAUNCHER_BINARY", launcher)
        .env("TEST_OUTPUT_LIMIT", MAX_STEP_OUTPUT_BYTES.to_string())
        .env("TEST_SCENARIO", scenario)
        .env("TEST_PROVIDER", provider)
        .env("CODEX_TEST_OBSERVE_DESCENDANT", "1")
        .env(
            "AMPLIHACK_RATELIMIT_MAX_RETRIES",
            if matches!(
                scenario,
                "retry" | "cleanup_failure" | "trust_failure" | "trust_failure_early"
            ) {
                "1"
            } else {
                "0"
            },
        )
        .env(
            "AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL",
            if scenario == "retry" { "1" } else { "" },
        );
    if let Some(model) = model {
        command.env("TEST_MODEL", model);
    }
    let mut child = ChildGuard::new(command.spawn().unwrap());
    let observes_descendant = matches!(scenario, "tree_timeout" | "tree_success" | "writer");
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observer = observes_descendant.then(|| {
        let worker = SpawnedWorker::capture(child.id()).expect("spawned worker identity");
        watch_ready(root.path(), worker, done.clone())
    });
    let status = finish_observed(
        &mut child,
        root.path(),
        done,
        observer,
        Duration::from_secs(8),
    )
    .expect("adapter fixture observation failed");
    assert!(status.success());
    let result =
        serde_json::from_slice(&fs::read(root.path().join("result.json")).unwrap()).unwrap();
    let record = match fs::read(root.path().join("record.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).expect("valid launcher entry record"),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && scenario == "zero_timeout" =>
        {
            Value::Null
        }
        Err(error) => panic!("launcher entry record is required for {scenario}: {error}"),
    };
    (root, result, record)
}

#[path = "fixtures/environment_tests.rs"]
mod environment_tests;
