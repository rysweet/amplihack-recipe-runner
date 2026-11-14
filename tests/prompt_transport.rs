//! #205 contract tests against live Rust wrapper stubs (Linux /proc).
//! All synchronization uses socket acknowledgments, stdin EOF, or child wait.
//! Small and large prompts share the same private transport.
#![cfg(target_os = "linux")]

#[path = "support/prompt_transport_worker.rs"]
mod worker;

use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::fd::AsRawFd,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::OnceLock,
    time::Duration,
};

const FOOTER: &str = "\n\nIMPORTANT: Proceed autonomously. Do not ask questions. Make reasonable decisions and continue.";
const NO_REENTRY: &str = "You are already inside an active recipe-managed workflow step. This is not a new top-level user request. Do not invoke /dev, dev-orchestrator, ultrathink, smart-orchestrator, or any other workflow or recipe runner. Execute the requested step directly and return only the output requested by this step.";
const TASK: &str = " \t--TASK_SECRET_205 λ🦀\n\nKeep\ttabs.\n \t";
const SYSTEM: &str = " \tSYSTEM_SECRET_205 漢字\n\nKeep persona whitespace.\n \t";

#[test]
#[ignore = "isolated adapter subprocess, invoked by this suite"]
fn prompt_transport_worker() {
    worker::run(Path::new(&std::env::var_os("PROMPT_TEST_ROOT").unwrap()));
}

fn launcher_bytes() -> &'static [u8] {
    static BINARY: OnceLock<Vec<u8>> = OnceLock::new();
    BINARY.get_or_init(|| {
        let build = tempfile::tempdir().unwrap();
        let output = build.path().join("launcher");
        let result = Command::new("rustc")
            .arg("--edition=2024")
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/support/prompt_transport_launcher.rs"),
            )
            .arg("-o")
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        fs::read(output).unwrap()
    })
}

fn receive(wire: &mut BufReader<UnixStream>) -> String {
    let mut text = String::new();
    assert_ne!(
        wire.read_line(&mut text).unwrap(),
        0,
        "fixture disconnected before acknowledgment"
    );
    text.trim_end().to_owned()
}

fn accept(listener: &UnixListener) -> BufReader<UnixStream> {
    let mut ready = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert!(
        unsafe { libc::poll(&mut ready, 1, 15_000) } > 0,
        "fixture connection deadline exceeded"
    );
    let stream = listener.accept().unwrap().0;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    BufReader::new(stream)
}

fn send(wire: &mut BufReader<UnixStream>, text: &str) {
    writeln!(wire.get_mut(), "{text}").unwrap();
}

fn assert_payload(actual: &[u8], expected: &[u8]) {
    // Byte-exact comparison without dumping arbitrarily large instructions.
    assert!(
        actual == expected,
        "payload differs at byte {:?}; received {} bytes, expected {} bytes",
        actual.iter().zip(expected).position(|(a, b)| a != b),
        actual.len(),
        expected.len()
    );
}

struct OwnedWorker(Child);
impl Drop for OwnedWorker {
    fn drop(&mut self) {
        // Control sockets close first. The stub exits on disconnection and
        // its adapter owner retires it before we reap this recorded child.
        let _ = self.0.wait();
    }
}

struct Fixture {
    // Declared first so controller disconnection releases the wrapper on unwind.
    agent: Option<BufReader<UnixStream>>,
    worker_wire: BufReader<UnixStream>,
    worker: OwnedWorker,
    root: tempfile::TempDir,
    provider: String,
    task: String,
    system: Option<String>,
    wrapper_pid: Option<u32>,
    args: Vec<String>,
    prompt_files: Vec<PathBuf>,
    live_system: Option<Vec<u8>>,
    modes: Vec<(u32, u32)>,
}

