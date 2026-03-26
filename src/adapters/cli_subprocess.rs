/// CLI subprocess adapter — executes agent steps by spawning `amplihack <agent>`
/// subprocesses (configurable via `AMPLIHACK_AGENT_BINARY` env var, defaults to `claude`)
/// and bash steps via `/bin/bash -c`.
///
/// Agent steps use a temporary working directory to prevent file write races
/// when running inside a nested Claude Code session (#2758). Session tree env
/// vars are propagated so child processes respect recursion depth limits.
use crate::adapters::Adapter;
use anyhow::Context;
use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const NON_INTERACTIVE_FOOTER: &str = "\n\nIMPORTANT: Proceed autonomously. Do not ask questions. \
     Make reasonable decisions and continue.";

/// Maximum number of bytes displayed per output line in heartbeat output (SEC-03).
/// Lines exceeding this limit are truncated with a `... [N bytes truncated]` suffix.
/// Content beyond this limit is still captured in the log file and returned as output.
pub(crate) const DISPLAY_LIMIT: usize = 4096;

/// Format current UTC time as HH:MM:SS for heartbeat output.
fn format_utc_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = now.as_secs();
    let hours = (total_secs / 3600) % 24;
    let minutes = (total_secs / 60) % 60;
    let seconds = total_secs % 60;
    format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
}

/// ANSI CSI escape sequence pattern: ESC [ <params> <final-byte>.
/// Compiled once and reused across `sanitize_label` and `sanitize_output_line`.
static ANSI_RE: OnceLock<regex::Regex> = OnceLock::new();

fn ansi_re() -> &'static regex::Regex {
    ANSI_RE.get_or_init(|| regex::Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").expect("valid regex"))
}

/// Sanitize an agent label for safe embedding in terminal/stderr output (SEC-01).
///
/// Allows only alphanumeric characters plus `[-:_./ ]` (space included).
/// ANSI escape sequences are stripped first, then any remaining unsafe
/// characters are removed.  The result is capped at 64 characters to prevent
/// terminal injection via long labels.
///
/// # Examples
/// ```ignore
/// assert_eq!(sanitize_label("\x1b[31mred\x1b[0m"), "red");
/// assert_eq!(sanitize_label("amplihack:architect-v1"), "amplihack:architect-v1");
/// ```
fn sanitize_label(input: &str) -> String {
    let stripped = ansi_re().replace_all(input, "");

    // Single pass: filter safe chars and cap at 64 — avoids an intermediate String.
    stripped
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | ':' | '_' | '.' | '/' | ' '))
        .take(64)
        .collect()
}

/// Sanitize a single output line before writing to stderr (SEC-01, SEC-02, SEC-03).
///
/// 1. Strips ANSI CSI escape sequences (e.g. `\x1b[31m`) that could inject
///    terminal control codes when agent output is piped to a parent terminal.
/// 2. Strips null bytes (`\x00`) which can truncate log lines in external aggregators.
/// 3. Caps the displayed portion at 4096 characters and appends a truncation notice
///    `... [N bytes truncated]` when the line exceeds the limit.
/// 4. Content beyond 4096 chars is still captured in the temp file and returned in
///    the final `Ok(String)` result — only the *display* is capped.
///
/// # Security note
/// Agent output written to stderr may contain secrets if the agent echoes them.
/// Callers in CI should redirect stderr if logs are stored in public artifact stores.
fn sanitize_output_line(input: &str) -> String {
    // SEC-01: Strip ANSI escape sequences to prevent terminal injection.
    // Uses the same compiled regex as sanitize_label so there is no extra cost.
    let no_ansi: Cow<str> = if input.contains('\x1b') {
        Cow::Owned(ansi_re().replace_all(input, "").into_owned())
    } else {
        Cow::Borrowed(input)
    };

    // SEC-02: Strip null bytes.
    // Fast-path: most lines have no null bytes — skip the allocation entirely.
    let no_nulls: Cow<str> = if no_ansi.contains('\x00') {
        Cow::Owned(no_ansi.chars().filter(|c| *c != '\x00').collect())
    } else {
        no_ansi
    };

    // SEC-03: Cap display at DISPLAY_LIMIT bytes.
    // Walk back to the nearest valid UTF-8 char boundary so we never slice
    // through a multi-byte codepoint (which would panic).
    let byte_len = no_nulls.len();
    if byte_len > DISPLAY_LIMIT {
        let end = (0..=DISPLAY_LIMIT)
            .rev()
            .find(|&i| no_nulls.is_char_boundary(i))
            // SAFETY: index 0 is always a valid char boundary, so None is unreachable.
            .unwrap_or(0);
        let truncated_bytes = byte_len - end;
        format!(
            "{}... [{} bytes truncated]",
            &no_nulls[..end],
            truncated_bytes
        )
    } else {
        no_nulls.into_owned()
    }
}

