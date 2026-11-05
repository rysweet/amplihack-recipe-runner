//! Execution progress listeners.
//!
//! Listeners receive callbacks during recipe execution for progress reporting,
//! logging, or custom integrations.

use crate::models::{StepResult, StepStatus, StepType, diagnostic_identifier};
use serde_json::{Value, json};
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
        eprintln!("▶ {}{}", diagnostic_identifier(step_id), type_hint);
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
        eprintln!(
            "  {} {}{}",
            icon,
            diagnostic_identifier(&result.step_id),
            dur
        );
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
                if let Err(e) = writeln!(
                    f,
                    "{}",
                    json!({"type": "recipe_start", "recipe": recipe_name, "ts": ts, "pid": std::process::id()})
                ) {
                    log::warn!("Failed to write recipe log header: {}", e);
                }
                if let Err(e) = f.flush() {
                    log::warn!("Failed to flush recipe log header: {}", e);
                }
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

    fn write_event(&self, event: &Value) {
        if let Ok(mut f) = self.file.lock() {
            if let Err(e) = writeln!(f, "{}", event) {
                log::warn!("Failed to write recipe log event: {}", e);
            }
            if let Err(e) = f.flush() {
                log::warn!("Failed to flush recipe log: {}", e);
            }
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
        StderrListener.on_step_start(step_id, step_type);
        self.write_event(&json!({
            "type": "step_transition",
            "step": step_id,
            "step_type": format!("{step_type:?}"),
            "status": "start",
            "ts": Self::timestamp(),
        }));
    }

    fn on_step_complete(&self, result: &StepResult) {
        StderrListener.on_step_complete(result);
        self.write_event(&json!({
            "type": "step_transition",
            "step": result.step_id,
            "status": match result.status {
                StepStatus::Completed => "done",
                StepStatus::Skipped => "skip",
                StepStatus::Failed => "fail",
                StepStatus::Degraded => "degraded",
                _ => "unknown",
            },
            "duration_secs": result.duration.map(|d| d.as_secs_f64()).unwrap_or(0.0),
            "ts": Self::timestamp(),
        }));
    }

    fn on_output(&self, step_id: &str, line: &str) {
        self.write_event(&json!({
            "type": "output",
            "step": step_id,
            "line": line,
            "ts": Self::timestamp(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn structured_events_round_trip_control_characters_quotes_and_backslashes() {
        let recipe = "listener-test\n\r\u{1b}[31m\"\\recipe";
        let step = "step\n\r\u{1b}[31m\"\\id";
        let line = "output\n\r\u{1b}[31m\t\0\"\\tail";
        let (listener, path) = FileLogListener::new(recipe).unwrap();
        listener.on_step_start(step, StepType::Recipe);
        listener.on_output(step, line);
        for status in [
            StepStatus::Completed,
            StepStatus::Skipped,
            StepStatus::Failed,
            StepStatus::Degraded,
            StepStatus::Running,
        ] {
            listener.on_step_complete(&StepResult {
                step_id: step.into(),
                status,
                output: String::new(),
                error: String::new(),
                duration: Some(Duration::from_millis(125)),
            });
        }
        drop(listener);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(!text.contains('\r'));
        assert!(!text.contains('\u{1b}'));
        let events: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 8, "one JSON object per event");
        assert_eq!(events[0]["type"], "recipe_start");
        assert_eq!(events[0]["recipe"], recipe);
        assert_eq!(events[0]["pid"], std::process::id());
        assert_eq!(events[1]["type"], "step_transition");
        assert_eq!(events[1]["status"], "start");
        assert_eq!(events[1]["step_type"], "Recipe");
        assert_eq!(events[2]["type"], "output");
        assert_eq!(events[2]["line"], line);
        for event in &events[1..] {
            assert_eq!(event["step"], step);
        }
        for (event, status) in events[3..]
            .iter()
            .zip(["done", "skip", "fail", "degraded", "unknown"])
        {
            assert_eq!(event["type"], "step_transition");
            assert_eq!(event["status"], status);
            assert_eq!(event["duration_secs"], 0.125);
        }
        for event in &events {
            assert!(event["ts"].as_f64().unwrap() > 0.0);
        }
    }
}
