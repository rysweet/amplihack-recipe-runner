/// CLI subprocess adapter — executes agent steps by spawning `claude -p` subprocesses
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
        Self {
            cli: "claude".to_string(),
            working_dir: ".".to_string(),
        }
    }

    pub fn with_binary(mut self, binary: &str) -> Self {
        self.cli = binary.to_string();
        self
    }

    pub fn with_working_dir(mut self, dir: &str) -> Self {
        self.working_dir = dir.to_string();
        self
    }

    /// Build environment for child processes.
    ///
    /// - Removes CLAUDECODE so nested Claude sessions work.
    /// - Propagates session tree env vars, incrementing depth by 1.
    /// - Generates a tree ID if none exists.
    fn build_child_env() -> HashMap<String, String> {
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
            env::var("AMPLIHACK_MAX_DEPTH").unwrap_or_else(|_| "3".to_string()),
        );
        child_env.insert(
            "AMPLIHACK_MAX_SESSIONS".to_string(),
            env::var("AMPLIHACK_MAX_SESSIONS").unwrap_or_else(|_| "10".to_string()),
        );

        child_env
    }

    /// Internal: spawn agent with optional timeout.
    fn execute_agent_step_with_timeout(
        &self,
        prompt: &str,
        timeout: Option<u64>,
    ) -> Result<String, anyhow::Error> {
        // Use a temp directory to avoid file races with the parent session (#2758)
        let temp_dir = tempfile::tempdir()
            .with_context(|| "Failed to create temp directory for agent step")?;
        let actual_cwd = temp_dir.path();

        // Append non-interactive footer so nested sessions never hang (#2464)
        let full_prompt = format!("{}{}", prompt, NON_INTERACTIVE_FOOTER);

        let child_env = Self::build_child_env();

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

        let mut child = std::process::Command::new(&self.cli)
            .args(["-p", &full_prompt])
            .current_dir(actual_cwd)
            .envs(&child_env)
            .stdout(log_fh)
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to execute '{}'", self.cli))?;

        let child_pid = child.id();

        // Background heartbeat thread with timeout enforcement
        let stop = Arc::new(AtomicBool::new(false));
        let timed_out = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let timed_out_clone = timed_out.clone();
        let output_path = output_file.clone();
        let deadline = timeout.map(|s| Instant::now() + Duration::from_secs(s));

        let heartbeat = std::thread::spawn(move || {
            let mut last_size = 0u64;
            let mut last_activity = Instant::now();
            while !stop_clone.load(Ordering::Relaxed) {
                // Check timeout deadline
                if let Some(dl) = deadline
                    && Instant::now() >= dl
                {
                    eprintln!(
                        "  [agent] TIMEOUT after {}s — killing process {}",
                        timeout.unwrap_or(0),
                        child_pid
                    );
                    timed_out_clone.store(true, Ordering::SeqCst);
                    // Send SIGTERM via kill
                    let _ = Command::new("kill")
                        .args(["-15", &child_pid.to_string()])
                        .output();
                    // Give 5s grace, then SIGKILL
                    std::thread::sleep(Duration::from_secs(5));
                    let _ = Command::new("kill")
                        .args(["-9", &child_pid.to_string()])
                        .output();
                    return;
                }

                if let Ok(meta) = std::fs::metadata(&output_path) {
                    let current_size = meta.len();
                    if current_size > last_size {
                        if let Ok(file) = std::fs::File::open(&output_path) {
                            let reader = BufReader::new(file);
                            if let Some(Ok(last_line)) = reader.lines().last() {
                                let truncated = &last_line[..last_line.len().min(120)];
                                eprintln!("  [agent] {}", truncated);
                            }
                        }
                        last_size = current_size;
                        last_activity = Instant::now();
                    } else if last_activity.elapsed() > Duration::from_secs(60) {
                        eprintln!(
                            "  [agent] ... still running ({}s since last output)",
                            last_activity.elapsed().as_secs()
                        );
                        last_activity = Instant::now();
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });

        let status = child.wait()?;
        stop.store(true, Ordering::SeqCst);
        let _ = heartbeat.join();

        if timed_out.load(Ordering::SeqCst) {
            let partial = std::fs::read_to_string(&output_file).unwrap_or_default();
            anyhow::bail!(
                "Agent step timed out after {}s. Partial output ({} bytes): {}...",
                timeout.unwrap_or(0),
                partial.len(),
                &partial[..partial.len().min(500)]
            );
        }

        let stdout = std::fs::read_to_string(&output_file).unwrap_or_default();

        // temp_dir is dropped here, cleaning up automatically

        if !status.success() {
            anyhow::bail!(
                "{} failed (exit {}): {}",
                self.cli,
                status.code().unwrap_or(-1),
                &stdout[stdout.len().saturating_sub(500)..]
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
        _system_prompt: Option<&str>,
        _mode: Option<&str>,
        _working_dir: &str,
    ) -> Result<String, anyhow::Error> {
        self.execute_agent_step_with_timeout(prompt, None)
    }

    fn execute_bash_step(
        &self,
        command: &str,
        working_dir: &str,
        timeout: Option<u64>,
    ) -> Result<String, anyhow::Error> {
        let child_env = Self::build_child_env();

        let output = if let Some(secs) = timeout {
            Command::new("timeout")
                .args([&secs.to_string(), "/bin/bash", "-c", command])
                .current_dir(working_dir)
                .envs(&child_env)
                .output()
                .with_context(|| "Failed to execute bash step with timeout")?
        } else {
            Command::new("/bin/bash")
                .args(["-c", command])
                .current_dir(working_dir)
                .envs(&child_env)
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
        which::which(&self.cli).is_ok()
    }

    fn name(&self) -> &str {
        "cli-subprocess"
    }
}