/// Cross-platform check for whether a process with the given PID is still alive.
///
/// On Unix (Linux, macOS, etc.) this sends signal 0 to the process using the
/// `kill` shell command — signal 0 is never delivered but the kernel validates
/// that the target exists and the caller has permission to signal it.
///
/// On Windows this queries the process list via `tasklist`.
///
/// This replaces the Linux-only `/proc/<pid>` path check that always returned
/// `false` on macOS and Windows.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // `kill -0 <pid>` exits 0 if the process exists, non-zero otherwise.
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        // Windows: check via tasklist /FI "PID eq <pid>"
        // Use word-boundary split to avoid false positives where PID "123" matches
        // a line containing PID "1234" (bare `contains` would incorrectly return true).
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| {
                let pid_str = pid.to_string();
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .any(|field| field == pid_str.as_str())
            })
            .unwrap_or(false)
    }
}

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
            env::var("AMPLIHACK_MAX_SESSIONS")
                .unwrap_or_else(|_| crate::models::DEFAULT_MAX_SESSIONS.to_string()),
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
        agent_name: Option<&str>,
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
        let heartbeat_label = sanitize_label(agent_name.unwrap_or("agent"));

        let heartbeat = std::thread::spawn(move || {
            let mut last_pos = 0u64;
            let mut last_activity = Instant::now();
            let start_time = Instant::now();
            while !stop_clone.load(Ordering::Relaxed) {
                match std::fs::metadata(&output_path) {
                    Ok(meta) => {
                        let current_size = meta.len();
                        if current_size > last_pos {
                            // Read and display all new lines since last position.
                            // Only advance last_pos when the read succeeds to avoid
                            // silently skipping bytes on transient I/O errors.
                            match std::fs::File::open(&output_path) {
                                Ok(mut file) => match file.seek(SeekFrom::Start(last_pos)) {
                                    Err(e) => {
                                        log::debug!(
                                            "heartbeat: seek to {} failed: {}",
                                            last_pos,
                                            e
                                        );
                                    }
                                    Ok(_) => {
                                        let reader = BufReader::new(file);
                                        let ts = format_utc_timestamp();
                                        let mut read_ok = true;
                                        for line_result in reader.lines() {
                                            match line_result {
                                                Ok(line) => {
                                                    let trimmed = line.trim();
                                                    if !trimmed.is_empty() {
                                                        let safe_line =
                                                            sanitize_output_line(trimmed);
                                                        eprintln!(
                                                            "  [{}] [{}:{}] {}",
                                                            ts,
                                                            heartbeat_label,
                                                            child_pid,
                                                            safe_line
                                                        );
                                                    }
                                                }
                                                Err(e) => {
                                                    log::debug!(
                                                        "heartbeat: read error at pos {}: {}",
                                                        last_pos,
                                                        e
                                                    );
                                                    read_ok = false;
                                                    break;
                                                }
                                            }
                                        }
                                        if read_ok {
                                            last_pos = current_size;
                                            last_activity = Instant::now();
                                        }
                                    }
                                },
                                Err(e) => {
                                    log::debug!("heartbeat: cannot open output file: {}", e);
                                }
                            }
                        } else if last_activity.elapsed() > Duration::from_secs(30) {
                            let ts = format_utc_timestamp();
                            let total_elapsed = start_time.elapsed().as_secs();
                            let idle_secs = last_activity.elapsed().as_secs();
                            // Check if the child process is still alive.
                            // Uses `kill -0 <pid>` on Unix (works on Linux and
                            // macOS alike) and a tasklist query on Windows —
                            // avoids the Linux-only /proc filesystem path.
                            let pid_alive = is_pid_alive(child_pid);
                            if pid_alive {
                                eprintln!(
                                    "  [{}] [{}:{}] ... working ({}s elapsed, {}s since last output)",
                                    ts, heartbeat_label, child_pid, total_elapsed, idle_secs
                                );
                            } else {
                                eprintln!(
                                    "  [{}] [{}:{}] ... waiting ({}s elapsed, process may be finishing)",
                                    ts, heartbeat_label, child_pid, total_elapsed
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
            log::error!("Heartbeat thread panicked: {:?}", e);
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
        agent_name: Option<&str>,
        system_prompt: Option<&str>,
        _mode: Option<&str>,
        _working_dir: &str,
        model: Option<&str>,
    ) -> Result<String, anyhow::Error> {
        log::debug!(
            "CLISubprocessAdapter::execute_agent_step: prompt_len={}, agent={:?}, model={:?}",
            prompt.len(),
            agent_name,
            model
        );
        self.execute_agent_step_impl(prompt, agent_name, system_prompt, model)
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
        // SEC-03: Audit log for bash step execution.
        // Records the command digest (not the raw command) and working directory so
        // operators can correlate activity without exposing secrets that may appear
        // in the command string.  Full command is available at log::debug level.
        log::info!(
            "bash_step: executing command (len={}, working_dir={:?}, timeout={:?}s)",
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

    // -------------------------------------------------------------------------
    // is_pid_alive tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_is_pid_alive_current_process() {
        // The current process is guaranteed to be alive — is_pid_alive must return true.
        let own_pid = std::process::id();
        assert!(
            is_pid_alive(own_pid),
            "current process PID {} must be reported alive",
            own_pid
        );
    }

    #[test]
    fn test_is_pid_alive_reaped_process() {
        // Spawn a child, wait for it to exit, then verify it is no longer alive.
        let mut child = std::process::Command::new("true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn 'true'");
        let pid = child.id();
        child.wait().expect("failed to wait for child");
        // After reaping, the PID should not be alive (no zombie entry on Unix,
        // absent from tasklist on Windows).
        assert!(
            !is_pid_alive(pid),
            "reaped process PID {} must not be reported alive",
            pid
        );
    }

    // -------------------------------------------------------------------------
    // format_utc_timestamp tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_format_utc_timestamp_length() {
        let ts = format_utc_timestamp();
        assert_eq!(
            ts.len(),
            8,
            "timestamp must be exactly 8 characters (HH:MM:SS), got {:?}",
            ts
        );
    }

    #[test]
    fn test_format_utc_timestamp_format() {
        let ts = format_utc_timestamp();
        let re = regex::Regex::new(r"^\d{2}:\d{2}:\d{2}$").unwrap();
        assert!(
            re.is_match(&ts),
            "timestamp {:?} does not match HH:MM:SS pattern",
            ts
        );
    }

    #[test]
    fn test_format_utc_timestamp_valid_ranges() {
        let ts = format_utc_timestamp();
        let parts: Vec<&str> = ts.split(':').collect();
        assert_eq!(parts.len(), 3, "expected HH:MM:SS with two colons");
        let hh: u32 = parts[0].parse().expect("HH must be numeric");
        let mm: u32 = parts[1].parse().expect("MM must be numeric");
        let ss: u32 = parts[2].parse().expect("SS must be numeric");
        assert!(hh <= 23, "hours must be in 00-23, got {}", hh);
        assert!(mm <= 59, "minutes must be in 00-59, got {}", mm);
        assert!(ss <= 59, "seconds must be in 00-59, got {}", ss);
    }

    // -------------------------------------------------------------------------
    // sanitize_label tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_sanitize_label_ansi_escape_stripped() {
        // ANSI escape codes must be removed entirely
        let result = sanitize_label("\x1b[31mred\x1b[0m");
        assert_eq!(result, "red", "ANSI escape codes must be stripped");
    }

    #[test]
    fn test_sanitize_label_empty_string() {
        let result = sanitize_label("");
        assert_eq!(result, "", "empty input should produce empty output");
    }

    #[test]
    fn test_sanitize_label_max_length() {
        // Input longer than 64 chars must be truncated to exactly 64
        let long_input = "a".repeat(100);
        let result = sanitize_label(&long_input);
        assert_eq!(
            result.len(),
            64,
            "output must be truncated to 64 chars, got {} chars",
            result.len()
        );
    }

    #[test]
    fn test_sanitize_label_safe_chars_passthrough() {
        // Alphanumeric plus [-:_./ ] must pass through unchanged
        let safe = "amplihack:architect-v1.2_test/foo";
        let result = sanitize_label(safe);
        assert_eq!(result, safe, "safe chars must pass through unchanged");
    }

    #[test]
    fn test_sanitize_label_null_bytes_stripped() {
        let result = sanitize_label("ag\x00ent");
        assert_eq!(result, "agent", "null bytes must be stripped");
    }

    #[test]
    fn test_sanitize_label_control_chars_stripped() {
        // Tabs, newlines, and carriage returns are not in the safe set
        let result = sanitize_label("ag\tent\nname\r");
        assert_eq!(
            result, "agentname",
            "tabs, newlines, and CR must be stripped; got {:?}",
            result
        );
    }

    #[test]
    fn test_sanitize_label_unicode_stripped() {
        // Non-ASCII (emoji, accented chars) must be removed
        let result = sanitize_label("agent\u{1F600}name\u{00E9}");
        assert_eq!(
            result, "agentname",
            "non-ASCII characters must be stripped; got {:?}",
            result
        );
    }

    #[test]
    fn test_sanitize_label_spaces_allowed() {
        // Spaces are listed in the safe charset and must not be removed
        let result = sanitize_label("my agent name");
        assert_eq!(
            result, "my agent name",
            "spaces must be allowed; got {:?}",
            result
        );
    }

    // -------------------------------------------------------------------------
    // sanitize_output_line tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_sanitize_output_line_ansi_escape_stripped() {
        // ANSI escape codes in agent output must be stripped to prevent terminal injection (SEC-01).
        let result = sanitize_output_line("\x1b[31mred text\x1b[0m and normal");
        assert_eq!(
            result, "red text and normal",
            "ANSI escape codes must be stripped from output lines; got {:?}",
            result
        );
    }

    #[test]
    fn test_sanitize_output_line_ansi_passthrough_without_escape() {
        // Lines with no ESC byte must be returned unchanged (fast-path check).
        let normal = "no escape codes here";
        let result = sanitize_output_line(normal);
        assert_eq!(
            result, normal,
            "lines without ESC must pass through unchanged"
        );
    }

    #[test]
    fn test_sanitize_output_line_null_bytes() {
        let result = sanitize_output_line("hello\x00world");
        assert_eq!(
            result, "helloworld",
            "null bytes must be stripped; got {:?}",
            result
        );
    }

    #[test]
    fn test_sanitize_output_line_normal_passthrough() {
        let normal = "this is a normal log line with spaces and punctuation!";
        let result = sanitize_output_line(normal);
        assert_eq!(result, normal, "normal text must pass through unchanged");
    }

    #[test]
    fn test_sanitize_output_line_truncation() {
        // A line longer than DISPLAY_LIMIT chars must be truncated with the notice suffix
        let long_line = "x".repeat(5000);
        let result = sanitize_output_line(&long_line);
        assert!(
            result.contains("... ["),
            "truncated line must contain truncation notice; got prefix: {:?}",
            &result[..50.min(result.len())]
        );
        assert!(
            result.contains("bytes truncated]"),
            "truncation notice must mention bytes truncated"
        );
        // The displayed portion must not exceed DISPLAY_LIMIT chars before the notice
        let prefix_end = result.find("... [").unwrap();
        assert_eq!(
            prefix_end, DISPLAY_LIMIT,
            "exactly DISPLAY_LIMIT chars should be kept before the truncation notice"
        );
    }

    #[test]
    fn test_sanitize_output_line_exactly_display_limit() {
        // Exactly DISPLAY_LIMIT chars must pass through without any truncation notice
        let exact = "y".repeat(DISPLAY_LIMIT);
        let result = sanitize_output_line(&exact);
        assert_eq!(
            result.len(),
            DISPLAY_LIMIT,
            "DISPLAY_LIMIT-char line must not be truncated"
        );
        assert!(
            !result.contains("truncated"),
            "DISPLAY_LIMIT-char line must not have truncation notice"
        );
    }

    #[test]
    fn test_sanitize_output_line_one_over_display_limit() {
        // DISPLAY_LIMIT+1 chars: 1 byte over the limit — must trigger truncation
        let over = "z".repeat(DISPLAY_LIMIT + 1);
        let result = sanitize_output_line(&over);
        assert!(
            result.contains("... [1 bytes truncated]"),
            "DISPLAY_LIMIT+1-char line must show '... [1 bytes truncated]'; got: {:?}",
            &result[(DISPLAY_LIMIT - 6).min(result.len())..]
        );
    }

    #[test]
    fn test_sanitize_output_line_ansi_then_truncation() {
        // A line with embedded ANSI codes whose stripped length still exceeds DISPLAY_LIMIT.
        // Verifies that ANSI stripping is applied before the length cap, and truncation
        // fires on the stripped byte count, not the raw byte count.
        let ansi_prefix = "\x1b[31m";
        let ansi_suffix = "\x1b[0m";
        let content = "x".repeat(DISPLAY_LIMIT + 10);
        let input = format!("{}{}{}", ansi_prefix, content, ansi_suffix);
        let result = sanitize_output_line(&input);
        // ANSI codes are stripped first, then the DISPLAY_LIMIT cap applies.
        assert!(
            result.contains("bytes truncated"),
            "line exceeding DISPLAY_LIMIT after ANSI stripping must be truncated; got: {}",
            &result[..50.min(result.len())]
        );
        // No ANSI codes should remain.
        assert!(
            !result.contains('\x1b'),
            "ANSI codes must be stripped before truncation check"
        );
    }

    #[test]
    fn test_sanitize_output_line_ansi_reduces_below_display_limit() {
        // A line whose raw byte count exceeds DISPLAY_LIMIT but whose stripped content
        // is within the limit. No truncation notice should appear.
        let ansi_codes = "\x1b[31m".repeat(100); // 500 bytes of ANSI codes
        let content = "y".repeat(DISPLAY_LIMIT - 1); // 4095 visible bytes
        let input = format!("{}{}", ansi_codes, content);
        assert!(
            input.len() > DISPLAY_LIMIT,
            "raw input must exceed DISPLAY_LIMIT for this test"
        );
        let result = sanitize_output_line(&input);
        assert!(
            !result.contains("bytes truncated"),
            "line within DISPLAY_LIMIT after ANSI stripping must not be truncated"
        );
        assert_eq!(
            result.len(),
            DISPLAY_LIMIT - 1,
            "result should contain only the visible content"
        );
    }

    #[test]
    fn test_sanitize_output_line_multibyte_utf8_at_boundary() {
        // Build a string whose 4096th byte falls in the middle of a 3-byte UTF-8
        // character (U+4E2D, "中", encoded as 0xE4 0xB8 0xAD).  The truncation
        // must not panic and must produce a valid String.
        let padding = "a".repeat(4095); // 4095 ASCII bytes
        let multibyte = "\u{4E2D}"; // 3 bytes: positions 4095–4097
        let extra = "b".repeat(10);
        let input = format!("{}{}{}", padding, multibyte, extra);
        // byte_len = 4095 + 3 + 10 = 4108 > 4096 — truncation path is exercised
        let result = sanitize_output_line(&input);
        // Must contain a truncation notice — the primary invariant under test.
        assert!(
            result.contains("bytes truncated"),
            "result must contain truncation notice when input exceeds 4096 bytes; got: {}",
            result
        );
        // Must be valid UTF-8 — the boundary-walk must not produce invalid sequences.
        assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "result must be valid UTF-8 after truncation at a char boundary"
        );
    }
}