impl Fixture {
    fn start(provider: &str, task: &str, system: Option<&str>, mut config: Value) -> Self {
        let root = tempfile::Builder::new().prefix("pt-").tempdir().unwrap();
        for dir in ["scratch", "work/nested"] {
            fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        fs::write(root.path().join("task"), task).unwrap();
        if let Some(system) = system {
            fs::write(root.path().join("system"), system).unwrap();
        }
        fs::write(
            root.path().join("scenario"),
            config["scenario"].as_str().unwrap_or("success"),
        )
        .unwrap();
        config["provider"] = json!(provider);
        fs::write(root.path().join("config.json"), config.to_string()).unwrap();
        let launcher = root.path().join("launcher");
        fs::write(&launcher, launcher_bytes()).unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
        let workers = UnixListener::bind(root.path().join("worker.sock")).unwrap();
        let agents = UnixListener::bind(root.path().join("agent.sock")).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "prompt_transport_worker",
                "--ignored",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", root.path())
            .env("TMPDIR", root.path().join("scratch"))
            .env("PROMPT_TEST_ROOT", root.path())
            .env("AMPLIHACK_SESSION_DEPTH", "0")
            .env("AMPLIHACK_MAX_DEPTH", "10")
            .env("AMPLIHACK_RATELIMIT_MAX_RETRIES", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(root.path().join("worker.stderr")).unwrap())
            .spawn()
            .unwrap();
        let worker = OwnedWorker(child);
        let mut worker_wire = accept(&workers);
        assert_eq!(receive(&mut worker_wire), "WORKER_STARTED");
        // Startup-error and zero-timeout cases must return without readiness.
        let expect_wrapper =
            config["startup"].is_null() && (config["timeout"] != 0 || config["report"] == true);
        let (agent, args, wrapper_pid) = if expect_wrapper {
            // Wait for either explicit readiness or an early adapter result.
            // A deadline bounds fixture failure; readiness establishes ordering.
            let mut ready = [
                libc::pollfd {
                    fd: agents.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: worker_wire.get_ref().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            assert!(unsafe { libc::poll(ready.as_mut_ptr(), ready.len() as _, 15_000) } > 0);
            assert_eq!(
                ready[1].revents,
                0,
                "adapter returned before wrapper readiness: {}",
                if ready[1].revents != 0 {
                    receive(&mut worker_wire)
                } else {
                    String::new()
                }
            );
            let mut wire = accept(&agents);
            let started = receive(&mut wire);
            let pid: u32 = started.strip_prefix("STARTED ").unwrap().parse().unwrap();
            // Inspect the actual, acknowledged wrapper, before it reads stdin.
            let cmdline = fs::read(format!("/proc/{pid}/cmdline")).unwrap();
            let args = cmdline
                .split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
                .collect();
            (Some(wire), args, Some(pid))
        } else {
            (None, Vec::new(), None)
        };
        let mut fixture = Self {
            root,
            worker,
            worker_wire,
            agent,
            provider: provider.into(),
            task: task.into(),
            system: system.map(str::to_owned),
            wrapper_pid,
            args,
            prompt_files: Vec::new(),
            live_system: None,
            modes: Vec::new(),
        };
        fixture.snapshot_prompt_files();
        fixture
    }

    fn snapshot_prompt_files(&mut self) {
        for pair in self.args.windows(2) {
            if matches!(
                pair[0].as_str(),
                "--system-prompt-file" | "--append-system-prompt-file"
            ) {
                self.prompt_files.push(PathBuf::from(&pair[1]));
            }
            if self.provider == "copilot" && pair[0] == "--add-dir" {
                for entry in fs::read_dir(&pair[1]).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_file()
                        && fs::read(&path)
                            .unwrap()
                            .windows(NO_REENTRY.len())
                            .any(|bytes| bytes == NO_REENTRY.as_bytes())
                    {
                        self.prompt_files.push(path);
                    }
                }
            }
        }
        self.prompt_files.sort();
        self.prompt_files.dedup();
        for path in &self.prompt_files {
            self.modes.push((
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
            ));
        }
        self.live_system = self
            .prompt_files
            .first()
            .map(|path| fs::read(path).unwrap());
    }

    fn read_input(&mut self) {
        let wire = self.agent.as_mut().unwrap();
        send(wire, "READ");
        assert_eq!(receive(wire), "INPUT_RECEIVED");
    }

    fn release(&mut self) {
        if let Some(mut wire) = self.agent.take() {
            // A correctly cancelled wrapper has already closed its end.
            let _ = writeln!(wire.get_mut(), "RELEASE");
        }
    }

    fn finish(&mut self) -> Value {
        self.read_input();
        self.release();
        self.result(None)
    }

    fn result(&mut self, first: Option<String>) -> Value {
        let line = first.unwrap_or_else(|| receive(&mut self.worker_wire));
        let value: Value = serde_json::from_str(line.strip_prefix("RESULT ").unwrap()).unwrap();
        let status = self.worker.0.wait().unwrap();
        assert!(status.success(), "{}", self.stderr());
        assert_eq!(
            value["signals_restored"], true,
            "prior signal handlers changed"
        );
        if let Some(pid) = self.wrapper_pid {
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "the recorded wrapper must be retired and reaped before adapter return"
            );
        }
        value
    }

    fn stderr(&self) -> String {
        fs::read_to_string(self.root.path().join("worker.stderr")).unwrap()
    }

    fn input(&self) -> Vec<u8> {
        fs::read(self.root.path().join("stdin.received")).unwrap()
    }

    fn assert_secrecy(&self) {
        for arg in &self.args {
            assert!(
                !arg.contains("TASK_SECRET_205"),
                "task text in live wrapper argv"
            );
            assert!(
                !arg.contains("SYSTEM_SECRET_205"),
                "system text in live wrapper argv"
            );
        }
        for text in [&self.task[..], self.system.as_deref().unwrap_or("")] {
            if !text.is_empty() {
                assert!(
                    !self.args.iter().any(|arg| arg.contains(text)),
                    "supplied input in argv"
                );
            }
        }
    }

    fn assert_cleaned(&self) {
        for path in &self.prompt_files {
            assert!(
                !path.exists(),
                "prompt file survived adapter return: {}",
                path.display()
            );
            assert!(
                !path.parent().unwrap().exists(),
                "prompt directory survived adapter return"
            );
        }
        // Also inspect scratch and retained logs: no unadvertised prompt copy.
        fn inspect(dir: &Path, task: &[u8], system: &[u8]) {
            if !dir.exists() {
                return;
            }
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    inspect(&path, task, system);
                } else {
                    let bytes = fs::read(&path).unwrap();
                    for secret in [task, system] {
                        if !secret.is_empty() {
                            assert!(
                                !bytes.windows(secret.len()).any(|part| part == secret),
                                "prompt copy survived in {}",
                                path.display()
                            );
                        }
                    }
                }
            }
        }
        inspect(
            &self.root.path().join("scratch"),
            self.task.as_bytes(),
            self.system.as_deref().unwrap_or("").as_bytes(),
        );
        inspect(
            &self.root.path().join("logs"),
            self.task.as_bytes(),
            self.system.as_deref().unwrap_or("").as_bytes(),
        );
    }

    fn assert_delivery(&self, result: &Value) {
        assert_eq!(
            result["ok"],
            if self.provider == "codex" {
                "STUB_FINAL"
            } else {
                "STUB_STDOUT"
            },
            "{result}"
        );
        let effective = match &self.system {
            Some(system) if !system.is_empty() => format!("{system}\n\n{NO_REENTRY}"),
            _ => NO_REENTRY.to_owned(),
        };
        let task = format!("{}{FOOTER}", self.task);
        if self.provider == "codex" {
            assert_payload(
                &self.input(),
                format!("# System instructions\n{effective}\n\n# Task instructions\n{task}")
                    .as_bytes(),
            );
            assert!(
                self.prompt_files.is_empty(),
                "Codex stdin needs no prompt file"
            );
            assert_eq!(self.args.last().unwrap(), "-");
            assert!(
                self.args
                    .windows(3)
                    .any(|args| args == ["codex", "--", "exec"])
            );
        } else {
            assert_eq!(
                self.prompt_files.len(),
                1,
                "one step-owned prompt file required"
            );
            assert_eq!(
                self.modes,
                [(0o600, 0o700)],
                "private file and directory modes while live"
            );
            let payload = self.live_system.as_ref().unwrap();
            assert_payload(
                &fs::read(self.root.path().join("system.received")).unwrap(),
                payload,
            );
            if self.provider == "copilot" {
                assert_payload(
                    payload,
                    format!("# System instructions\n{effective}\n\n# Task instructions\n{task}")
                        .as_bytes(),
                );
                let prompt = self.args.windows(2).find(|pair| pair[0] == "-p").unwrap();
                let path = &self.prompt_files[0];
                assert!(path.is_absolute());
                assert!(
                    prompt[1].contains(path.to_str().unwrap()),
                    "pointer must name its file"
                );
                assert!(
                    self.args.windows(2).any(|pair| pair[0] == "--add-dir"
                        && Path::new(&pair[1]) == path.parent().unwrap())
                );
            } else {
                assert_payload(payload, effective.as_bytes());
                assert_payload(&self.input(), task.as_bytes());
                assert!(self.args.contains(&"--system-prompt-file".into()));
                let p = self.args.iter().position(|arg| arg == "-p").unwrap();
                assert!(
                    self.args.get(p + 1).is_none_or(|arg| arg.starts_with("--")),
                    "-p must have no value"
                );
            }
            for obsolete in [
                "--system-prompt",
                "--append-system-prompt",
                "--append-system-prompt-file",
            ] {
                assert!(!self.args.iter().any(|arg| arg == obsolete));
            }
        }
        assert!(
            self.args
                .windows(2)
                .any(|pair| pair == ["--model", "fixture-model"])
        );
        assert_eq!(
            fs::read(self.root.path().join("wrapper.cwd")).unwrap(),
            self.root
                .path()
                .join("work/nested")
                .as_os_str()
                .as_encoded_bytes()
        );
    }
}

