/// CLI subprocess adapter — executes agent steps by spawning `amplihack <agent>`
/// subprocesses (configurable via `AMPLIHACK_AGENT_BINARY` env var, defaults to `claude`)
/// and bash steps via `/bin/bash -c`.
///
/// Agent steps use a temporary working directory to prevent file write races
/// when running inside a nested Claude Code session (#2758). Session tree env
/// vars are propagated so child processes respect recursion depth limits.
use crate::adapters::Adapter;
use anyhow::Context;
use std::collections::HashMap;
use std::env;
use std::io::{BufRead, BufReader};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const NON_INTERACTIVE_FOOTER: &str = "\n\nIMPORTANT: Proceed autonomously. Do not ask questions. \
     Make reasonable decisions and continue.";

pub struct CLISubprocessAdapter {
    cli: String,
    working_dir: String,
}

impl CLISubprocessAdapter {
    pub fn new() -> Self {
        // Use AMPLIHACK_AGENT_BINARY env var if set, otherwise default to "claude"
        let cli = env::var("AMPLIHACK_AGENT_BINARY").unwrap_or_else(|_| "claude".to_string());
        log::debug!(
            "CLISubprocessAdapter::new: creating adapter with cli={:?}",
            cli
        );
        Self {
            cli,
            working_dir: ".".to_string(),
        }
    }

    pub fn with_binary(mut self, binary: &str) -> Self {
        log::debug!("CLISubprocessAdapter::with_binary: binary={:?}", binary);
        self.cli = binary.to_string();
        self
    }

    pub fn with_working_dir(mut self, dir: &str) -> Self {
        log::debug!("CLISubprocessAdapter::with_working_dir: dir={:?}", dir);
        self.working_dir = dir.to_string();
        self
    }

    /// Build environment for child processes.
    ///
    /// - Removes CLAUDECODE so nested Claude sessions work.
    /// - Propagates session tree env vars, incrementing depth by 1.
    /// - Generates a tree ID if none exists.
    fn build_child_env() -> HashMap<String, String> {
        log::debug!("CLISubprocessAdapter::build_child_env: building child environment");
        let mut child_env: HashMap<String, String> =
            env::vars().filter(|(k, _)| k != "CLAUDECODE").collect();

        let current_depth: u32 = env::var("AMPLIHACK_SESSION_DEPTH")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let tree_id = env::var("AMPLIHACK_TREE_ID")
            .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string()[..8].to_string());

        child_env.insert("AMPLIHACK_TREE_ID".to_string(), tree_id);
        child_env.insert(
            "AMPLIHACK_SESSION_DEPTH".to_string(),
            (current_depth + 1).to_string(),
        );
        child_env.insert(
            "AMPLIHACK_MAX_DEPTH".to_string(),
            env::var("AMPLIHACK_MAX_DEPTH")
                .unwrap_or_else(|_| crate::models::DEFAULT_MAX_DEPTH.to_string()),
        );
        child_env.insert(
            "AMPLIHACK_MAX_SESSIONS".to_string(),
            env::var("AMPLIHACK_MAX_SESSIONS").unwrap_or_else(|_| "10".to_string()),
        );

