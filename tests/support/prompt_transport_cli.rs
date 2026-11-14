//! Exercise the real runner CLI as well as its acknowledged live wrapper.
use super::*;

#[test]
fn context_file_keeps_runner_and_wrapper_argv_private() {
    for provider in ["claude", "codex", "copilot"] {
        for padding in [0, 131_073] {
            let root = tempfile::Builder::new().prefix("pc-").tempdir().unwrap();
            for dir in ["scratch", "work", "agents"] {
                fs::create_dir(root.path().join(dir)).unwrap();
            }
            let task = format!("{TASK}RUNNER_CONTEXT_SECRET_213{}", "x".repeat(padding));
            let context = root.path().join("context.json");
            fs::write(&context, json!({"private_value": task}).to_string()).unwrap();
            fs::set_permissions(&context, fs::Permissions::from_mode(0o600)).unwrap();
            fs::write(root.path().join("agents/persona.md"), SYSTEM).unwrap();
            fs::write(root.path().join("scenario"), "success").unwrap();
            let recipe = root.path().join("recipe.yaml");
            fs::write(
                &recipe,
                "name: private-context\nsteps:\n  - id: ask\n    type: agent\n    agent: amplihack:persona\n    prompt: '{{private_value}}'\n    auto_stage: false\n",
            )
            .unwrap();
            let launcher = root.path().join("launcher");
            fs::write(&launcher, launcher_bytes()).unwrap();
            fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
            let agents = UnixListener::bind(root.path().join("agent.sock")).unwrap();
            // Keep stdout in a file: the large JSON context must not block exit
            // on an undrained output pipe after the wrapper is released.
            let mut runner = OwnedWorker(
                Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
                    .arg(&recipe)
                    .arg("--context-file")
                    .arg(&context)
                    .args([
                        "--no-update-check",
                        "--no-auto-stage",
                        "--no-default-search",
                        "--output-format",
                        "json",
                        "--agent-binary",
                        provider,
                    ])
                    .arg("--agent-dir")
                    .arg(root.path().join("agents"))
                    .arg("--launcher")
                    .arg(&launcher)
                    .arg("--log-dir")
                    .arg(root.path().join("logs"))
                    .arg("-C")
                    .arg(root.path().join("work"))
                    .env_clear()
                    .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                    .env("HOME", root.path())
                    .env("TMPDIR", root.path().join("scratch"))
                    .env("PROMPT_TEST_ROOT", root.path())
                    .env("AMPLIHACK_RATELIMIT_MAX_RETRIES", "0")
                    .stdin(Stdio::null())
                    .stdout(fs::File::create(root.path().join("runner.stdout")).unwrap())
                    .stderr(fs::File::create(root.path().join("runner.stderr")).unwrap())
                    .spawn()
                    .unwrap(),
            );
            // Drop the control socket before the runner on assertion failure.
            let mut wire = accept(&agents);
            let wrapper_pid: u32 = receive(&mut wire)
                .strip_prefix("STARTED ")
                .unwrap()
                .parse()
                .unwrap();
            let runner_pid = runner.0.id();
            assert_ne!(runner_pid, wrapper_pid);
            for pid in [runner_pid, wrapper_pid] {
                let cmdline = fs::read(format!("/proc/{pid}/cmdline")).unwrap();
                for sentinel in [
                    "RUNNER_CONTEXT_SECRET_213",
                    "TASK_SECRET_205",
                    "SYSTEM_SECRET_205",
                ] {
                    assert!(
                        !cmdline
                            .windows(sentinel.len())
                            .any(|part| part == sentinel.as_bytes()),
                        "private content in live process {pid} argv ({provider})"
                    );
                }
            }
            send(&mut wire, "READ");
            assert_eq!(receive(&mut wire), "INPUT_RECEIVED");
            let effective = format!("{SYSTEM}\n\n{NO_REENTRY}");
            let task_input = format!("{task}{FOOTER}");
            let envelope =
                format!("# System instructions\n{effective}\n\n# Task instructions\n{task_input}");
            let input = fs::read(root.path().join("stdin.received")).unwrap();
            let system = fs::read(root.path().join("system.received")).unwrap();
            match provider {
                "claude" => {
                    assert_payload(&input, task_input.as_bytes());
                    assert_payload(&system, effective.as_bytes());
                }
                "codex" => assert_payload(&input, envelope.as_bytes()),
                "copilot" => assert_payload(&system, envelope.as_bytes()),
                _ => unreachable!(),
            }
            send(&mut wire, "RELEASE");
            drop(wire);
            let status = runner.0.wait().unwrap();
            assert!(
                status.success(),
                "{}",
                fs::read_to_string(root.path().join("runner.stderr")).unwrap()
            );
            assert!(!Path::new(&format!("/proc/{wrapper_pid}")).exists());
            let result: Value =
                serde_json::from_slice(&fs::read(root.path().join("runner.stdout")).unwrap())
                    .unwrap();
            assert_eq!(result["success"], true);
            assert_eq!(result["context"]["private_value"], task);
            assert!(
                fs::read_dir(root.path().join("scratch"))
                    .unwrap()
                    .next()
                    .is_none(),
                "temporary prompt resources survived CLI completion ({provider})"
            );
            // The runner reads but never deletes the caller's input file.
            assert!(context.exists());
        }
    }
}