fn exercise(provider: &str, task: &str, system: Option<&str>) {
    let mut fixture = Fixture::start(provider, task, system, json!({}));
    let result = fixture.finish();
    fixture.assert_cleaned();
    fixture.assert_secrecy();
    fixture.assert_delivery(&result);
}

#[test]
fn claude_small_prompt_is_stdin_and_a_private_system_file() {
    exercise("claude", TASK, Some(SYSTEM));
}
#[test]
fn codex_small_prompt_is_a_complete_stdin_envelope() {
    exercise("codex", TASK, Some(SYSTEM));
}

#[test]
fn codex_existing_stdin_transport_remains_compatible() {
    exercise("codex", "TASK_SECRET_205", Some("SYSTEM_SECRET_205"));
}
#[test]
fn copilot_small_prompt_pointer_includes_its_persona() {
    exercise("copilot", TASK, Some(SYSTEM));
}

#[test]
fn prompts_across_former_preview_and_inline_limits_remain_byte_exact() {
    for provider in ["claude", "codex", "copilot"] {
        for size in [8191, 8192, 8193, 32767, 32768, 32769, 131073] {
            let task = format!("{TASK}{}{TASK}", "x".repeat(size - 2 * TASK.len()));
            exercise(provider, &task, Some(SYSTEM));
            let system = format!("{SYSTEM}{}{SYSTEM}", "x".repeat(size - 2 * SYSTEM.len()));
            exercise(provider, TASK, Some(&system));
        }
    }
}

