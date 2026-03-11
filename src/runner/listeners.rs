//! Execution progress listeners.
//!
//! Listeners receive callbacks during recipe execution for progress reporting,
//! logging, or custom integrations.

use crate::models::{StepResult, StepStatus, StepType};

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
pub struct StderrListener;
impl ExecutionListener for StderrListener {
    fn on_step_start(&self, step_id: &str, step_type: StepType) {
        log::debug!("StderrListener::on_step_start: step_id={:?}, type={:?}", step_id, step_type);
        eprintln!("▶ {} ({:?})", step_id, step_type);
    }
    fn on_step_complete(&self, result: &StepResult) {
        log::debug!("StderrListener::on_step_complete: step_id={:?}, status={:?}", result.step_id, result.status);
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
