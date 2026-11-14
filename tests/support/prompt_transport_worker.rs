//! Adapter isolation: environment and process-wide signal handlers stay local.
use recipe_runner_rs::{
    adapters::{
        Adapter,
        cli_subprocess::{CLISubprocessAdapter, TimeoutAction},
    },
    runner::RecipeRunner,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::Path,
    sync::atomic::{AtomicI32, Ordering},
};

static SIGNAL_SOCKET: AtomicI32 = AtomicI32::new(-1);

extern "C" fn baseline_signal(_: libc::c_int) {
    // Only async-signal-safe write; this identifies absent product ownership
    // without killing the worker and orphaning its owned wrapper.
    let fd = SIGNAL_SOCKET.load(Ordering::Relaxed);
    let bytes = b"BASELINE_SIGNAL\n";
    unsafe {
        libc::write(fd, bytes.as_ptr().cast(), bytes.len());
    }
}

fn dispositions() -> [(usize, i32); 2] {
    [libc::SIGINT, libc::SIGTERM].map(|signal| unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut action), 0);
        (action.sa_sigaction, action.sa_flags & !0x04000000)
    })
}

// Keep the real adapter error intact so the runner exercises terminal-error
// classification under nonfatal policies, while supplying byte-exact inputs.
struct Inputs {
    adapter: CLISubprocessAdapter,
    task: String,
    system: Option<String>,
}

impl Adapter for Inputs {
    fn execute_agent_step(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        cwd: &str,
        _: Option<&str>,
        timeout: Option<u64>,
    ) -> anyhow::Result<String> {
        self.adapter.execute_agent_step(
            &self.task,
            None,
            self.system.as_deref(),
            None,
            cwd,
            Some("fixture-model"),
            timeout,
        )
    }

    fn execute_bash_step(
        &self,
        command: &str,
        cwd: &str,
        timeout: Option<u64>,
        env: &std::collections::HashMap<String, String>,
    ) -> anyhow::Result<String> {
        self.adapter.execute_bash_step(command, cwd, timeout, env)
    }

    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "prompt-transport-test"
    }
}

pub fn run(root: &Path) {
    let mut wire = UnixStream::connect(root.join("worker.sock")).unwrap();
    SIGNAL_SOCKET.store(wire.as_raw_fd(), Ordering::Relaxed);
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = baseline_signal as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGINT, libc::SIGTERM] {
            assert_eq!(libc::sigaction(signal, &action, std::ptr::null_mut()), 0);
        }
    }
    let before = dispositions();
    let config: Value =
        serde_json::from_slice(&fs::read(root.join("config.json")).unwrap()).unwrap();
    let task = fs::read_to_string(root.join("task")).unwrap();
    let system = fs::read_to_string(root.join("system")).ok();
    let launcher = match config["startup"].as_str() {
        Some("relative") => "relative/launcher".into(),
        Some("missing") => root.join("missing-launcher"),
        _ => root.join("launcher"),
    };
    let mut adapter = CLISubprocessAdapter::new()
        .with_binary(config["provider"].as_str().unwrap())
        .with_launcher(launcher)
        .with_working_dir(root.join("work").to_str().unwrap())
        .with_env_allow(["PATH", "HOME", "TMPDIR", "PROMPT_TEST_ROOT"]);
    if config["no_add_dir"] == true {
        adapter = adapter.without_working_dir_grant();
    }
    if config["no_allow_tools"] == true {
        adapter = adapter.without_allow_all_tools();
    }
    if config["logs"] == true {
        adapter = adapter.with_log_dir(root.join("logs"));
    }
    if config["report"] == true {
        adapter = adapter.with_timeout_action(TimeoutAction::Report);
    }
    writeln!(wire, "WORKER_STARTED").unwrap();
    let value = if let Some(policy) = config["recipe_policy"].as_str() {
        let recipe = recipe_runner_rs::parse_recipe(&format!(
            "name: prompt-interruption\nsteps:\n  - id: agent\n    type: agent\n    prompt: fixture\n    {policy}\n    auto_stage: false\n  - id: after\n    type: bash\n    command: printf continued > continued\n    auto_stage: false\n"
        )).unwrap();
        let adapter = Inputs {
            adapter,
            task,
            system,
        };
        let result = RecipeRunner::new(adapter)
            .with_working_dir(root.join("work").to_str().unwrap())
            .with_auto_stage(false)
            .execute(&recipe, None);
        json!({"recipe": result})
    } else {
        match adapter.execute_agent_step(
            &task,
            Some("fixture"),
            system.as_deref(),
            Some("autonomous"),
            "nested",
            Some("fixture-model"),
            config["timeout"].as_u64(),
        ) {
            Ok(output) => json!({"ok": output}),
            Err(error) => json!({"error": format!("{error:#}")}),
        }
    };
    let mut value = value;
    value["signals_restored"] = json!(dispositions() == before);
    writeln!(wire, "RESULT {value}").unwrap();
    SIGNAL_SOCKET.store(-1, Ordering::Relaxed);
}
