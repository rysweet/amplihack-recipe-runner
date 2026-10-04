//! Deterministic product-boundary tests. Fixtures are not live Codex proof.
#![cfg(unix)]
use recipe_runner_rs::adapters::{Adapter, cli_subprocess::CLISubprocessAdapter};
use recipe_runner_rs::runner::MAX_STEP_OUTPUT_BYTES;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::Command,
    time::{Duration, Instant},
};

// Run the adapter in a separate test process: no process-global environment races.
#[test]
fn adapter_worker() {
    let Ok(root) = std::env::var("CODEX_TEST_ROOT") else {
        return;
    };
    let prompt = fs::read_to_string(format!("{root}/prompt")).unwrap();
    let result = CLISubprocessAdapter::new()
        .with_binary(&std::env::var("TEST_PROVIDER").unwrap())
        .execute_agent_step(
            &prompt,
            Some("persona"),
            Some("PERSONA_SENTINEL"),
            Some("autonomous"),
            &root,
            std::env::var("TEST_MODEL").ok().as_deref(),
            Some(
                if std::env::var("TEST_SCENARIO").as_deref() == Ok("zero_timeout") {
                    0
                } else {
                    1
                },
            ),
        );
    let value = match result {
        Ok(s) => json!({"ok":s}),
        Err(e) => json!({"error":format!("{e:#}")}),
    };
    fs::write(format!("{root}/result.json"), value.to_string()).unwrap();
}

const LAUNCHER: &str = r#"#!/usr/bin/python3
import sys, os, json, pathlib, time, stat, subprocess
root = pathlib.Path(os.environ['CODEX_TEST_ROOT'])
args = sys.argv[1:]
mode = os.environ['TEST_SCENARIO']
final = pathlib.Path(args[args.index('--output-last-message')+1]) if '--output-last-message' in args else None
record = {'args':args, 'final':str(final) if final else None, 'absent':not final.exists() if final else False,
          'directory_mode':stat.S_IMODE(final.parent.stat().st_mode) if final else None}
(root/'record.json').write_text(json.dumps(record))
with (root/'attempts.jsonl').open('a') as log: log.write(json.dumps(record)+'\n')
if mode in ('tree_timeout', 'tree_success'):
    descendant = subprocess.Popen(['/usr/bin/python3', '-c', 'import time; time.sleep(30)'])
    (root/'descendant.pid').write_text(str(descendant.pid))
    if mode == 'tree_timeout': time.sleep(30)
if mode == 'blocked': time.sleep(30)
if mode == 'partial':
    if final: final.write_text('false success')
    sys.exit(0)
data = sys.stdin.buffer.read() if final else b''
(root/'stdin').write_bytes(data)
print('PROGRESS_ONLY')
if mode == 'retry' and len((root/'attempts.jsonl').read_text().splitlines()) == 1:
    print('rate limit', file=sys.stderr); sys.exit(1)
if final:
    if mode == 'missing': pass
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

fn run(
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
            if scenario == "retry" { "1" } else { "0" },
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
    let record = serde_json::from_slice(
        &fs::read(root.path().join("record.json"))
            .unwrap_or_else(|e| panic!("fixture did not execute: {e}; adapter result: {result}")),
    )
    .unwrap();
    (root, result, record)
}

fn assert_envelope(task: String) {
    let (root, result, record) = run("success", &task, None, "codex");
    assert_eq!(result["ok"], "  FINAL\n\0Unicode: λ\n\n");
    let args = record["args"].as_array().unwrap();
    assert_eq!(&args[..3], &[json!("codex"), json!("--"), json!("exec")]);
    assert_eq!(args.last().unwrap(), "-");
    for forbidden in ["-p", "--model", "--add-dir", "--system-prompt", "--json"] {
        assert!(!args.contains(&json!(forbidden)));
    }
    let input = fs::read_to_string(root.path().join("stdin")).unwrap();
    for text in [
        &task[..],
        "PERSONA_SENTINEL",
        "Do not invoke",
        "Proceed autonomously",
    ] {
        assert!(input.contains(text), "missing {text:?}");
    }
    assert_eq!(input.matches("Proceed autonomously").count(), 1);
    assert_eq!(record["absent"], true);
    assert_eq!(record["directory_mode"], 0o700);
    assert_eq!(record["file_mode"], 0o600);
    assert!(!std::path::Path::new(record["final"].as_str().unwrap()).exists());
}
#[test]
fn small_stdin_envelope() {
    assert_envelope("small task".into());
}
#[test]
fn leading_dash_unicode_multiline_nul_stdin_envelope() {
    assert_envelope("-leading\nλ\0END".into());
}
#[test]
fn large_stdin_envelope() {
    assert_envelope(format!("{}TAIL_SENTINEL", "λ".repeat(100_000)));
}

#[test]
fn deliberately_empty_final_succeeds() {
    let (_, result, _) = run("empty", "task", None, "codex");
    assert_eq!(result, json!({"ok":""}));
}

