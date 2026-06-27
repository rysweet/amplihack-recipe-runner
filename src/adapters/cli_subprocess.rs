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
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Format current wall-clock time as `HH:MM:SS` UTC with no external deps.
fn utc_hms() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let day_secs = secs % 86_400;
    let h = day_secs / 3600;
    let m = (day_secs % 3600) / 60;
    let s = day_secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

const NON_INTERACTIVE_FOOTER: &str = "\n\nIMPORTANT: Proceed autonomously. Do not ask questions. \
     Make reasonable decisions and continue.";
const RECIPE_CHILD_NO_REENTRY_SYSTEM_PROMPT: &str = "You are already inside an active \
     recipe-managed workflow step. This is not a new top-level user request. Do not invoke \
     /dev, dev-orchestrator, ultrathink, smart-orchestrator, or any other workflow or recipe \
     runner. Execute the requested step directly and return only the output requested by this \
     step.";
const MAX_INLINE_AGENT_PROMPT_BYTES: usize = 32 * 1024;
const FILE_BACKED_INLINE_PROMPT_BYTES: usize = 8 * 1024;
const FILE_BACKED_PROMPT_CONTINUATION_NOTE: &str = "\n\nIMPORTANT: Additional task instructions, output requirements, and context continue in the appended system prompt. Treat that appended content as part of this same request and follow it fully.";