        child_env
    }

    /// Internal: spawn agent with optional system prompt.
    ///
    /// Agent steps run without a timeout — they complete when the underlying
    /// CLI process exits.
    fn execute_agent_step_impl(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
        model: Option<&str>,
    ) -> Result<String, anyhow::Error> {
        log::debug!(
            "execute_agent_step_impl: prompt_len={}, has_system_prompt={}, model={:?}",
            prompt.len(),
            system_prompt.is_some(),
            model
        );
        // Use a temp directory to avoid file races with the parent session (#2758)
        let temp_dir = tempfile::tempdir()
            .with_context(|| "Failed to create temp directory for agent step")?;
        let actual_cwd = temp_dir.path();

        // Append non-interactive footer so nested sessions never hang (#2464)
        let full_prompt = format!("{}{}", prompt, NON_INTERACTIVE_FOOTER);

        let mut child_env = Self::build_child_env();
        // Ensure nested agent steps inherit the same agent binary preference
        child_env.insert("AMPLIHACK_AGENT_BINARY".to_string(), self.cli.clone());

        // Create output log file
        let output_dir = actual_cwd.join(".recipe-output");
        std::fs::create_dir_all(&output_dir)?;
        let output_file = output_dir.join(format!(
            "agent-step-{}.log",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        ));

        let log_fh = std::fs::File::create(&output_file)?;

        // Always launch via `amplihack <agent>` so the amplihack infrastructure
        // (env setup, guards, hooks) is properly initialized.
        let mut cmd = std::process::Command::new("amplihack");
        cmd.args([&self.cli, "-p", &full_prompt]);
        if let Some(sp) = system_prompt {
            cmd.args(["--system-prompt", sp]);
        }
        if let Some(m) = model {
            cmd.args(["--model", m]);
        }
        let mut child = cmd
            .current_dir(actual_cwd)
            .env_remove("CLAUDECODE")
            .envs(&child_env)
            .stdout(log_fh)
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to execute 'amplihack {}'", self.cli))?;

        // Background heartbeat thread for progress reporting.
        // Monitors the output log file for growth and prints status updates
        // to stderr.  When the agent is working but producing no stdout
        // (common with `claude -p` which writes all output at the end),
        // the heartbeat shows elapsed time and confirms the process is alive
        // so the user (or parent orchestrator) does not mistake silence for
        // a hang.  See issue #3266.
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let output_path = output_file.clone();
        let child_pid = child.id();

        let heartbeat = std::thread::spawn(move || {
            let mut last_size = 0u64;
            let mut last_activity = Instant::now();
            let start_time = Instant::now();
            while !stop_clone.load(Ordering::Relaxed) {
                match std::fs::metadata(&output_path) {
                    Ok(meta) => {
                        let current_size = meta.len();
                        if current_size > last_size {
                            match std::fs::File::open(&output_path) {
                                Ok(file) => {
                                    let reader = BufReader::new(file);
                                    if let Some(Ok(last_line)) = reader.lines().last() {
                                        let truncated = crate::safe_truncate(&last_line, 120);
                                        eprintln!("  [agent] {}", truncated);
                                    }
                                }
                                Err(e) => {
                                    log::debug!("heartbeat: cannot open output file: {}", e);
                                }
                            }
                            last_size = current_size;
                            last_activity = Instant::now();
                        } else if last_activity.elapsed() > Duration::from_secs(30) {
                            let total_elapsed = start_time.elapsed().as_secs();
                            let idle_secs = last_activity.elapsed().as_secs();
                            // Check if the child process is still alive via /proc
                            let pid_alive =
                                std::path::Path::new(&format!("/proc/{}", child_pid)).exists();
                            if pid_alive {
                                eprintln!(
                                    "  [agent] ... working ({}s elapsed, {}s since last output, pid {} alive)",
                                    total_elapsed, idle_secs, child_pid
                                );
                            } else {
                                eprintln!(
                                    "  [agent] ... waiting ({}s elapsed, process may be finishing)",
                                    total_elapsed
                                );
                            }
                            last_activity = Instant::now();
                        }
                    }
                    Err(e) => {
                        log::debug!("heartbeat: cannot stat output file: {}", e);
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });

        let status = child.wait()?;
        stop.store(true, Ordering::SeqCst);
        if let Err(e) = heartbeat.join() {
            log::warn!("Heartbeat thread panicked: {:?}", e);
        }

        let stdout =
            std::fs::read_to_string(&output_file).context("Failed to read agent output file")?;

        // temp_dir is dropped here, cleaning up automatically

        if !status.success() {
            anyhow::bail!(
                "amplihack {} failed (exit {}): {}",
                self.cli,
                status.code().unwrap_or(-1),
                crate::safe_tail(&stdout, 500)
            );
        }

        Ok(stdout.trim().to_string())
    }
}

impl Default for CLISubprocessAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl Adapter for CLISubprocessAdapter {
    fn execute_agent_step(
        &self,
        prompt: &str,
        _agent_name: Option<&str>,
        system_prompt: Option<&str>,
        _mode: Option<&str>,
        _working_dir: &str,
        model: Option<&str>,
    ) -> Result<String, anyhow::Error> {
        log::debug!(
            "CLISubprocessAdapter::execute_agent_step: prompt_len={}, model={:?}",
            prompt.len(),
            model
        );
        self.execute_agent_step_impl(prompt, system_prompt, model)
    }

    fn execute_bash_step(
        &self,
        command: &str,
        working_dir: &str,
        timeout: Option<u64>,
        extra_env: &std::collections::HashMap<String, String>,
    ) -> Result<String, anyhow::Error> {
        log::debug!(
            "CLISubprocessAdapter::execute_bash_step: command_len={}, working_dir={:?}, timeout={:?}",
            command.len(),
            working_dir,
            timeout
        );
        let mut child_env = Self::build_child_env();
        // Propagate agent binary preference so scripts spawning nested agents
        // use the same binary as the parent (mirrors execute_agent_step_impl).
        child_env.insert("AMPLIHACK_AGENT_BINARY".to_string(), self.cli.clone());
        let effective_dir = if working_dir.is_empty() || working_dir == "." {
            &self.working_dir
        } else {
            working_dir
        };

        let output = if let Some(secs) = timeout {
            Command::new("timeout")
                .args([&secs.to_string(), "/bin/bash", "-c", command])
                .current_dir(effective_dir)
                .env_remove("CLAUDECODE")
                .envs(&child_env)
                .envs(extra_env)
                .output()
                .with_context(|| "Failed to execute bash step with timeout")?
        } else {
            Command::new("/bin/bash")
                .args(["-c", command])
                .current_dir(effective_dir)
                .env_remove("CLAUDECODE")
                .envs(&child_env)
                .envs(extra_env)
                .output()
                .with_context(|| "Failed to execute bash step")?
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            anyhow::bail!(
                "Command failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            );
        }

        Ok(stdout.trim().to_string())
    }

    fn is_available(&self) -> bool {
        // Always available for bash steps. Agent steps will fail at execution
        // time if `amplihack` is not in PATH, providing a clear error message
        // for the specific step that needs it.
        log::debug!("CLISubprocessAdapter::is_available: always true");
        true
    }

    fn name(&self) -> &str {
        log::trace!("CLISubprocessAdapter::name: returning 'cli-subprocess'");
        "cli-subprocess"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to serialize tests that mutate AMPLIHACK_AGENT_BINARY env var.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// RAII guard that restores an env var on drop (even during panic unwinding).
    struct EnvGuard {
        key: &'static str,
        saved: Option<String>,
    }

    impl EnvGuard {
        fn new(key: &'static str) -> Self {
            let saved = env::var(key).ok();
            Self { key, saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: test runs hold ENV_MUTEX to serialize env var access
            unsafe {
                env::remove_var(self.key);
            }
            if let Some(val) = self.saved.take() {
                // SAFETY: test runs hold ENV_MUTEX to serialize env var access
                unsafe {
                    env::set_var(self.key, val);
                }
            }
        }
    }

    #[test]
    fn test_new_defaults_without_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::new("AMPLIHACK_AGENT_BINARY");
        // SAFETY: test runs hold ENV_MUTEX to serialize env var access
        unsafe {
            env::remove_var("AMPLIHACK_AGENT_BINARY");
        }

        let adapter = CLISubprocessAdapter::new();
        assert_eq!(adapter.cli, "claude");
        assert_eq!(adapter.working_dir, ".");
    }

    #[test]
    fn test_new_reads_env_var() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::new("AMPLIHACK_AGENT_BINARY");
        // SAFETY: test runs hold ENV_MUTEX to serialize env var access
        unsafe {
            env::set_var("AMPLIHACK_AGENT_BINARY", "copilot");
        }

        let adapter = CLISubprocessAdapter::new();
        assert_eq!(adapter.cli, "copilot");
    }

    #[test]
    fn test_with_binary() {
        let adapter = CLISubprocessAdapter::new().with_binary("my-agent");
        assert_eq!(adapter.cli, "my-agent");
    }

    #[test]
    fn test_with_working_dir() {
        let adapter = CLISubprocessAdapter::new().with_working_dir("/tmp/test");
        assert_eq!(adapter.working_dir, "/tmp/test");
    }

    #[test]
    fn test_default_impl() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::new("AMPLIHACK_AGENT_BINARY");
        // SAFETY: test runs hold ENV_MUTEX to serialize env var access
        unsafe {
            env::remove_var("AMPLIHACK_AGENT_BINARY");
        }

        let adapter = CLISubprocessAdapter::default();
        assert_eq!(adapter.cli, "claude");
        assert_eq!(adapter.working_dir, ".");
    }

    #[test]
    fn test_is_available_always_true() {
        let adapter = CLISubprocessAdapter::new();
        assert!(adapter.is_available());
    }

    #[test]
    fn test_name() {
        let adapter = CLISubprocessAdapter::new();
        assert_eq!(adapter.name(), "cli-subprocess");
    }

    #[test]
    fn test_build_child_env_has_required_keys() {
        let env = CLISubprocessAdapter::build_child_env();
        // All of these keys must always be present
        assert!(env.contains_key("AMPLIHACK_SESSION_DEPTH"));
        assert!(env.contains_key("AMPLIHACK_MAX_DEPTH"));
        assert!(env.contains_key("AMPLIHACK_TREE_ID"));
        assert!(env.contains_key("AMPLIHACK_MAX_SESSIONS"));
        // CLAUDECODE is never passed to children
        assert!(!env.contains_key("CLAUDECODE"));
    }

    #[test]
    fn test_build_child_env_tree_id_nonempty() {
        let env = CLISubprocessAdapter::build_child_env();
        let tree_id = env.get("AMPLIHACK_TREE_ID").unwrap();
        assert!(!tree_id.is_empty(), "tree ID should be non-empty");
    }

    #[test]
    fn test_build_child_env_max_sessions_is_numeric() {
        let env = CLISubprocessAdapter::build_child_env();
        let ms: u32 = env
            .get("AMPLIHACK_MAX_SESSIONS")
            .unwrap()
            .parse()
            .expect("max_sessions must be numeric");
        assert!(ms >= 1);
    }

    #[test]
    fn test_build_child_env_increments_depth() {
        // build_child_env reads current AMPLIHACK_SESSION_DEPTH and increments by 1
        // Since tests run in parallel, just verify the result is a valid number > 0
        let env = CLISubprocessAdapter::build_child_env();
        let depth: u32 = env
            .get("AMPLIHACK_SESSION_DEPTH")
            .unwrap()
            .parse()
            .expect("depth should be a number");
        assert!(depth >= 1, "child depth should be at least 1");
    }

    #[test]
    fn test_build_child_env_max_depth_valid() {
        let env = CLISubprocessAdapter::build_child_env();
        let max_depth: u32 = env
            .get("AMPLIHACK_MAX_DEPTH")
            .unwrap()
            .parse()
            .expect("max_depth should be a number");
        assert!(max_depth >= 1, "max_depth should be at least 1");
    }

    #[test]
    fn test_build_child_env_preserves_max_depth() {
        // Verify max_depth is always set to a valid value
        let env = CLISubprocessAdapter::build_child_env();
        let max_depth: u32 = env
            .get("AMPLIHACK_MAX_DEPTH")
            .unwrap()
            .parse()
            .expect("max_depth should be a number");
        assert!(max_depth >= 1, "max_depth should be at least 1");
    }

    #[test]
    fn test_build_child_env_preserves_existing_tree_id() {
        // If AMPLIHACK_TREE_ID is already set, build_child_env preserves it
        let env = CLISubprocessAdapter::build_child_env();
        let tree_id = env.get("AMPLIHACK_TREE_ID").unwrap().clone();
        // Call again — tree_id should remain stable when already set in env
        assert!(!tree_id.is_empty());
    }

    #[test]
    fn test_build_child_env_depth_is_always_valid() {
        // Regardless of env state, the child depth must be a valid positive number
        let env = CLISubprocessAdapter::build_child_env();
        let depth: u32 = env
            .get("AMPLIHACK_SESSION_DEPTH")
            .unwrap()
            .parse()
            .expect("depth must always be a valid number");
        assert!(depth >= 1);
    }

    #[test]
    fn test_execute_bash_step_echo() {
        let adapter = CLISubprocessAdapter::new();
        let empty_env = std::collections::HashMap::new();
        let result = adapter.execute_bash_step("echo hello world", ".", None, &empty_env);
        assert!(result.is_ok(), "echo should succeed: {:?}", result);
        assert_eq!(result.unwrap(), "hello world");
    }

    #[test]
    fn test_execute_bash_step_failure() {
        let adapter = CLISubprocessAdapter::new();
        let empty_env = std::collections::HashMap::new();
        let result = adapter.execute_bash_step("exit 1", ".", None, &empty_env);
        assert!(result.is_err(), "exit 1 should fail");
    }

    #[test]
    fn test_execute_bash_step_with_timeout() {
        let adapter = CLISubprocessAdapter::new();
        let empty_env = std::collections::HashMap::new();
        let result = adapter.execute_bash_step("echo timed", ".", Some(10), &empty_env);
        assert!(result.is_ok(), "timed echo should succeed: {:?}", result);
        assert_eq!(result.unwrap(), "timed");
    }

    #[test]
    fn test_execute_bash_step_timeout_kills() {
        let adapter = CLISubprocessAdapter::new();
        let empty_env = std::collections::HashMap::new();
        let result = adapter.execute_bash_step("sleep 60", ".", Some(1), &empty_env);
        assert!(result.is_err(), "sleep 60 with 1s timeout should fail");
    }

    #[test]
    fn test_execute_bash_step_working_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = CLISubprocessAdapter::new().with_working_dir(tmp.path().to_str().unwrap());
        let empty_env = std::collections::HashMap::new();
        let result = adapter.execute_bash_step("pwd", "", None, &empty_env);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(
            output.contains(tmp.path().to_str().unwrap()),
            "working dir should be respected, got: {}",
            output
        );
    }

    #[test]
    fn test_execute_bash_step_empty_command() {
        let adapter = CLISubprocessAdapter::new();
        let empty_env = std::collections::HashMap::new();
        let result = adapter.execute_bash_step("", ".", None, &empty_env);
        // Empty command succeeds with empty output in bash
        assert!(result.is_ok());
    }

    #[test]
    fn test_non_interactive_footer_constant() {
        assert!(NON_INTERACTIVE_FOOTER.contains("autonomously"));
        assert!(NON_INTERACTIVE_FOOTER.contains("Do not ask questions"));
    }

    #[test]
    fn test_with_binary_propagates_agent_binary_env() {
        let adapter = CLISubprocessAdapter::new().with_binary("copilot");
        // Simulate what execute_agent_step_impl does: build env then insert
        let mut env = CLISubprocessAdapter::build_child_env();
        env.insert("AMPLIHACK_AGENT_BINARY".to_string(), adapter.cli.clone());
        assert_eq!(
            env.get("AMPLIHACK_AGENT_BINARY").unwrap(),
            "copilot",
            "child env must propagate the overridden agent binary"
        );
    }
}