fn assert_failure(mode: &str) {
    let prompt = if matches!(mode, "partial" | "blocked") {
        "x".repeat(1_000_000)
    } else {
        "task".into()
    };
    let (_, result, record) = run(mode, &prompt, None, "codex");
    assert!(result.get("error").is_some(), "{mode}: {result}");
    assert_ne!(result["error"], "", "{mode} needs contextual diagnostics");
    if let Some(path) = record["final"].as_str() {
        assert!(!std::path::Path::new(path).exists(), "{mode} leaked output");
    }
}
macro_rules! failure_case {
    ($name:ident, $mode:literal) => {
        #[test]
        fn $name() {
            assert_failure($mode);
        }
    };
}
failure_case!(missing_final_fails, "missing");
failure_case!(nonzero_with_final_fails, "nonzero");
failure_case!(directory_final_fails, "directory");
failure_case!(symlink_final_fails, "symlink");
failure_case!(fifo_final_fails_without_blocking, "fifo");
failure_case!(unsafe_permissions_fail, "unsafe_mode");
failure_case!(invalid_utf8_fails, "invalid");
failure_case!(oversized_final_fails, "oversize");
failure_case!(partial_stdin_with_zero_exit_fails, "partial");
failure_case!(blocked_stdin_respects_timeout, "blocked");

#[test]
fn exact_existing_output_limit_is_verbatim() {
    let (_, result, _) = run("limit", "task", None, "codex");
    assert_eq!(result["ok"].as_str().unwrap().len(), 10_000_000);
}

#[test]
fn existing_claude_and_copilot_stdout_and_prompt_contracts_survive() {
    for provider in ["claude", "copilot"] {
        let (_, result, record) = run("success", "TASK", None, provider);
        assert_eq!(result["ok"], "PROGRESS_ONLY");
        let args = record["args"].as_array().unwrap();
        assert!(args.contains(&json!("-p")));
        assert!(!args.contains(&json!("exec")));
        assert!(args.contains(&json!("--add-dir")));
    }
}

#[test]
fn explicit_model_is_forwarded_without_an_implicit_default() {
    let (_, result, record) = run("success", "task", Some("caller-model"), "codex");
    assert!(result.get("ok").is_some());
    let args = record["args"].as_array().unwrap();
    let index = args
        .iter()
        .position(|arg| arg == "--model")
        .expect("explicit model flag");
    assert_eq!(args[index + 1], "caller-model");
}

#[test]
fn concurrent_attempts_have_distinct_private_paths() {
    let handles: Vec<_> = (0..4)
        .map(|_| std::thread::spawn(|| run("success", "task", None, "codex")))
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let mut paths = std::collections::HashSet::new();
    for (_, result, record) in &results {
        assert!(result.get("ok").is_some());
        assert!(paths.insert(record["final"].as_str().expect("final path")));
        assert_eq!(record["directory_mode"], 0o700);
    }
}

#[test]
fn rate_limit_retry_has_fresh_output_and_keeps_explicit_model() {
    let (root, result, _) = run("retry", "TASK_SENTINEL", Some("caller-model"), "codex");
    assert!(result.get("ok").is_some(), "{result}");
    let attempts: Vec<Value> = fs::read_to_string(root.path().join("attempts.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(attempts.len(), 2);
    assert_ne!(attempts[0]["final"], attempts[1]["final"]);
    for attempt in attempts {
        let args = attempt["args"].as_array().unwrap();
        assert!(!args.contains(&json!("auto")));
        assert!(args.contains(&json!("caller-model")));
    }
    let input = fs::read_to_string(root.path().join("stdin")).unwrap();
    assert!(
        input.contains("TASK_SENTINEL")
            && input.contains("PERSONA_SENTINEL")
            && input.contains("Do not invoke")
    );
}

fn assert_tree_cleanup(scenario: &str) {
    let (root, result, _) = run(scenario, "task", None, "codex");
    let pid: i32 = fs::read_to_string(root.path().join("descendant.pid"))
        .unwrap()
        .parse()
        .unwrap();
    // Linux zombies have exited and cannot retain stdin or write output.
    let is_running = || {
        fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .is_some_and(|stat| {
                !stat
                    .split_whitespace()
                    .nth(2)
                    .is_some_and(|state| state == "Z")
            })
    };
    let deadline = Instant::now() + Duration::from_millis(500);
    while is_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let leaked = is_running();
    // Clean our fixture even when the baseline fails this assertion.
    if leaked {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    assert!(!leaked, "{scenario}: descendant survived adapter return");
    assert_eq!(result.get("error").is_some(), scenario == "tree_timeout");
}
#[test]
fn timeout_terminates_owned_descendants() {
    assert_tree_cleanup("tree_timeout");
}
#[test]
fn nominal_exit_terminates_lingering_descendants() {
    assert_tree_cleanup("tree_success");
}

#[test]
fn failure_diagnostics_are_actionable_and_redacted() {
    let (_, result, _) = run("auth_failure", "PRIVATE_PROMPT", None, "codex");
    let error = result["error"].as_str().unwrap();
    assert!(error.contains("authentication rejected"));
    assert!(error.contains("check Codex login"));
    assert!(!error.contains("SECRET_TOKEN"));
    assert!(!error.contains("PRIVATE_PROMPT"));
}