#[test]
fn empty_absent_and_whitespace_only_inputs_are_preserved() {
    for provider in ["claude", "codex", "copilot"] {
        for system in [None, Some(""), Some(" \t\n\n \t")] {
            exercise(provider, "", system);
        }
    }
}

#[test]
fn claude_compatible_aliases_use_the_same_private_transport() {
    for provider in ["launch", "RustyClawd"] {
        exercise(provider, TASK, Some(SYSTEM));
    }
}

#[test]
fn nonzero_exit_removes_private_resources_and_preserves_the_failure() {
    for provider in ["claude", "codex", "copilot"] {
        let mut fixture =
            Fixture::start(provider, TASK, Some(SYSTEM), json!({"scenario": "nonzero"}));
        let result = fixture.finish();
        assert!(
            result["error"]
                .as_str()
                .is_some_and(|error| error.contains("7")),
            "{result}"
        );
        fixture.assert_cleaned();
        fixture.assert_secrecy();
    }
}

#[test]
fn retained_logs_do_not_retain_prompt_files() {
    for provider in ["claude", "copilot"] {
        for scenario in ["success", "nonzero"] {
            let mut fixture = Fixture::start(
                provider,
                TASK,
                Some(SYSTEM),
                json!({"logs": true, "scenario": scenario}),
            );
            let result = fixture.finish();
            fixture.assert_cleaned();
            fixture.assert_secrecy();
            let logs: String = fs::read_dir(fixture.root.path().join("logs"))
                .unwrap()
                .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
                .collect();
            assert!(logs.contains("STUB_STDOUT") && logs.contains("STUB_STDERR"));
            assert_eq!(result.get("ok").is_some(), scenario == "success");
        }
    }
}

