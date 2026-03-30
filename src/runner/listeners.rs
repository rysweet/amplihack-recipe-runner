//! Execution progress listeners.
//!
//! Listeners receive callbacks during recipe execution for progress reporting,
//! logging, or custom integrations.

use crate::models::{StepResult, StepStatus, StepType};
use std::io::Write;
use std::sync::Mutex;

/// Callback trait for step execution progress events.
///
/// Implement this trait to receive notifications when steps start and complete.
/// The default implementations are no-ops, so you only need to override the
/// methods you care about.
pub trait ExecutionListener {
    /// Called when a step begins execution.
    fn on_step_start(&self, step_id: &str, step_type: StepType) {
        let _ = (step_id, step_type);
    }
    /// Called when a step finishes (regardless of success/failure).
    fn on_step_complete(&self, result: &StepResult) {
        let _ = result;
    }
    /// Called when a step produces output (line by line).
    fn on_output(&self, step_id: &str, line: &str) {
        let _ = (step_id, line);
    }
}

/// No-op listener (default).
pub struct NullListener;
impl ExecutionListener for NullListener {}

/// Stderr progress listener (for `--progress` flag).
///
/// Prints step start/complete events to stderr with status icons and timing.
/// Agent steps can run for minutes, so start messages include the step type
/// to help distinguish quick bash steps from long-running agent steps.
pub struct StderrListener;
impl ExecutionListener for StderrListener {
    fn on_step_start(&self, step_id: &str, step_type: StepType) {
        log::debug!(
            "StderrListener::on_step_start: step_id={:?}, type={:?}",
            step_id,
            step_type
        );
        let type_hint = match step_type {
            StepType::Agent => " [agent — may take several minutes]",
            StepType::Bash => "",
            StepType::Recipe => " [sub-recipe]",
        };
        eprintln!("▶ {}{}", step_id, type_hint);
    }
    fn on_step_complete(&self, result: &StepResult) {
        log::debug!(
            "StderrListener::on_step_complete: step_id={:?}, status={:?}",
            result.step_id,
            result.status
        );
        let icon = match result.status {
            StepStatus::Completed => "✓",
            StepStatus::Skipped => "⊘",
            StepStatus::Failed => "✗",
            StepStatus::Degraded => "⚠",
            _ => "?",
        };
        let dur = result
            .duration
            .map(|d| format!(" ({:.1}s)", d.as_secs_f64()))
            .unwrap_or_default();
        eprintln!("  {} {}{}", icon, result.step_id, dur);
    }
}

/// File-based structured log listener.
///
/// Writes JSON events to a persistent log file that callers can `tail -f`.
/// Each line is a self-contained JSON object with type, step, status, and timestamp.
pub struct FileLogListener {
    file: Mutex<std::fs::File>,
    path: std::path::PathBuf,
}

impl FileLogListener {
    /// Create a new log file at the standard path.
    /// Returns (listener, path) or None if file creation fails.
    pub fn new(recipe_name: &str) -> Option<(Self, std::path::PathBuf)> {
        let safe_name: String = recipe_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(64)
            .collect();
        let path = std::env::temp_dir().join(format!(
            "amplihack-recipe-{}-{}.log",
            safe_name,
            std::process::id()
        ));
        match std::fs::File::create(&path) {
            Ok(mut f) => {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                let _ = writeln!(
                    f,
                    r#"{{"type":"recipe_start","recipe":"{}","ts":{:.3},"pid":{}}}"#,
                    recipe_name,
                    ts,
                    std::process::id()
                );
                let _ = f.flush();
                eprintln!("[amplihack] recipe log: {}", path.display());
                Some((
                    Self {
                        file: Mutex::new(f),
                        path: path.clone(),
                    },
                    path,
                ))
            }
            Err(e) => {
                log::warn!("Could not create recipe log file {}: {}", path.display(), e);
                None
            }
        }
    }

    fn write_event(&self, event: &str) {
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{}", event);
            let _ = f.flush();
        }
    }

    fn timestamp() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }

    /// Return the log file path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl ExecutionListener for FileLogListener {
    fn on_step_start(&self, step_id: &str, step_type: StepType) {
        let type_hint = match step_type {
            StepType::Agent => " [agent — may take several minutes]",
            StepType::Bash => "",
            StepType::Recipe => " [sub-recipe]",
        };
        // Also print to stderr for live visibility
        eprintln!("▶ {}{}", step_id, type_hint);
        self.write_event(&format!(
            r#"{{"type":"step_transition","step":"{}","step_type":"{:?}","status":"start","ts":{:.3}}}"#,
            step_id, step_type, Self::timestamp()
        ));
    }

    fn on_step_complete(&self, result: &StepResult) {
        let icon = match result.status {
            StepStatus::Completed => "✓",
            StepStatus::Skipped => "⊘",
            StepStatus::Failed => "✗",
            StepStatus::Degraded => "⚠",
            _ => "?",
        };
        let dur = result
            .duration
            .map(|d| format!(" ({:.1}s)", d.as_secs_f64()))
            .unwrap_or_default();
        eprintln!("  {} {}{}", icon, result.step_id, dur);
        self.write_event(&format!(
            r#"{{"type":"step_transition","step":"{}","status":"{}","duration_secs":{},"ts":{:.3}}}"#,
            result.step_id,
            match result.status {
                StepStatus::Completed => "done",
                StepStatus::Skipped => "skip",
                StepStatus::Failed => "fail",
                StepStatus::Degraded => "degraded",
                _ => "unknown",
            },
            result.duration.map(|d| d.as_secs_f64()).unwrap_or(0.0),
            Self::timestamp()
        ));
    }

    fn on_output(&self, step_id: &str, line: &str) {
        self.write_event(&format!(
            r#"{{"type":"output","step":"{}","line":"{}","ts":{:.3}}}"#,
            step_id,
            line.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n"),
            Self::timestamp()
        ));
    }
}
