/// CLI subprocess adapter — executes agent steps by spawning `claude -p` subprocesses
/// and bash steps via `sh -c`. Direct port from Python `cli_subprocess.py`.
use crate::adapters::Adapter;
use anyhow::Context;
use std::process::Command;

pub struct CLISubprocessAdapter {
    claude_binary: String,
}

impl CLISubprocessAdapter {
    pub fn new() -> Self {
        Self {
            claude_binary: "claude".to_string(),
        }
    }

    pub fn with_binary(mut self, binary: &str) -> Self {
        self.claude_binary = binary.to_string();
        self
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
        working_dir: &str,
    ) -> Result<String, anyhow::Error> {
        let output = Command::new(&self.claude_binary)
            .args(["-p", prompt])
            .current_dir(working_dir)
            .env_remove("CLAUDECODE")
            .output()
            .with_context(|| format!("Failed to execute '{}'", self.claude_binary))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            anyhow::bail!(
                "Agent step failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                stderr
            );
        }

        Ok(stdout)
    }

    fn execute_bash_step(
        &self,
        command: &str,
        working_dir: &str,
        timeout: Option<u64>,
    ) -> Result<String, anyhow::Error> {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]).current_dir(working_dir);

        let output = if let Some(secs) = timeout {
            // Use timeout command as a wrapper
            Command::new("timeout")
                .args([&secs.to_string(), "sh", "-c", command])
                .current_dir(working_dir)
                .output()
                .with_context(|| "Failed to execute bash step with timeout")?
        } else {
            cmd.output()
                .with_context(|| "Failed to execute bash step")?
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            anyhow::bail!(
                "Bash step failed (exit {}): {}{}",
                output.status.code().unwrap_or(-1),
                stdout,
                stderr
            );
        }

        Ok(stdout.trim().to_string())
    }

    fn is_available(&self) -> bool {
        Command::new(&self.claude_binary)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn name(&self) -> &str {
        "cli-subprocess"
    }
}