#[test]
fn command_construction_and_spawn_failures_remove_prepared_prompts() {
    for provider in ["claude", "codex", "copilot"] {
        for startup in ["relative", "missing"] {
            let mut fixture =
                Fixture::start(provider, TASK, Some(SYSTEM), json!({"startup": startup}));
            let result = fixture.result(None);
            assert!(result["error"].is_string(), "{result}");
            fixture.assert_cleaned();
        }
    }
}

#[test]
fn successful_child_exit_before_complete_stdin_is_a_delivery_failure() {
    for provider in ["claude", "codex"] {
        let task = format!("{TASK}{}", "x".repeat(2 * 1024 * 1024));
        let mut fixture = Fixture::start(
            provider,
            &task,
            Some(SYSTEM),
            json!({"scenario": "early_zero"}),
        );
        let result = fixture.finish();
        assert!(
            result["error"].is_string(),
            "incomplete delivery accepted: {result}"
        );
        fixture.assert_cleaned();
        fixture.assert_secrecy();
    }
}

#[test]
fn zero_execution_deadline_removes_prompt_resources() {
    for provider in ["claude", "codex", "copilot"] {
        let mut fixture = Fixture::start(provider, TASK, Some(SYSTEM), json!({"timeout": 0}));
        let result = fixture.result(None);
        assert!(result["error"].is_string(), "{result}");
        fixture.assert_cleaned();
    }
}

#[test]
fn report_only_deadline_preserves_resources_until_explicit_release() {
    for provider in ["claude", "codex", "copilot"] {
        let mut fixture = Fixture::start(
            provider,
            TASK,
            Some(SYSTEM),
            json!({"timeout": 0, "report": true}),
        );
        let result = fixture.finish();
        fixture.assert_cleaned();
        fixture.assert_secrecy();
        fixture.assert_delivery(&result);
        assert_eq!(
            fixture
                .stderr()
                .matches(if provider == "codex" {
                    "does not apply to the Codex adapter"
                } else {
                    "exceeded its 0s timeout"
                })
                .count(),
            1
        );
    }
}

#[test]
fn copilot_prompt_access_survives_working_directory_and_tool_opt_outs() {
    let mut fixture = Fixture::start(
        "copilot",
        TASK,
        Some(SYSTEM),
        json!({"no_add_dir": true, "no_allow_tools": true}),
    );
    let result = fixture.finish();
    fixture.assert_cleaned();
    fixture.assert_secrecy();
    fixture.assert_delivery(&result);
    assert!(!fixture.args.contains(&"--allow-all-tools".into()));
    let grants: Vec<_> = fixture
        .args
        .windows(2)
        .filter(|pair| pair[0] == "--add-dir")
        .map(|pair| PathBuf::from(&pair[1]))
        .collect();
    assert_eq!(grants, [fixture.prompt_files[0].parent().unwrap()]);
}