/// Read at most `max_bytes + 1` bytes from `path`, returning UTF-8 (lossy).
///
/// The `+1` is a sentinel: callers can detect overflow when the returned
/// length exceeds `max_bytes` and apply their own truncation/warning policy
/// (see [`crate::runner::MAX_STEP_OUTPUT_BYTES`]). This caps agent-output
/// memory growth at a fixed bound regardless of how large the on-disk file
/// is, defending against runaway processes that produce multi-GB output.
///
/// Errors propagate from `File::open` (e.g. NotFound, PermissionDenied) so
/// callers can decide whether to retry or fall back. UTF-8 errors do not
/// fail; invalid sequences are replaced with U+FFFD.
pub(crate) fn read_capped(path: &Path, max_bytes: usize) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let cap = max_bytes.saturating_add(1);
    let mut buf = Vec::with_capacity(std::cmp::min(cap, 64 * 1024));
    file.take(cap as u64).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Detect transient agent rate-limit signals in captured output (#839).
///
/// Matches case-insensitively against the documented signal phrases. A match
/// means the failure is transient and retryable; a non-match means the failure
/// is a genuine logic/auth error that must keep failing fast (no retry).
pub(crate) fn is_rate_limit(text: &str) -> bool {
    const SIGNALS: [&str; 5] = [
        "hit your rate limit",
        "reset in",
        "rate limit",
        "429",
        "too many requests",
    ];
    let haystack = text.as_bytes();
    SIGNALS
        .iter()
        .any(|s| contains_ignore_ascii_case(haystack, s.as_bytes()))
}

/// Case-insensitive ASCII substring search with no heap allocation.
///
/// Equivalent to `haystack.to_lowercase().contains(needle)` when `needle` is
/// lowercase ASCII (all rate-limit signals are), but avoids allocating a full
/// lowercase copy of `haystack` — which on the failure path can be multi-MB of
/// captured agent output. Non-ASCII bytes (>= 0x80) never match an ASCII needle
/// since `eq_ignore_ascii_case` only folds ASCII, so this is byte-exact safe.
fn contains_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

/// Bounded exponential backoff delay for a rate-limit retry (#839).
///
/// `retry` is the 1-based retry counter (the just-failed attempt number), so
/// the first wait is `base_secs * 2^0 = base_secs`. The result is clamped to
/// `cap_secs`. A `base_secs` of 0 yields an instant retry (used by tests).
/// Arithmetic saturates so a hostile/large `retry` can never panic or overflow.
pub(crate) fn backoff_delay(retry: u32, base_secs: u64, cap_secs: u64) -> Duration {
    if base_secs == 0 {
        return Duration::from_secs(0);
    }
    let exp = retry.saturating_sub(1);
    // 1 << exp saturates to u64::MAX once exp >= 64 (checked_shl returns None).
    let factor = 1u64.checked_shl(exp).unwrap_or(u64::MAX);
    let delay = base_secs.saturating_mul(factor);
    Duration::from_secs(delay.min(cap_secs))
}

/// Hard ceiling on `max_retries` so a hostile env value cannot create an
/// unbounded execution budget.
const RATELIMIT_MAX_RETRIES_CEILING: u32 = 100;

/// Configuration for rate-limit retry/backoff, loaded from env (#839).
///
/// All knobs parse with a default fallback (never panic on bad input):
/// - `AMPLIHACK_RATELIMIT_MAX_RETRIES` (default 5, clamped to <=100)
/// - `AMPLIHACK_RATELIMIT_BASE_DELAY_SECS` (default 60; 0 => instant retries)
/// - `AMPLIHACK_RATELIMIT_MAX_DELAY_SECS` (default 600; raised to >= base)
/// - `AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL` (default off; non-empty => on)
#[derive(Debug, Clone, Copy)]
pub(crate) struct RateLimitConfig {
    pub max_retries: u32,
    pub base_delay_secs: u64,
    pub max_delay_secs: u64,
    pub fallback_auto_model: bool,
}

impl RateLimitConfig {
    pub(crate) fn from_env() -> Self {
        let max_retries = env::var("AMPLIHACK_RATELIMIT_MAX_RETRIES")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(5)
            .min(RATELIMIT_MAX_RETRIES_CEILING);

        let base_delay_secs = env::var("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(60);

        let mut max_delay_secs = env::var("AMPLIHACK_RATELIMIT_MAX_DELAY_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(600);
        // Never let the cap invert the formula.
        if max_delay_secs < base_delay_secs {
            max_delay_secs = base_delay_secs;
        }

        let fallback_auto_model = env::var("AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL")
            .map(|v| !v.is_empty())
            .unwrap_or(false);

        Self {
            max_retries,
            base_delay_secs,
            max_delay_secs,
            fallback_auto_model,
        }
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

        // Defense-in-depth: ensure HOME and PATH are always present and non-empty,
        // even when the parent shell has them unset. Mirrors amplihack-rs fork
        // (fix #277) — empty HOME/PATH can break non-interactive child steps.
        if child_env.get("HOME").is_none_or(|v| v.is_empty()) {
            child_env.insert("HOME".to_string(), "/root".to_string());
        }
        if child_env.get("PATH").is_none_or(|v| v.is_empty()) {
            child_env.insert(
                "PATH".to_string(),
                "/usr/local/bin:/usr/bin:/bin".to_string(),
            );
        }

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

    fn supports_file_backed_prompt_transport(&self) -> bool {
        // Claude-family CLIs support --append-system-prompt for overflow.
        // Copilot CLI does not, but we still need to avoid E2BIG.
        // For Copilot: truncate -p and write overflow to a prompt file
        // in the working dir that the agent can read via --add-dir.
        true
    }

    fn should_use_file_backed_prompt_transport(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
    ) -> bool {
        self.supports_file_backed_prompt_transport()
            && prompt.len() + system_prompt.map_or(0, str::len) > MAX_INLINE_AGENT_PROMPT_BYTES
    }

    fn build_effective_system_prompt(system_prompt: Option<&str>) -> String {
        match system_prompt.map(str::trim).filter(|s| !s.is_empty()) {
            Some(existing) => format!("{existing}\n\n{RECIPE_CHILD_NO_REENTRY_SYSTEM_PROMPT}"),
            None => RECIPE_CHILD_NO_REENTRY_SYSTEM_PROMPT.to_string(),
        }
    }

    fn write_private_prompt_file(
        output_dir: &std::path::Path,
        content: &str,
    ) -> Result<std::path::PathBuf, anyhow::Error> {
        let prompt_file = output_dir.join("agent-system-prompt.md");
        let mut file = std::fs::File::create(&prompt_file)
            .with_context(|| format!("Failed to create prompt file {}", prompt_file.display()))?;
        file.write_all(content.as_bytes())
            .with_context(|| format!("Failed to write prompt file {}", prompt_file.display()))?;
        file.flush()
            .with_context(|| format!("Failed to flush prompt file {}", prompt_file.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&prompt_file, std::fs::Permissions::from_mode(0o600))
                .with_context(|| {
                    format!("Failed to chmod prompt file {}", prompt_file.display())
                })?;
        }

        Ok(prompt_file)
    }

    fn clamp_char_boundary(text: &str, max_bytes: usize) -> usize {
        let mut boundary = max_bytes.min(text.len());
        while boundary > 0 && !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        boundary
    }

    fn build_file_backed_prompt_payload(
        &self,
        output_dir: &std::path::Path,
        prompt: &str,
        system_prompt: Option<&str>,
    ) -> Result<(String, Option<std::path::PathBuf>), anyhow::Error> {
        let preview_boundary = Self::clamp_char_boundary(prompt, FILE_BACKED_INLINE_PROMPT_BYTES);
        let prompt_overflow = if preview_boundary < prompt.len() {
            &prompt[preview_boundary..]
        } else {
            ""
        };

        let mut inline_prompt = if preview_boundary > 0 {
            prompt[..preview_boundary].to_string()
        } else {
            String::new()
        };
        if !prompt_overflow.is_empty() {
            inline_prompt.push_str(FILE_BACKED_PROMPT_CONTINUATION_NOTE);
        }

        let mut appended_prompt = String::new();
        if let Some(sp) = system_prompt
            && !sp.is_empty()
        {
            appended_prompt.push_str(sp);
        }
        if !prompt_overflow.is_empty() {
            if !appended_prompt.is_empty() {
                appended_prompt.push_str("\n\n");
            }
            appended_prompt.push_str("# Continued task instructions\n\n");
            appended_prompt.push_str(prompt_overflow);
        }

        let prompt_file = if appended_prompt.is_empty() {
            None
        } else {
            Some(Self::write_private_prompt_file(
                output_dir,
                &appended_prompt,
            )?)
        };

        Ok((inline_prompt, prompt_file))
    }

    fn build_agent_command(
        &self,
        output_dir: &std::path::Path,
        resolved_cwd: &std::path::Path,
        prompt: &str,
        system_prompt: Option<&str>,
        model: Option<&str>,
    ) -> Result<std::process::Command, anyhow::Error> {
        // Launch via `amplihack <agent>` by default so the amplihack
        // infrastructure (env setup, guards, hooks) is properly initialized.
        // Tests inject a fake launcher via AMPLIHACK_LAUNCHER_BINARY; when
        // unset, behavior is identical to the previous hardcoded "amplihack".
        let launcher = env::var("AMPLIHACK_LAUNCHER_BINARY")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "amplihack".to_string());
        let mut cmd = std::process::Command::new(launcher);
        cmd.arg(&self.cli);
        // All subsequent flags (-p, --model, --add-dir, --system-prompt, etc.)
        // are passthrough args for the underlying CLI (claude, copilot, codex).
        // The `amplihack <agent>` subcommand requires `--` to separate its own
        // flags from passthrough args (#4342).
        cmd.arg("--");
        // Copilot CLI requires --allow-all-tools for non-interactive use; without it,
        // nested copilot agents prompt for tool approval and hang/exit 1 (#88).
        // Opt out by setting AMPLIHACK_NO_ALLOW_ALL_TOOLS to any non-empty value.
        if self.cli == "copilot"
            && env::var("AMPLIHACK_NO_ALLOW_ALL_TOOLS")
                .map(|v| v.is_empty())
                .unwrap_or(true)
        {
            cmd.arg("--allow-all-tools");
        }
        let effective_system_prompt = Self::build_effective_system_prompt(system_prompt);

        if self.should_use_file_backed_prompt_transport(prompt, Some(&effective_system_prompt)) {
            let (inline_prompt, prompt_file) = self.build_file_backed_prompt_payload(
                output_dir,
                prompt,
                Some(&effective_system_prompt),
            )?;
            log::info!(
                "Using file-backed prompt transport for '{}' (prompt={} bytes, system_prompt={} bytes)",
                self.cli,
                prompt.len(),
                effective_system_prompt.len()
            );
            if let Some(ref pf) = prompt_file {
                // Claude-family CLIs support --append-system-prompt directly.
                // Copilot CLI does not — use --add-dir so the agent can read the file.
                if matches!(self.cli.as_str(), "claude" | "launch" | "RustyClawd") {
                    cmd.args(["--append-system-prompt", &pf.to_string_lossy()]);
                } else {
                    // For Copilot and others: make the prompt file accessible via --add-dir
                    // and include a note in the inline prompt pointing to it.
                    cmd.args(["--add-dir", &output_dir.to_string_lossy()]);
                }
            }
            cmd.args(["-p", &inline_prompt]);
        } else {
            cmd.args(["-p", prompt]);
            // Only pass --system-prompt for CLIs that support it
            if matches!(self.cli.as_str(), "claude" | "launch" | "RustyClawd") {
                cmd.args(["--system-prompt", &effective_system_prompt]);
            }
        }

        cmd.args(["--add-dir", &resolved_cwd.to_string_lossy()]);
        if let Some(m) = model {
            cmd.args(["--model", m]);
        }

        Ok(cmd)
    }

    /// Internal: spawn agent with optional system prompt and timeout.
    ///
    /// When `timeout` is `Some(secs)`, the agent process is killed after the
    /// given number of seconds.  Without a timeout the step runs until the
    /// underlying CLI process exits on its own.
    fn execute_agent_step_impl(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
        model: Option<&str>,
        working_dir: &str,
        timeout: Option<u64>,
    ) -> Result<String, anyhow::Error> {
        log::debug!(
            "execute_agent_step_impl: prompt_len={}, has_system_prompt={}, model={:?}, working_dir={:?}",
            prompt.len(),
            system_prompt.is_some(),
            model,
            working_dir
        );
        // Resolve the actual working directory for the agent.
        // Use the recipe's working_dir so agents operate against the real repo,
        // not a disconnected temp dir (#3766, #3769).
        let resolved_cwd = if working_dir.is_empty() || working_dir == "." {
            std::path::PathBuf::from(&self.working_dir)
        } else {
            let p = std::path::PathBuf::from(working_dir);
            if p.is_relative() {
                // Resolve relative paths against the runner's working directory,
                // not the process cwd (which may differ).
                std::path::PathBuf::from(&self.working_dir).join(&p)
            } else {
                p
            }
        };

        // Canonicalize to an absolute path so downstream args like --add-dir
        // are not re-resolved against the child process cwd. Without this,
        // a relative resolved_cwd like "./worktrees/foo" gets passed to
        // copilot's --add-dir, which copilot then joins to its cwd
        // (already <worktree>) producing a doubled path that doesn't exist.
        let resolved_cwd = match resolved_cwd.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                // Fall through to the existence check below to surface
                // a clear error if the path is bogus.
                resolved_cwd
            }
        };

        // Verify the resolved cwd exists before launching the agent.
        // A missing cwd causes a confusing "No such file or directory" on
        // the amplihack binary itself, masking the real error.
        if !resolved_cwd.is_dir() {
            anyhow::bail!(
                "Agent working directory does not exist: {}. \
                 Check that step-04 created the worktree successfully.",
                resolved_cwd.display()
            );
        }

        // Create a temp directory for the output log file only.
        // The agent process itself runs from the resolved repo cwd.
        let temp_dir = tempfile::tempdir()
            .with_context(|| "Failed to create temp directory for agent output")?;
        let output_log_dir = temp_dir.path();

        // Append non-interactive footer so nested sessions never hang (#2464)
        let full_prompt = format!("{}{}", prompt, NON_INTERACTIVE_FOOTER);

        let mut child_env = Self::build_child_env();
        // Ensure nested agent steps inherit the same agent binary preference
        child_env.insert("AMPLIHACK_AGENT_BINARY".to_string(), self.cli.clone());

        // Bounded rate-limit retry loop (#839). Total executions = 1 + max_retries.
        // Only transient rate-limit failures are retried with exponential
        // backoff; every other failure still fails fast on the first attempt.
        let rl_config = RateLimitConfig::from_env();
        let total_executions = rl_config.max_retries.saturating_add(1);

        let mut attempt: u32 = 1;
        loop {
            let is_final_attempt = attempt == total_executions;
            // On the final attempt, optionally force `--model auto` as the
            // rate-limit message suggests (opt-in via env, overrides `model`).
            let effective_model: Option<&str> = if is_final_attempt && rl_config.fallback_auto_model
            {
                Some("auto")
            } else {
                model
            };

            // Create output log file in temp dir (not in repo to avoid polluting it).
            // The attempt number keeps per-attempt filenames unique.
            let output_dir = output_log_dir.join(".recipe-output");
            std::fs::create_dir_all(&output_dir)?;
            let output_file = output_dir.join(format!(
                "agent-step-{}-{}.log",
                attempt,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));

            let log_fh = std::fs::File::create(&output_file)?;

            // Capture stderr to a persistent file in /tmp so that on failure
            // we can include the agent's actual error output in the bail message
            // (the temp_dir gets cleaned up before the error is reported).
            let stderr_persist_dir = std::path::PathBuf::from("/tmp/amplihack-agent-stderr");
            if let Err(e) = std::fs::create_dir_all(&stderr_persist_dir) {
                log::warn!(
                    "Failed to create stderr persist dir {}: {}",
                    stderr_persist_dir.display(),
                    e
                );
            }
            let stderr_file = stderr_persist_dir.join(format!(
                "agent-stderr-{}-{}.log",
                attempt,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            let stderr_fh = std::fs::File::create(&stderr_file)?;

            // Always launch via `amplihack <agent>` so the amplihack infrastructure
            // (env setup, guards, hooks) is properly initialized.
            // The agent runs from the real repo/worktree cwd, not a temp dir.
            let mut cmd = self.build_agent_command(
                &output_dir,
                &resolved_cwd,
                &full_prompt,
                system_prompt,
                effective_model,
            )?;
            let mut child = cmd
                .current_dir(&resolved_cwd)
                .env_remove("CLAUDECODE")
                .envs(&child_env)
                .stdout(log_fh)
                .stderr(stderr_fh)
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
            let agent_label = self.cli.clone();

            let heartbeat = std::thread::spawn(move || {
                // Issue #52: stream all new bytes between ticks (not just the
                // last line) and prefix each line with `[HH:MM:SS] [agent:pid]`
                // so operators can correlate with external logs.
                let mut last_size = 0u64;
                let mut last_activity = Instant::now();
                let start_time = Instant::now();
                let label = format!("amplihack:{}:{}", agent_label, child_pid);
                while !stop_clone.load(Ordering::Relaxed) {
                    match std::fs::metadata(&output_path) {
                        Ok(meta) => {
                            let current_size = meta.len();
                            if current_size > last_size {
                                // Read the *new* bytes since last tick (file-seek
                                // semantics) so no output is silently dropped.
                                match std::fs::File::open(&output_path) {
                                    Ok(mut file) => {
                                        use std::io::{Seek, SeekFrom};
                                        if file.seek(SeekFrom::Start(last_size)).is_ok() {
                                            let mut buf = String::new();
                                            if let Err(e) = file.read_to_string(&mut buf) {
                                                log::debug!(
                                                    "heartbeat: read_to_string failed: {}",
                                                    e
                                                );
                                            }
                                            for line in buf.lines() {
                                                if line.is_empty() {
                                                    continue;
                                                }
                                                eprintln!("  [{}] [{}] {}", utc_hms(), label, line);
                                            }
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
                                        "  [{}] [{}] ... working ({}s elapsed, {}s since last output)",
                                        utc_hms(),
                                        label,
                                        total_elapsed,
                                        idle_secs
                                    );
                                } else {
                                    eprintln!(
                                        "  [{}] [{}] ... waiting ({}s elapsed, process may be finishing)",
                                        utc_hms(),
                                        label,
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

            // Enforce timeout: poll in a loop instead of blocking on wait().
            let status = if let Some(secs) = timeout {
                let deadline = Instant::now() + Duration::from_secs(secs);
                loop {
                    match child.try_wait()? {
                        Some(s) => break s,
                        None if Instant::now() >= deadline => {
                            log::warn!(
                                "Agent step timed out after {}s — killing pid {}",
                                secs,
                                child.id()
                            );
                            let pid = child.id();
                            child.kill().ok();
                            // Reap the zombie so we don't leak processes.
                            let _ = child.wait();
                            stop.store(true, Ordering::SeqCst);
                            let _ = heartbeat.join();
                            anyhow::bail!(
                                "Agent step timed out after {}s (killed pid {})",
                                secs,
                                pid
                            );
                        }
                        None => std::thread::sleep(Duration::from_millis(250)),
                    }
                }
            } else {
                child.wait()?
            };
            stop.store(true, Ordering::SeqCst);
            if let Err(e) = heartbeat.join() {
                log::warn!("Heartbeat thread panicked: {:?}", e);
            }

            // Read agent output with retry and graceful fallback (#3740).
            // The output file can be missing if: the temp dir was cleaned by the OS,
            // the agent crashed before writing, or a race condition on fast exits.
            // Bounded read at MAX_STEP_OUTPUT_BYTES + 1 (#47): protects against
            // unbounded memory growth from runaway agent processes; the runner
            // detects len > MAX and applies safe_truncate as the final policy step.
            let max_out = crate::runner::MAX_STEP_OUTPUT_BYTES;
            let stdout = match read_capped(&output_file, max_out) {
                Ok(content) => content,
                Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
                    log::warn!(
                        "Agent output file not found: {}. Retrying after 1s...",
                        output_file.display()
                    );
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    match read_capped(&output_file, max_out) {
                        Ok(content) => {
                            log::info!("Agent output file found on retry");
                            content
                        }
                        Err(_) => {
                            log::error!(
                                "Agent output file missing after retry: {}. \
                             Continuing with empty output instead of aborting.",
                                output_file.display()
                            );
                            String::new()
                        }
                    }
                }
                Err(e) => {
                    log::error!(
                        "Failed to read agent output file {}: {}. Continuing with empty output.",
                        output_file.display(),
                        e
                    );
                    String::new()
                }
            };

            // temp_dir is dropped after the loop exits, cleaning up automatically.

            if status.success() {
                // On success, remove the persistent stderr file.
                if let Err(e) = std::fs::remove_file(&stderr_file) {
                    log::debug!(
                        "Failed to clean up stderr file {}: {}",
                        stderr_file.display(),
                        e
                    );
                }
                return Ok(stdout.trim().to_string());
            }

            // Non-zero exit: read the stderr tail for diagnostics + detection.
            let stderr_tail = std::fs::read_to_string(&stderr_file)
                .map(|s| crate::safe_tail(&s, 4000).to_string())
                .unwrap_or_else(|_| String::from("(stderr file unreadable)"));

            // Only retry transient rate-limit failures (#839). Every other
            // failure (auth, logic, missing binary, ...) must fail fast, exactly
            // as before — never blanket-retry all errors. Scan stdout and the
            // (small) stderr tail separately to avoid concatenating a fresh
            // multi-MB copy of stdout just for detection.
            if is_rate_limit(&stdout) || is_rate_limit(&stderr_tail) {
                if attempt < total_executions {
                    let wait =
                        backoff_delay(attempt, rl_config.base_delay_secs, rl_config.max_delay_secs);
                    // Surface the backoff LOUDLY to stderr — never silent.
                    eprintln!(
                        "  [{}] [amplihack:{}] RATE LIMIT detected (exit {}); backing off {}s \
                         then retrying (attempt {} of {})...",
                        utc_hms(),
                        self.cli,
                        status.code().unwrap_or(-1),
                        wait.as_secs(),
                        attempt + 1,
                        total_executions
                    );
                    log::warn!(
                        "Rate limit detected on attempt {}/{}; waiting {}s before retry",
                        attempt,
                        total_executions,
                        wait.as_secs()
                    );
                    // Remove this failed attempt's stderr file before retrying
                    // so transient files don't leak across attempts.
                    if let Err(e) = std::fs::remove_file(&stderr_file) {
                        log::debug!(
                            "Failed to clean up stderr file {}: {}",
                            stderr_file.display(),
                            e
                        );
                    }
                    if !wait.is_zero() {
                        std::thread::sleep(wait);
                    }
                    attempt += 1;
                    continue;
                }

                // Retries exhausted on a persistent rate limit: fail explicitly
                // with a clear message — never loop forever.
                anyhow::bail!(
                    "amplihack {} failed: rate limit persisted after {} retries \
                     ({} total attempts, last exit {})\n--- stdout (tail) ---\n{}\n\
                     --- stderr (tail) ---\n{}\n--- stderr-log: {}",
                    self.cli,
                    rl_config.max_retries,
                    total_executions,
                    status.code().unwrap_or(-1),
                    crate::safe_tail(&stdout, 2000),
                    stderr_tail,
                    stderr_file.display()
                );
            }

            // Non-rate-limit failure: preserve the existing fail-fast message.
            anyhow::bail!(
                "amplihack {} failed (exit {})\n--- stdout (tail) ---\n{}\n--- stderr (tail) ---\n{}\n--- stderr-log: {}",
                self.cli,
                status.code().unwrap_or(-1),
                crate::safe_tail(&stdout, 2000),
                stderr_tail,
                stderr_file.display()
            );
        }
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
        working_dir: &str,
        model: Option<&str>,
        timeout: Option<u64>,
    ) -> Result<String, anyhow::Error> {
        log::debug!(
            "CLISubprocessAdapter::execute_agent_step: prompt_len={}, model={:?}, timeout={:?}, working_dir={:?}",
            prompt.len(),
            model,
            timeout,
            working_dir
        );
        self.execute_agent_step_impl(prompt, system_prompt, model, working_dir, timeout)
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

        // Issue #80: argv + env must fit in ARG_MAX (~128 KiB on Linux). For
        // large bash scripts (e.g. cleanup-helper / complete-session steps that
        // accumulate round-results across multiple parallel workstreams), the
        // inline `-c` form fails with `Argument list too long (os error 7)`.
        // Spill the script to a tempfile and execute it as a script file.
        const BASH_INLINE_LIMIT: usize = 64 * 1024;
        let script_file: Option<tempfile::NamedTempFile> = if command.len() > BASH_INLINE_LIMIT {
            let mut tf = tempfile::Builder::new()
                .prefix("recipe-bash-step-")
                .suffix(".sh")
                .tempfile()
                .with_context(|| "Failed to create tempfile for large bash step")?;
            tf.write_all(command.as_bytes())
                .with_context(|| "Failed to write large bash step to tempfile")?;
            tf.flush()
                .with_context(|| "Failed to flush large bash step tempfile")?;
            Some(tf)
        } else {
            None
        };

        let output = match (&script_file, timeout) {
            (Some(tf), Some(secs)) => Command::new("timeout")
                .args([
                    secs.to_string().as_str(),
                    "/bin/bash",
                    tf.path().to_str().unwrap_or(""),
                ])
                .current_dir(effective_dir)
                .env_remove("CLAUDECODE")
                .envs(&child_env)
                .envs(extra_env)
                .output()
                .with_context(|| "Failed to execute file-backed bash step with timeout")?,
            (Some(tf), None) => Command::new("/bin/bash")
                .arg(tf.path())
                .current_dir(effective_dir)
                .env_remove("CLAUDECODE")
                .envs(&child_env)
                .envs(extra_env)
                .output()
                .with_context(|| "Failed to execute file-backed bash step")?,
            (None, Some(secs)) => Command::new("timeout")
                .args([&secs.to_string(), "/bin/bash", "-c", command])
                .current_dir(effective_dir)
                .env_remove("CLAUDECODE")
                .envs(&child_env)
                .envs(extra_env)
                .output()
                .with_context(|| "Failed to execute bash step with timeout")?,
            (None, None) => Command::new("/bin/bash")
                .args(["-c", command])
                .current_dir(effective_dir)
                .env_remove("CLAUDECODE")
                .envs(&child_env)
                .envs(extra_env)
                .output()
                .with_context(|| "Failed to execute bash step")?,
        };

        // Drop the tempfile (auto-cleans on drop) only AFTER bash completed.
        drop(script_file);

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

    // ── #47: read_capped enforces MAX+1 byte cap on agent output ──────

    #[test]
    fn test_read_capped_returns_full_content_under_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("small.txt");
        let payload = "hello world";
        std::fs::write(&path, payload).unwrap();
        let out = read_capped(&path, 1024).expect("small read must succeed");
        assert_eq!(out, payload);
    }

    #[test]
    fn test_read_capped_caps_at_max_plus_one_byte() {
        // Writes a file > MAX bytes; read_capped must return AT MOST MAX+1
        // bytes of UTF-8 (the +1 is the sentinel that signals "overflow").
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("big.txt");
        let max = 1024usize;
        let oversize = max + 5000;
        let payload = vec![b'x'; oversize];
        std::fs::write(&path, &payload).unwrap();
        let out = read_capped(&path, max).expect("read must not error on oversize");
        assert!(
            out.len() <= max + 1,
            "read_capped returned {} bytes, must be <= MAX+1 ({})",
            out.len(),
            max + 1
        );
        assert!(
            out.len() > max,
            "read_capped returned {} bytes, must exceed MAX so caller can detect truncation",
            out.len()
        );
    }

    #[test]
    fn test_read_capped_missing_file_propagates_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nonexistent.txt");
        assert!(read_capped(&path, 1024).is_err());
    }

    #[test]
    fn test_read_capped_at_agent_output_limit_is_truncated() {
        // Integration-style: write >MAX_STEP_OUTPUT_BYTES and confirm the
        // bounded reader caps memory growth at the runner's limit.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agent.out");
        // Use 11 MB written in chunks to avoid a 10MB+ Vec literal.
        let max = crate::runner::MAX_STEP_OUTPUT_BYTES;
        let chunk = vec![b'a'; 1_000_000];
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&path).unwrap();
            for _ in 0..(max / chunk.len() + 1) {
                f.write_all(&chunk).unwrap();
            }
        }
        let out = read_capped(&path, max).unwrap();
        assert!(out.len() <= max + 1, "must cap at MAX+1; got {}", out.len());
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
    fn test_build_child_env_guarantees_home_and_path() {
        // Fix #277 parity with amplihack-rs fork: HOME and PATH must always be
        // present and non-empty, even when unset in parent.
        let env = CLISubprocessAdapter::build_child_env();
        assert!(env.contains_key("HOME"));
        assert!(env.contains_key("PATH"));
        assert!(!env.get("HOME").unwrap().is_empty());
        assert!(!env.get("PATH").unwrap().is_empty());
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
    fn test_execute_bash_step_large_script_uses_tempfile() {
        // Issue #80: scripts > BASH_INLINE_LIMIT (64 KiB) must be spilled to a
        // tempfile to avoid `Argument list too long (os error 7)`. Generate a
        // ~96 KiB script that prints a sentinel — the inline `-c` form would
        // crash on a system already near ARG_MAX, while the file-backed path
        // executes cleanly.
        let adapter = CLISubprocessAdapter::new();
        let empty_env = std::collections::HashMap::new();
        let mut large_script = String::with_capacity(100 * 1024);
        // Pad with comment lines so the script size grows without changing its
        // observable behavior. Each comment line is ~80 bytes; need ~1200 lines.
        for i in 0..1300 {
            large_script.push_str(&format!(
                "# padding line {i:04} aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
            ));
        }
        large_script.push_str("echo SENTINEL_OK\n");
        assert!(
            large_script.len() > 64 * 1024,
            "test script must exceed BASH_INLINE_LIMIT, got {}",
            large_script.len()
        );

        let result = adapter.execute_bash_step(&large_script, ".", None, &empty_env);
        assert!(
            result.is_ok(),
            "large bash step should succeed via tempfile: {:?}",
            result
        );
        assert_eq!(result.unwrap(), "SENTINEL_OK");
    }

    #[test]
    fn test_execute_bash_step_large_script_with_timeout() {
        // Same as above but exercising the (Some(tf), Some(secs)) match arm.
        let adapter = CLISubprocessAdapter::new();
        let empty_env = std::collections::HashMap::new();
        let mut large_script = String::with_capacity(100 * 1024);
        for i in 0..1300 {
            large_script.push_str(&format!(
                "# padding line {i:04} aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
            ));
        }
        large_script.push_str("echo SENTINEL_TIMED\n");
        let result = adapter.execute_bash_step(&large_script, ".", Some(10), &empty_env);
        assert!(
            result.is_ok(),
            "large timed bash step should succeed via tempfile: {:?}",
            result
        );
        assert_eq!(result.unwrap(), "SENTINEL_TIMED");
    }

    #[test]
    fn test_non_interactive_footer_constant() {
        assert!(NON_INTERACTIVE_FOOTER.contains("autonomously"));
        assert!(NON_INTERACTIVE_FOOTER.contains("Do not ask questions"));
    }

    #[test]
    fn test_no_reentry_system_prompt_constant() {
        assert!(RECIPE_CHILD_NO_REENTRY_SYSTEM_PROMPT.contains("/dev"));
        assert!(RECIPE_CHILD_NO_REENTRY_SYSTEM_PROMPT.contains("smart-orchestrator"));
        assert!(
            RECIPE_CHILD_NO_REENTRY_SYSTEM_PROMPT.contains("Execute the requested step directly")
        );
    }

    #[test]
    fn test_build_effective_system_prompt_without_existing_prompt() {
        let prompt = CLISubprocessAdapter::build_effective_system_prompt(None);
        assert_eq!(prompt, RECIPE_CHILD_NO_REENTRY_SYSTEM_PROMPT);
    }

    #[test]
    fn test_build_effective_system_prompt_with_existing_prompt() {
        let prompt =
            CLISubprocessAdapter::build_effective_system_prompt(Some("Existing system prompt"));
        assert!(prompt.starts_with("Existing system prompt"));
        assert!(prompt.contains(RECIPE_CHILD_NO_REENTRY_SYSTEM_PROMPT));
    }

    #[test]
    fn test_build_agent_command_always_includes_system_prompt() {
        // Claude CLI gets --system-prompt; Copilot does not
        let adapter = CLISubprocessAdapter::new().with_binary("claude");
        let tmp = tempfile::tempdir().unwrap();
        let cmd = adapter
            .build_agent_command(tmp.path(), tmp.path(), "hello", None, None)
            .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.windows(2).any(|w| {
            w[0] == "--system-prompt" && w[1].contains(RECIPE_CHILD_NO_REENTRY_SYSTEM_PROMPT)
        }));
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

    #[test]
    fn test_should_use_file_backed_prompt_transport_for_large_claude_prompt() {
        let adapter = CLISubprocessAdapter::new().with_binary("claude");
        let large_prompt = "x".repeat(MAX_INLINE_AGENT_PROMPT_BYTES + 1);
        assert!(adapter.should_use_file_backed_prompt_transport(&large_prompt, None));
    }

    #[test]
    fn test_uses_file_backed_prompt_transport_for_copilot() {
        // Copilot now also uses file-backed transport for large prompts
        let adapter = CLISubprocessAdapter::new().with_binary("copilot");
        let large_prompt = "x".repeat(MAX_INLINE_AGENT_PROMPT_BYTES + 1);
        assert!(adapter.should_use_file_backed_prompt_transport(&large_prompt, None));
    }

    #[test]
    fn test_build_agent_command_inlines_small_prompts() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().join(".recipe-output");
        std::fs::create_dir_all(&output_dir).unwrap();

        let adapter = CLISubprocessAdapter::new().with_binary("claude");
        let cmd = adapter
            .build_agent_command(
                &output_dir,
                tmp.path(),
                "short prompt",
                Some("system"),
                None,
            )
            .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert!(args.iter().any(|arg| arg == "-p"));
        assert!(args.iter().any(|arg| arg == "--system-prompt"));
        assert!(!args.iter().any(|arg| arg == "--append-system-prompt"));
    }

    #[test]
    fn test_build_agent_command_uses_file_transport_for_large_claude_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().join(".recipe-output");
        std::fs::create_dir_all(&output_dir).unwrap();

        let adapter = CLISubprocessAdapter::new().with_binary("claude");
        let large_prompt = format!(
            "Task: implement prompt transport.\nOutput: json.\n\n{}",
            "x".repeat(MAX_INLINE_AGENT_PROMPT_BYTES + 1)
        );
        let prompt_with_footer = format!("{}{}", large_prompt, NON_INTERACTIVE_FOOTER);
        let cmd = adapter
            .build_agent_command(
                &output_dir,
                tmp.path(),
                &prompt_with_footer,
                Some("system"),
                None,
            )
            .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert!(args.iter().any(|arg| arg == "--append-system-prompt"));
        assert!(!args.iter().any(|arg| arg == "--system-prompt"));

        let prompt_arg_index = args.iter().position(|arg| arg == "-p").unwrap();
        let inline_prompt = &args[prompt_arg_index + 1];
        assert!(inline_prompt.starts_with("Task: implement prompt transport."));
        assert!(inline_prompt.contains(FILE_BACKED_PROMPT_CONTINUATION_NOTE));
        assert!(inline_prompt.len() < prompt_with_footer.len());

        let prompt_file_index = args
            .iter()
            .position(|arg| arg == "--append-system-prompt")
            .unwrap();
        let prompt_file = std::path::PathBuf::from(&args[prompt_file_index + 1]);
        let prompt_file_contents = std::fs::read_to_string(&prompt_file).unwrap();
        assert!(prompt_file_contents.contains("system"));
        assert!(prompt_file_contents.contains("# Continued task instructions"));
        assert!(prompt_file_contents.contains(NON_INTERACTIVE_FOOTER));
        assert!(prompt_file_contents.contains(&"x".repeat(1024)));
        assert!(prompt_file_contents.contains(NON_INTERACTIVE_FOOTER));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = std::fs::metadata(&prompt_file)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn test_large_system_prompt_keeps_full_user_prompt_inline() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().join(".recipe-output");
        std::fs::create_dir_all(&output_dir).unwrap();

        let adapter = CLISubprocessAdapter::new().with_binary("claude");
        let prompt = "Task: audit this workflow.";
        let large_system_prompt = "s".repeat(MAX_INLINE_AGENT_PROMPT_BYTES + 1);
        let cmd = adapter
            .build_agent_command(
                &output_dir,
                tmp.path(),
                prompt,
                Some(&large_system_prompt),
                None,
            )
            .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        let prompt_arg_index = args.iter().position(|arg| arg == "-p").unwrap();
        assert_eq!(args[prompt_arg_index + 1], prompt);

        let prompt_file_index = args
            .iter()
            .position(|arg| arg == "--append-system-prompt")
            .unwrap();
        let prompt_file = std::path::PathBuf::from(&args[prompt_file_index + 1]);
        let prompt_file_contents = std::fs::read_to_string(&prompt_file).unwrap();
        assert!(prompt_file_contents.starts_with(&large_system_prompt));
        assert!(prompt_file_contents.contains(RECIPE_CHILD_NO_REENTRY_SYSTEM_PROMPT));
    }

    #[test]
    fn test_build_agent_command_includes_separator_before_passthrough_args() {
        // All CLIs require `--` separator between `amplihack <agent>` flags
        // and passthrough args like `-p`, `--model`, `--add-dir` (#4342).
        for binary in &["claude", "copilot", "codex", "launch"] {
            let adapter = CLISubprocessAdapter::new().with_binary(binary);
            let tmp = tempfile::tempdir().unwrap();
            let cmd = adapter
                .build_agent_command(tmp.path(), tmp.path(), "hello", None, None)
                .unwrap();
            let args: Vec<String> = cmd
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();

            // First arg is the agent name, second must be "--"
            assert_eq!(args[0], *binary, "first arg should be the agent binary");
            assert_eq!(
                args[1], "--",
                "second arg must be '--' separator for {binary}"
            );

            // `-p` must come after the separator
            let separator_pos = args.iter().position(|a| a == "--").unwrap();
            let p_pos = args.iter().position(|a| a == "-p").unwrap();
            assert!(
                p_pos > separator_pos,
                "-p must come after -- separator for {binary}"
            );
        }
    }

    #[test]
    fn test_build_agent_command_separator_with_model_flag() {
        let adapter = CLISubprocessAdapter::new().with_binary("copilot");
        let tmp = tempfile::tempdir().unwrap();
        let cmd = adapter
            .build_agent_command(tmp.path(), tmp.path(), "hello", None, Some("gpt-4"))
            .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        let separator_pos = args.iter().position(|a| a == "--").unwrap();
        let model_pos = args.iter().position(|a| a == "--model").unwrap();
        assert!(
            model_pos > separator_pos,
            "--model must come after -- separator"
        );
        assert_eq!(args[model_pos + 1], "gpt-4");
    }

    #[test]
    fn test_build_agent_command_copilot_includes_allow_all_tools() {
        // Copilot needs --allow-all-tools to run non-interactively without
        // prompting for tool approval (#88). The flag must come after `--`
        // so it is passed through to the copilot CLI.
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::new("AMPLIHACK_NO_ALLOW_ALL_TOOLS");
        // SAFETY: test runs hold ENV_MUTEX to serialize env var access
        unsafe {
            env::remove_var("AMPLIHACK_NO_ALLOW_ALL_TOOLS");
        }

        let adapter = CLISubprocessAdapter::new().with_binary("copilot");
        let tmp = tempfile::tempdir().unwrap();
        let cmd = adapter
            .build_agent_command(tmp.path(), tmp.path(), "hello", None, None)
            .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let separator_pos = args.iter().position(|a| a == "--").unwrap();
        let allow_pos = args
            .iter()
            .position(|a| a == "--allow-all-tools")
            .expect("copilot command must include --allow-all-tools");
        assert!(
            allow_pos > separator_pos,
            "--allow-all-tools must come after -- separator"
        );
    }

    #[test]
    fn test_build_agent_command_non_copilot_omits_allow_all_tools() {
        // Only copilot needs --allow-all-tools; claude/codex/launch must not get it.
        for binary in &["claude", "codex", "launch", "RustyClawd"] {
            let adapter = CLISubprocessAdapter::new().with_binary(binary);
            let tmp = tempfile::tempdir().unwrap();
            let cmd = adapter
                .build_agent_command(tmp.path(), tmp.path(), "hello", None, None)
                .unwrap();
            let args: Vec<String> = cmd
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert!(
                !args.iter().any(|a| a == "--allow-all-tools"),
                "{binary} must not include --allow-all-tools"
            );
        }
    }

    #[test]
    fn test_build_agent_command_copilot_opt_out_via_env() {
        // Setting AMPLIHACK_NO_ALLOW_ALL_TOOLS to any non-empty value
        // suppresses the auto-injected --allow-all-tools flag.
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::new("AMPLIHACK_NO_ALLOW_ALL_TOOLS");
        // SAFETY: test runs hold ENV_MUTEX to serialize env var access
        unsafe {
            env::set_var("AMPLIHACK_NO_ALLOW_ALL_TOOLS", "1");
        }

        let adapter = CLISubprocessAdapter::new().with_binary("copilot");
        let tmp = tempfile::tempdir().unwrap();
        let cmd = adapter
            .build_agent_command(tmp.path(), tmp.path(), "hello", None, None)
            .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.iter().any(|a| a == "--allow-all-tools"),
            "AMPLIHACK_NO_ALLOW_ALL_TOOLS=1 must suppress --allow-all-tools"
        );
    }

    #[test]
    fn test_build_agent_command_copilot_empty_env_keeps_flag() {
        // Empty string should NOT count as opt-out — flag stays.
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::new("AMPLIHACK_NO_ALLOW_ALL_TOOLS");
        // SAFETY: test runs hold ENV_MUTEX to serialize env var access
        unsafe {
            env::set_var("AMPLIHACK_NO_ALLOW_ALL_TOOLS", "");
        }

        let adapter = CLISubprocessAdapter::new().with_binary("copilot");
        let tmp = tempfile::tempdir().unwrap();
        let cmd = adapter
            .build_agent_command(tmp.path(), tmp.path(), "hello", None, None)
            .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.iter().any(|a| a == "--allow-all-tools"),
            "empty AMPLIHACK_NO_ALLOW_ALL_TOOLS must not suppress --allow-all-tools"
        );
    }

    // ════════════════════════════════════════════════════════════════════
    // Issue #839: rate-limit detection + bounded retry/backoff
    //
    // These tests are written test-first (TDD) and DEFINE the contract for
    // the implementation. They exercise:
    //   * `is_rate_limit(&str) -> bool`           — pure signal detector
    //   * `backoff_delay(retry, base, cap)`        — pure backoff math
    //   * `RateLimitConfig::from_env()`            — env-driven config
    //   * `execute_agent_step_impl` retry loop     — integration behavior
    //   * `AMPLIHACK_LAUNCHER_BINARY` override      — test-only launcher inject
    //
    // Backoff delays are made instant via AMPLIHACK_RATELIMIT_BASE_DELAY_SECS=0
    // so the integration tests run fast.
    // ════════════════════════════════════════════════════════════════════

    // ── Unit: is_rate_limit signal detection (case-insensitive) ──────────

    #[test]
    fn test_is_rate_limit_detects_hit_your_rate_limit() {
        // The canonical Copilot enterprise message from issue #839.
        let msg = "You've hit your rate limit. Please wait for your limit to \
                   reset in under a minute or switch to auto model to continue.";
        assert!(is_rate_limit(msg));
    }

    #[test]
    fn test_is_rate_limit_detects_all_signal_phrases() {
        // Every documented signal phrase must match (case-insensitive).
        let cases = [
            "hit your rate limit",
            "please wait for your limit to RESET IN under a minute",
            "Rate Limit exceeded",
            "HTTP 429 Too Many Requests",
            "429",
            "too many requests",
        ];
        for c in cases {
            assert!(is_rate_limit(c), "expected rate-limit signal in: {c:?}");
        }
    }

    #[test]
    fn test_is_rate_limit_is_case_insensitive() {
        assert!(is_rate_limit("HIT YOUR RATE LIMIT"));
        assert!(is_rate_limit("hit your RATE limit"));
    }

    #[test]
    fn test_is_rate_limit_rejects_non_rate_limit_failures() {
        // Genuine failures that must NOT be treated as rate limits — these
        // have to keep failing fast (no retry).
        let non_matches = [
            "error: authentication failed (401 Unauthorized)",
            "panic: index out of bounds",
            "command not found: amplihack",
            "compilation error: expected `;`",
            "permission denied",
            "",
        ];
        for c in non_matches {
            assert!(!is_rate_limit(c), "must NOT flag as rate-limit: {c:?}");
        }
    }

    // ── Unit: backoff_delay exponential math with cap + saturation ───────

    #[test]
    fn test_backoff_delay_exponential_progression() {
        // delay(retry) = min(base * 2^(retry-1), cap). retry starts at 1.
        let base = 60u64;
        let cap = 600u64;
        assert_eq!(backoff_delay(1, base, cap).as_secs(), 60);
        assert_eq!(backoff_delay(2, base, cap).as_secs(), 120);
        assert_eq!(backoff_delay(3, base, cap).as_secs(), 240);
        assert_eq!(backoff_delay(4, base, cap).as_secs(), 480);
    }

    #[test]
    fn test_backoff_delay_respects_cap() {
        // The 5th retry would be 960s but must be clamped to the 600s cap.
        assert_eq!(backoff_delay(5, 60, 600).as_secs(), 600);
        assert_eq!(backoff_delay(6, 60, 600).as_secs(), 600);
    }

    #[test]
    fn test_backoff_delay_zero_base_is_instant() {
        // base = 0 (used by tests) makes every wait instant.
        for retry in 1..=5 {
            assert_eq!(
                backoff_delay(retry, 0, 600).as_secs(),
                0,
                "base=0 must yield zero delay for retry {retry}"
            );
        }
    }

    #[test]
    fn test_backoff_delay_saturates_without_overflow() {
        // A hostile/large retry count must never panic or overflow; the
        // result is always clamped to the cap.
        let d = backoff_delay(1000, 60, 600);
        assert_eq!(d.as_secs(), 600, "extreme retry must saturate to cap");
    }

    // ── Unit: RateLimitConfig::from_env defaults, overrides, clamping ────

    #[test]
    fn test_rate_limit_config_defaults_when_unset() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _g1 = EnvGuard::new("AMPLIHACK_RATELIMIT_MAX_RETRIES");
        let _g2 = EnvGuard::new("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS");
        let _g3 = EnvGuard::new("AMPLIHACK_RATELIMIT_MAX_DELAY_SECS");
        let _g4 = EnvGuard::new("AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL");
        // SAFETY: test holds ENV_MUTEX to serialize env var access
        unsafe {
            env::remove_var("AMPLIHACK_RATELIMIT_MAX_RETRIES");
            env::remove_var("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS");
            env::remove_var("AMPLIHACK_RATELIMIT_MAX_DELAY_SECS");
            env::remove_var("AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL");
        }

        let cfg = RateLimitConfig::from_env();
        assert_eq!(cfg.max_retries, 5);
        assert_eq!(cfg.base_delay_secs, 60);
        assert_eq!(cfg.max_delay_secs, 600);
        assert!(!cfg.fallback_auto_model);
    }

    #[test]
    fn test_rate_limit_config_reads_overrides() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _g1 = EnvGuard::new("AMPLIHACK_RATELIMIT_MAX_RETRIES");
        let _g2 = EnvGuard::new("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS");
        let _g3 = EnvGuard::new("AMPLIHACK_RATELIMIT_MAX_DELAY_SECS");
        let _g4 = EnvGuard::new("AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL");
        // SAFETY: test holds ENV_MUTEX to serialize env var access
        unsafe {
            env::set_var("AMPLIHACK_RATELIMIT_MAX_RETRIES", "8");
            env::set_var("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS", "0");
            env::set_var("AMPLIHACK_RATELIMIT_MAX_DELAY_SECS", "120");
            env::set_var("AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL", "1");
        }

        let cfg = RateLimitConfig::from_env();
        assert_eq!(cfg.max_retries, 8);
        assert_eq!(cfg.base_delay_secs, 0);
        assert_eq!(cfg.max_delay_secs, 120);
        assert!(cfg.fallback_auto_model);
    }

    #[test]
    fn test_rate_limit_config_unparseable_falls_back_to_default() {
        // Garbage values must fall back to defaults, never panic or fail the run.
        let _lock = ENV_MUTEX.lock().unwrap();
        let _g1 = EnvGuard::new("AMPLIHACK_RATELIMIT_MAX_RETRIES");
        let _g2 = EnvGuard::new("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS");
        // SAFETY: test holds ENV_MUTEX to serialize env var access
        unsafe {
            env::set_var("AMPLIHACK_RATELIMIT_MAX_RETRIES", "not-a-number");
            env::set_var("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS", "");
        }

        let cfg = RateLimitConfig::from_env();
        assert_eq!(cfg.max_retries, 5, "bad max_retries must use default 5");
        assert_eq!(cfg.base_delay_secs, 60, "empty base must use default 60");
    }

    #[test]
    fn test_rate_limit_config_clamps_max_retries_to_ceiling() {
        // A hostile MAX_RETRIES must be clamped to the hard ceiling (100) so
        // the worst-case execution budget stays bounded.
        let _lock = ENV_MUTEX.lock().unwrap();
        let _g = EnvGuard::new("AMPLIHACK_RATELIMIT_MAX_RETRIES");
        // SAFETY: test holds ENV_MUTEX to serialize env var access
        unsafe {
            env::set_var("AMPLIHACK_RATELIMIT_MAX_RETRIES", "100000");
        }

        let cfg = RateLimitConfig::from_env();
        assert!(
            cfg.max_retries <= 100,
            "max_retries must be clamped to <=100, got {}",
            cfg.max_retries
        );
    }

    #[test]
    fn test_rate_limit_config_enforces_max_delay_ge_base() {
        // If a user sets max_delay < base_delay, the config must raise max_delay
        // to at least base_delay so the backoff formula never inverts.
        let _lock = ENV_MUTEX.lock().unwrap();
        let _g1 = EnvGuard::new("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS");
        let _g2 = EnvGuard::new("AMPLIHACK_RATELIMIT_MAX_DELAY_SECS");
        // SAFETY: test holds ENV_MUTEX to serialize env var access
        unsafe {
            env::set_var("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS", "300");
            env::set_var("AMPLIHACK_RATELIMIT_MAX_DELAY_SECS", "100");
        }

        let cfg = RateLimitConfig::from_env();
        assert!(
            cfg.max_delay_secs >= cfg.base_delay_secs,
            "max_delay ({}) must be >= base_delay ({})",
            cfg.max_delay_secs,
            cfg.base_delay_secs
        );
    }

    // ── Unit: AMPLIHACK_LAUNCHER_BINARY override in build_agent_command ──

    #[test]
    fn test_build_agent_command_uses_launcher_override() {
        // When AMPLIHACK_LAUNCHER_BINARY is set, the spawned program must be
        // that path instead of the hardcoded "amplihack".
        let _lock = ENV_MUTEX.lock().unwrap();
        let _g = EnvGuard::new("AMPLIHACK_LAUNCHER_BINARY");
        // SAFETY: test holds ENV_MUTEX to serialize env var access
        unsafe {
            env::set_var("AMPLIHACK_LAUNCHER_BINARY", "/path/to/fake-launcher");
        }

        let adapter = CLISubprocessAdapter::new().with_binary("claude");
        let tmp = tempfile::tempdir().unwrap();
        let cmd = adapter
            .build_agent_command(tmp.path(), tmp.path(), "hello", None, None)
            .unwrap();
        assert_eq!(
            cmd.get_program().to_string_lossy(),
            "/path/to/fake-launcher",
            "launcher override must replace the 'amplihack' program"
        );
    }

    #[test]
    fn test_build_agent_command_defaults_to_amplihack_launcher() {
        // Unset override => production behavior unchanged (program == amplihack).
        let _lock = ENV_MUTEX.lock().unwrap();
        let _g = EnvGuard::new("AMPLIHACK_LAUNCHER_BINARY");
        // SAFETY: test holds ENV_MUTEX to serialize env var access
        unsafe {
            env::remove_var("AMPLIHACK_LAUNCHER_BINARY");
        }

        let adapter = CLISubprocessAdapter::new().with_binary("claude");
        let tmp = tempfile::tempdir().unwrap();
        let cmd = adapter
            .build_agent_command(tmp.path(), tmp.path(), "hello", None, None)
            .unwrap();
        assert_eq!(cmd.get_program().to_string_lossy(), "amplihack");
    }

    // ── Integration helpers: fake launcher injection ────────────────────

    /// Write an executable fake-launcher shell script at `path` (mode 0o755).
    ///
    /// The script increments a counter file (whose path is read from the
    /// `AMPLIHACK_TEST_RL_COUNTER` env var it inherits) on every invocation,
    /// then runs `body`. Inside `body`, `$c` is the 1-based invocation count
    /// and `$@` are the passthrough launcher args.
    fn write_fake_launcher(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let script = format!(
            "#!/usr/bin/env bash\n\
             set -u\n\
             f=\"$AMPLIHACK_TEST_RL_COUNTER\"\n\
             c=$(cat \"$f\" 2>/dev/null || echo 0)\n\
             c=$((c+1))\n\
             echo \"$c\" > \"$f\"\n\
             {body}\n"
        );
        std::fs::write(path, script).unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    const RATE_LIMIT_MSG: &str = "You've hit your rate limit. Please wait for your limit to reset in under a minute \
         or switch to auto model to continue.";

    // (a) Transient rate limit: fail on attempts 1-2, succeed on attempt 3.
    #[test]
    fn test_execute_agent_step_retries_then_succeeds_on_rate_limit() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _g_launcher = EnvGuard::new("AMPLIHACK_LAUNCHER_BINARY");
        let _g_base = EnvGuard::new("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS");
        let _g_retries = EnvGuard::new("AMPLIHACK_RATELIMIT_MAX_RETRIES");
        let _g_counter = EnvGuard::new("AMPLIHACK_TEST_RL_COUNTER");
        let _g_agent = EnvGuard::new("AMPLIHACK_AGENT_BINARY");

        let script_dir = tempfile::tempdir().unwrap();
        let counter_dir = tempfile::tempdir().unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        let launcher = script_dir.path().join("fake-launcher.sh");
        let counter_file = counter_dir.path().join("count");

        write_fake_launcher(
            &launcher,
            &format!(
                "if [ \"$c\" -le 2 ]; then\n\
                 echo \"{RATE_LIMIT_MSG}\" 1>&2\n\
                 exit 1\n\
                 fi\n\
                 echo \"AGENT_OK\"\n\
                 exit 0",
            ),
        );

        // SAFETY: test holds ENV_MUTEX to serialize env var access
        unsafe {
            env::set_var("AMPLIHACK_LAUNCHER_BINARY", launcher.to_str().unwrap());
            env::set_var("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS", "0");
            env::set_var("AMPLIHACK_RATELIMIT_MAX_RETRIES", "5");
            env::set_var("AMPLIHACK_TEST_RL_COUNTER", counter_file.to_str().unwrap());
            env::set_var("AMPLIHACK_AGENT_BINARY", "claude");
        }

        let adapter = CLISubprocessAdapter::new();
        let result = adapter.execute_agent_step_impl(
            "hello",
            None,
            None,
            work_dir.path().to_str().unwrap(),
            Some(60),
        );

        assert!(
            result.is_ok(),
            "step must succeed after transient rate limits, got: {result:?}"
        );
        assert!(
            result.unwrap().contains("AGENT_OK"),
            "successful output must come from the final (successful) attempt"
        );
        let count: u32 = std::fs::read_to_string(&counter_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(count, 3, "expected 2 throttled attempts + 1 success");
    }

    // (b) Persistent rate limit: retries exhausted => clear, bounded error.
    #[test]
    fn test_execute_agent_step_exhausts_retries_on_persistent_rate_limit() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _g_launcher = EnvGuard::new("AMPLIHACK_LAUNCHER_BINARY");
        let _g_base = EnvGuard::new("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS");
        let _g_retries = EnvGuard::new("AMPLIHACK_RATELIMIT_MAX_RETRIES");
        let _g_counter = EnvGuard::new("AMPLIHACK_TEST_RL_COUNTER");
        let _g_agent = EnvGuard::new("AMPLIHACK_AGENT_BINARY");

        let script_dir = tempfile::tempdir().unwrap();
        let counter_dir = tempfile::tempdir().unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        let launcher = script_dir.path().join("fake-launcher.sh");
        let counter_file = counter_dir.path().join("count");

        // Always throttle.
        write_fake_launcher(
            &launcher,
            &format!("echo \"{RATE_LIMIT_MSG}\" 1>&2\nexit 1"),
        );

        // SAFETY: test holds ENV_MUTEX to serialize env var access
        unsafe {
            env::set_var("AMPLIHACK_LAUNCHER_BINARY", launcher.to_str().unwrap());
            env::set_var("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS", "0");
            env::set_var("AMPLIHACK_RATELIMIT_MAX_RETRIES", "3");
            env::set_var("AMPLIHACK_TEST_RL_COUNTER", counter_file.to_str().unwrap());
            env::set_var("AMPLIHACK_AGENT_BINARY", "claude");
        }

        let adapter = CLISubprocessAdapter::new();
        let result = adapter.execute_agent_step_impl(
            "hello",
            None,
            None,
            work_dir.path().to_str().unwrap(),
            Some(60),
        );

        let err = result.expect_err("persistent rate limit must eventually fail");
        let msg = format!("{err:#}").to_lowercase();
        assert!(
            msg.contains("rate limit") && msg.contains("persist"),
            "exhaustion error must clearly state the rate limit persisted, got: {err:#}"
        );

        // Bounded: exactly 1 initial attempt + 3 retries = 4 executions.
        let count: u32 = std::fs::read_to_string(&counter_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(count, 4, "executions must be bounded to 1 + max_retries");
    }

    // (c) Non-rate-limit failure: must fail fast with NO retry.
    #[test]
    fn test_execute_agent_step_does_not_retry_non_rate_limit_failure() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _g_launcher = EnvGuard::new("AMPLIHACK_LAUNCHER_BINARY");
        let _g_base = EnvGuard::new("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS");
        let _g_retries = EnvGuard::new("AMPLIHACK_RATELIMIT_MAX_RETRIES");
        let _g_counter = EnvGuard::new("AMPLIHACK_TEST_RL_COUNTER");
        let _g_agent = EnvGuard::new("AMPLIHACK_AGENT_BINARY");

        let script_dir = tempfile::tempdir().unwrap();
        let counter_dir = tempfile::tempdir().unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        let launcher = script_dir.path().join("fake-launcher.sh");
        let counter_file = counter_dir.path().join("count");

        // Generic, non-rate-limit failure (e.g. an auth error).
        write_fake_launcher(
            &launcher,
            "echo \"error: authentication failed (401 Unauthorized)\" 1>&2\nexit 2",
        );

        // SAFETY: test holds ENV_MUTEX to serialize env var access
        unsafe {
            env::set_var("AMPLIHACK_LAUNCHER_BINARY", launcher.to_str().unwrap());
            env::set_var("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS", "0");
            env::set_var("AMPLIHACK_RATELIMIT_MAX_RETRIES", "5");
            env::set_var("AMPLIHACK_TEST_RL_COUNTER", counter_file.to_str().unwrap());
            env::set_var("AMPLIHACK_AGENT_BINARY", "claude");
        }

        let adapter = CLISubprocessAdapter::new();
        let result = adapter.execute_agent_step_impl(
            "hello",
            None,
            None,
            work_dir.path().to_str().unwrap(),
            Some(60),
        );

        let err = result.expect_err("non-rate-limit failure must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("failed (exit 2)"),
            "must preserve the existing fail-fast error message, got: {msg}"
        );
        let count: u32 = std::fs::read_to_string(&counter_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(count, 1, "non-rate-limit failures must NOT be retried");
    }

    // (d) Optional --model auto fallback on the final attempt.
    #[test]
    fn test_execute_agent_step_falls_back_to_model_auto_on_final_attempt() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _g_launcher = EnvGuard::new("AMPLIHACK_LAUNCHER_BINARY");
        let _g_base = EnvGuard::new("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS");
        let _g_retries = EnvGuard::new("AMPLIHACK_RATELIMIT_MAX_RETRIES");
        let _g_fallback = EnvGuard::new("AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL");
        let _g_counter = EnvGuard::new("AMPLIHACK_TEST_RL_COUNTER");
        let _g_agent = EnvGuard::new("AMPLIHACK_AGENT_BINARY");

        let script_dir = tempfile::tempdir().unwrap();
        let counter_dir = tempfile::tempdir().unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        let launcher = script_dir.path().join("fake-launcher.sh");
        let counter_file = counter_dir.path().join("count");

        // Throttle until the launcher is invoked with `--model auto`; only the
        // final retry (with fallback enabled) supplies it, so success proves
        // the fallback was applied on the last attempt.
        write_fake_launcher(
            &launcher,
            &format!(
                "has_auto=0\n\
                 for a in \"$@\"; do if [ \"$a\" = \"auto\" ]; then has_auto=1; fi; done\n\
                 if [ \"$has_auto\" = \"1\" ]; then\n\
                 echo \"AGENT_OK_AUTO\"\n\
                 exit 0\n\
                 fi\n\
                 echo \"{RATE_LIMIT_MSG}\" 1>&2\n\
                 exit 1",
            ),
        );

        // SAFETY: test holds ENV_MUTEX to serialize env var access
        unsafe {
            env::set_var("AMPLIHACK_LAUNCHER_BINARY", launcher.to_str().unwrap());
            env::set_var("AMPLIHACK_RATELIMIT_BASE_DELAY_SECS", "0");
            env::set_var("AMPLIHACK_RATELIMIT_MAX_RETRIES", "1");
            env::set_var("AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL", "1");
            env::set_var("AMPLIHACK_TEST_RL_COUNTER", counter_file.to_str().unwrap());
            env::set_var("AMPLIHACK_AGENT_BINARY", "claude");
        }

        let adapter = CLISubprocessAdapter::new();
        let result = adapter.execute_agent_step_impl(
            "hello",
            None,
            None,
            work_dir.path().to_str().unwrap(),
            Some(60),
        );

        assert!(
            result.is_ok(),
            "final-attempt --model auto fallback must allow success, got: {result:?}"
        );
        assert!(
            result.unwrap().contains("AGENT_OK_AUTO"),
            "success must come from the --model auto fallback attempt"
        );
        let count: u32 = std::fs::read_to_string(&counter_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(count, 2, "1 throttled attempt + 1 auto-model retry");
    }
}
