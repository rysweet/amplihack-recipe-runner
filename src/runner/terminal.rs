//! Private, monotone terminal state shared by an execution and its workers.
//! Classify before text conversion; only independent top-level calls reset it.
use crate::{
    adapters::{CleanupFailure, Interruption},
    models::{StepResult, StepStatus},
};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Cleanup,
    Interruption,
}

#[derive(Clone, Debug)]
pub(super) struct TerminalFailure {
    kind: Kind,
    diagnostic: String,
}

impl TerminalFailure {
    pub(super) fn classify(error: &anyhow::Error) -> Option<Self> {
        let kind = if error.downcast_ref::<Interruption>().is_some() {
            Kind::Interruption
        } else if error.downcast_ref::<CleanupFailure>().is_some() {
            Kind::Cleanup
        } else {
            return None;
        };
        Some(Self {
            kind,
            diagnostic: format!("{error:#}"),
        })
    }
}

#[derive(Default)]
pub(super) struct TerminalState(Mutex<Vec<TerminalFailure>>);

impl TerminalState {
    pub(super) fn reset(&self) {
        self.0.lock().unwrap().clear();
    }

    pub(super) fn merge(&self, failure: Option<TerminalFailure>) {
        if let Some(failure) = failure {
            let mut state = self.0.lock().unwrap();
            if !state.iter().any(|f| f.diagnostic == failure.diagnostic) {
                state.push(failure);
                // Interruption is primary; preserve distinct cleanup diagnostics.
                state.sort_by_key(|failure| std::cmp::Reverse(failure.kind));
            }
        }
    }

    pub(super) fn observe(&self, error: &anyhow::Error) {
        self.merge(TerminalFailure::classify(error));
    }

    pub(super) fn is_terminal(&self) -> bool {
        !self.0.lock().unwrap().is_empty()
    }

    pub(super) fn diagnostic(&self) -> Option<String> {
        let state = self.0.lock().unwrap();
        if state.is_empty() {
            None
        } else {
            Some(
                state
                    .iter()
                    .map(|f| f.diagnostic.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        }
    }

    /// Hooks and already admitted work cannot report successful completion
    /// after terminal observation. Keep any primary failure alongside it.
    pub(super) fn apply(&self, result: &mut StepResult) {
        if let Some(diagnostic) = self.diagnostic() {
            result.status = StepStatus::Failed;
            result.output.clear();
            if result.error.is_empty() {
                result.error = diagnostic;
            } else if !result.error.contains(&diagnostic) {
                result.error.push('\n');
                result.error.push_str(&diagnostic);
            }
        }
    }
}

pub(super) struct ParallelOutcome {
    pub result: StepResult,
    pub terminal: Option<TerminalFailure>,
}

impl From<StepResult> for ParallelOutcome {
    fn from(result: StepResult) -> Self {
        Self {
            result,
            terminal: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_text_does_not_establish_terminality() {
        let state = TerminalState::default();
        state.observe(&anyhow::anyhow!("cleanup failed; cancelled by signal"));
        assert!(!state.is_terminal());
        assert!(state.diagnostic().is_none());
    }

    #[test]
    fn interruption_precedes_cleanup_and_merge_retains_distinct_causes() {
        let state = TerminalState::default();
        let cleanup = TerminalFailure {
            kind: Kind::Cleanup,
            diagnostic: "first cleanup".into(),
        };
        state.merge(Some(cleanup.clone()));
        state.merge(Some(TerminalFailure {
            kind: Kind::Interruption,
            diagnostic: "interrupted with second cleanup".into(),
        }));
        state.merge(Some(cleanup));
        assert_eq!(
            state.diagnostic().as_deref(),
            Some("interrupted with second cleanup\nfirst cleanup")
        );
        state.reset();
        assert!(!state.is_terminal());
    }
}