#[test]
fn claude_signals_cancel_delivery_and_stop_nonfatal_recipes() {
    assert_signal_contract("claude");
}

#[test]
fn codex_signals_cancel_delivery_and_stop_nonfatal_recipes() {
    assert_signal_contract("codex");
}

#[test]
fn copilot_signals_remove_prompt_files_and_stop_nonfatal_recipes() {
    assert_signal_contract("copilot");
}

fn backend_running(pid: u32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    !matches!(
        stat.rsplit_once(')').unwrap().1.split_whitespace().next(),
        Some("Z" | "X")
    )
}

#[test]
fn descendant_backends_retire_before_prompt_cleanup_on_both_signals() {
    for provider in ["claude", "copilot"] {
        for signal in [libc::SIGINT, libc::SIGTERM] {
            for scenario in [
                "descendant_graceful",
                "descendant_ignore",
                "descendant_orphan",
            ] {
                let mut fixture =
                    Fixture::start(provider, TASK, Some(SYSTEM), json!({"scenario": scenario}));
                let listener =
                    UnixListener::bind(fixture.root.path().join("backend.sock")).unwrap();
                let wrapper = fixture.agent.as_mut().unwrap();
                send(wrapper, "READ");
                let launched = receive(wrapper);
                let pid: u32 = launched
                    .strip_prefix("BACKEND_LAUNCHED ")
                    .unwrap()
                    .parse()
                    .unwrap();
                let mut backend = accept(&listener);
                assert_eq!(receive(&mut backend), format!("BACKEND_STARTED {pid}"));
                send(&mut backend, "READ");
                assert_eq!(receive(&mut backend), "INPUT_RECEIVED");
                // Backend receipt, rather than a filesystem snapshot, proves delivery.
                assert_payload(
                    &fs::read(fixture.root.path().join("system.received")).unwrap(),
                    fixture.live_system.as_ref().unwrap(),
                );
                if provider == "claude" {
                    assert_payload(&fixture.input(), format!("{TASK}{FOOTER}").as_bytes());
                }
                assert_eq!(
                    unsafe { libc::kill(fixture.worker.0.id() as i32, signal) },
                    0
                );
                assert_eq!(receive(fixture.agent.as_mut().unwrap()), "WRAPPER_SHUTDOWN");
                assert_eq!(receive(&mut backend), "BACKEND_SHUTDOWN");
                assert!(
                    backend_running(pid),
                    "backend exited before explicit release"
                );
                send(&mut backend, "CHECK");
                assert_eq!(receive(&mut backend), "PROMPT_AVAILABLE");
                for path in &fixture.prompt_files {
                    assert!(
                        path.exists(),
                        "prompt removed while backend is shutting down"
                    );
                }
                let mut ready = libc::pollfd {
                    fd: fixture.worker_wire.get_ref().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                assert_eq!(
                    unsafe { libc::poll(&mut ready, 1, 0) },
                    0,
                    "adapter returned while backend remained alive"
                );
                match scenario {
                    "descendant_graceful" => send(&mut backend, "RELEASE"),
                    "descendant_orphan" => send(fixture.agent.as_mut().unwrap(), "ORPHAN"),
                    _ => {} // Backend deliberately keeps running after TERM; only escalation retires it.
                }
                let result = fixture.result(None);
                assert!(
                    !backend_running(pid),
                    "backend survived cancellation: {result}"
                );
                assert!(
                    result["error"]
                        .as_str()
                        .unwrap()
                        .contains("cancelled by signal"),
                    "{result}"
                );
                fixture.assert_cleaned();
                fixture.assert_secrecy();
                if scenario == "descendant_graceful" {
                    assert!(
                        fs::read_to_string(fixture.root.path().join("backend.wait_status"))
                            .unwrap()
                            .contains("exit status: 0"),
                        "wrapper did not reap its backend gracefully"
                    );
                }
            }
        }
    }
}

fn assert_signal_contract(provider: &str) {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        for policy in [
            "fatal: false",
            "continue_on_error: true",
            "parse_json: true\n    continue_on_error: true",
        ] {
            for during_delivery in [false, true] {
                if during_delivery && provider == "copilot" {
                    continue;
                }
                let task = if during_delivery {
                    format!("{TASK}{}", "x".repeat(2 * 1024 * 1024))
                } else {
                    TASK.into()
                };
                let mut fixture = Fixture::start(
                    provider,
                    &task,
                    Some(SYSTEM),
                    json!({"recipe_policy": policy}),
                );
                let prefix = if during_delivery {
                    let wire = fixture.agent.as_mut().unwrap();
                    send(wire, "PREFIX");
                    Some(receive(wire))
                } else {
                    fixture.read_input();
                    None
                };
                // This PID was started and recorded by this fixture.
                assert_eq!(
                    unsafe { libc::kill(fixture.worker.0.id() as i32, signal) },
                    0
                );
                let first = receive(&mut fixture.worker_wire);
                // Old adapters invoke our baseline handler. Release their
                // child so the failing assertion never abandons a process.
                fixture.release();
                let result = fixture.result((first != "BASELINE_SIGNAL").then_some(first.clone()));
                fixture.assert_cleaned();
                assert_ne!(
                    first, "BASELINE_SIGNAL",
                    "{provider} never installed cancellation ownership"
                );
                if let Some(prefix) = prefix {
                    let parts: Vec<_> = prefix.split_whitespace().collect();
                    assert_eq!(
                        &parts[..2],
                        ["PREFIX_RECEIVED", "1"],
                        "stdin delivery never began"
                    );
                    let capacity: usize = parts[2].parse().expect("stdin must be a pipe");
                    assert!(
                        task.len() > capacity + 1,
                        "payload must exceed observed pipe capacity"
                    );
                }
                assert_eq!(result["recipe"]["success"], false, "{result}");
                assert!(
                    !fixture.root.path().join("work/continued").exists(),
                    "{provider} ran subsequent work after interruption"
                );
                assert!(
                    result.to_string().contains("cancelled by signal"),
                    "{result}"
                );
                fixture.assert_secrecy();
            }
        }
    }
}

#[test]
fn cleanup_failures_are_reported_alongside_primary_execution_failures() {
    for provider in ["claude", "copilot"] {
        for (scenario, policy) in [
            ("success", None),
            ("nonzero", None),
            ("nonzero", Some("fatal: false")),
        ] {
            let mut fixture = Fixture::start(
                provider,
                TASK,
                Some(SYSTEM),
                json!({"scenario": scenario, "recipe_policy": policy}),
            );
            fixture.read_input();
            // Inject a deterministic filesystem error without privilege-dependent chmod.
            // Preserve moved resources for the fixture's own final cleanup.
            if let Some(file) = fixture.prompt_files.first() {
                let directory = file.parent().unwrap();
                fs::rename(directory, fixture.root.path().join("quarantine")).unwrap();
                fs::write(directory, "cleanup obstruction").unwrap();
            }
            fixture.release();
            let result = fixture.result(None);
            let error = if policy.is_some() {
                assert_eq!(
                    result["recipe"]["success"], false,
                    "cleanup failure must remain terminal: {result}"
                );
                assert!(
                    !fixture.root.path().join("work/continued").exists(),
                    "cleanup failure allowed subsequent work"
                );
                result.to_string()
            } else {
                result["error"]
                    .as_str()
                    .expect("prompt cleanup failure must fail the step")
                    .to_owned()
            };
            assert!(error.to_lowercase().contains("clean"), "{error}");
            if scenario == "nonzero" {
                assert!(
                    error.contains("7"),
                    "primary execution error was lost: {error}"
                );
            }
        }
    }
}
