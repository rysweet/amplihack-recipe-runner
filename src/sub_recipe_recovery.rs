//! Sub-recipe recovery — agentic error recovery for nested recipe failures.
//!
//! Matches Python `amplihack/recipes/tests/test_sub_recipe_recovery.py`:
//! - When a sub-recipe step fails, attempt agent-based recovery
//! - Classify failures as recoverable vs unrecoverable
//! - Preserve error context from both original and recovery attempts
//! - Limit recovery attempts to prevent infinite loops

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Maximum recovery attempts before giving up.
const MAX_RECOVERY_ATTEMPTS: u32 = 2;

/// Classification of a sub-recipe failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureClass {
    /// The failure can potentially be recovered by an agent.
    Recoverable,
    /// The failure is permanent (e.g., missing dependency, auth error).
    Unrecoverable,
    /// Unknown — attempt recovery once then give up.
    Unknown,
}

/// Context about a failed sub-recipe execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureContext {
    pub recipe_name: String,
    pub step_id: String,
    pub error_message: String,
    pub exit_code: Option<i32>,
    pub failure_class: FailureClass,
    pub attempt: u32,
}

/// Result of a recovery attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryResult {
    pub recovered: bool,
    pub output: String,
    pub attempt: u32,
    pub strategy: String,
}

/// Manages sub-recipe error recovery.
pub struct SubRecipeRecovery {
    max_attempts: u32,
}

impl SubRecipeRecovery {
    pub fn new() -> Self {
        Self {
            max_attempts: MAX_RECOVERY_ATTEMPTS,
        }
    }

    pub fn with_max_attempts(max_attempts: u32) -> Self {
        Self { max_attempts }
    }

    /// Classify a failure to determine if recovery should be attempted.
    pub fn classify_failure(&self, error: &str, exit_code: Option<i32>) -> FailureClass {
        let error_lower = error.to_lowercase();

        // Unrecoverable patterns
        let unrecoverable = [
            "permission denied",
            "authentication failed",
            "not found: 404",
            "out of memory",
            "disk full",
            "unrecoverable",
            "fatal:",
            "cannot find module",
        ];
        if unrecoverable.iter().any(|p| error_lower.contains(p)) {
            return FailureClass::Unrecoverable;
        }

        // Exit codes that indicate unrecoverable issues
        if matches!(exit_code, Some(126) | Some(127) | Some(137)) {
            return FailureClass::Unrecoverable;
        }

        // Recoverable patterns
        let recoverable = [
            "test failed",
            "compilation error",
            "syntax error",
            "lint error",
            "type error",
            "assertion failed",
            "command exited with code 1",
        ];
        if recoverable.iter().any(|p| error_lower.contains(p)) {
            return FailureClass::Recoverable;
        }

        FailureClass::Unknown
    }

    /// Check if recovery should be attempted.
    pub fn should_attempt_recovery(&self, ctx: &FailureContext) -> bool {
        if ctx.attempt >= self.max_attempts {
            warn!(
                recipe = ctx.recipe_name,
                attempt = ctx.attempt,
                "Max recovery attempts reached"
            );
            return false;
        }
        ctx.failure_class != FailureClass::Unrecoverable
    }

    /// Build a recovery prompt for the agent.
    pub fn build_recovery_prompt(&self, ctx: &FailureContext) -> String {
        format!(
            "Sub-recipe '{}' failed at step '{}' (attempt {}/{}).\n\n\
             Error: {}\n\n\
             Please analyze the failure and attempt to fix the issue. \
             If the issue is unrecoverable, respond with UNRECOVERABLE.",
            ctx.recipe_name,
            ctx.step_id,
            ctx.attempt + 1,
            self.max_attempts,
            ctx.error_message,
        )
    }

    /// Parse the agent's recovery response.
    pub fn parse_recovery_response(&self, response: &str, attempt: u32) -> RecoveryResult {
        let response_lower = response.to_lowercase();
        if response_lower.contains("unrecoverable") {
            return RecoveryResult {
                recovered: false,
                output: response.to_string(),
                attempt,
                strategy: "agent_declared_unrecoverable".into(),
            };
        }
        // If the agent provided a response without UNRECOVERABLE, consider it recovered
        RecoveryResult {
            recovered: !response.trim().is_empty(),
            output: response.to_string(),
            attempt,
            strategy: "agent_recovery".into(),
        }
    }
}

impl Default for SubRecipeRecovery {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_unrecoverable_patterns() {
        let r = SubRecipeRecovery::new();
        assert_eq!(
            r.classify_failure("Permission denied: /root/.ssh", None),
            FailureClass::Unrecoverable
        );
        assert_eq!(
            r.classify_failure("Fatal: could not read", None),
            FailureClass::Unrecoverable
        );
        assert_eq!(
            r.classify_failure("something", Some(127)),
            FailureClass::Unrecoverable
        );
    }

    #[test]
    fn classify_recoverable_patterns() {
        let r = SubRecipeRecovery::new();
        assert_eq!(
            r.classify_failure("Test failed: 3 assertions", None),
            FailureClass::Recoverable
        );
        assert_eq!(
            r.classify_failure("Compilation error in main.rs", None),
            FailureClass::Recoverable
        );
        assert_eq!(
            r.classify_failure("command exited with code 1", None),
            FailureClass::Recoverable
        );
    }

    #[test]
    fn classify_unknown() {
        let r = SubRecipeRecovery::new();
        assert_eq!(
            r.classify_failure("something unexpected happened", None),
            FailureClass::Unknown
        );
    }

    #[test]
    fn should_attempt_recovery() {
        let r = SubRecipeRecovery::new();
        let ctx = FailureContext {
            recipe_name: "test".into(),
            step_id: "s1".into(),
            error_message: "test failed".into(),
            exit_code: Some(1),
            failure_class: FailureClass::Recoverable,
            attempt: 0,
        };
        assert!(r.should_attempt_recovery(&ctx));
    }

    #[test]
    fn should_not_recover_unrecoverable() {
        let r = SubRecipeRecovery::new();
        let ctx = FailureContext {
            recipe_name: "test".into(),
            step_id: "s1".into(),
            error_message: "perm denied".into(),
            exit_code: None,
            failure_class: FailureClass::Unrecoverable,
            attempt: 0,
        };
        assert!(!r.should_attempt_recovery(&ctx));
    }

    #[test]
    fn should_not_recover_max_attempts() {
        let r = SubRecipeRecovery::new();
        let ctx = FailureContext {
            recipe_name: "test".into(),
            step_id: "s1".into(),
            error_message: "test failed".into(),
            exit_code: None,
            failure_class: FailureClass::Recoverable,
            attempt: 2,
        };
        assert!(!r.should_attempt_recovery(&ctx));
    }

    #[test]
    fn recovery_prompt_includes_context() {
        let r = SubRecipeRecovery::new();
        let ctx = FailureContext {
            recipe_name: "default-workflow".into(),
            step_id: "step-05".into(),
            error_message: "cargo test failed".into(),
            exit_code: Some(1),
            failure_class: FailureClass::Recoverable,
            attempt: 0,
        };
        let prompt = r.build_recovery_prompt(&ctx);
        assert!(prompt.contains("default-workflow"));
        assert!(prompt.contains("step-05"));
        assert!(prompt.contains("cargo test failed"));
        assert!(prompt.contains("1/2"));
    }

    #[test]
    fn parse_unrecoverable_response() {
        let r = SubRecipeRecovery::new();
        let result =
            r.parse_recovery_response("This is UNRECOVERABLE - missing external dependency", 0);
        assert!(!result.recovered);
        assert_eq!(result.strategy, "agent_declared_unrecoverable");
    }

    #[test]
    fn parse_successful_recovery() {
        let r = SubRecipeRecovery::new();
        let result =
            r.parse_recovery_response("Fixed the compilation error by adding missing import", 0);
        assert!(result.recovered);
        assert_eq!(result.strategy, "agent_recovery");
    }

    #[test]
    fn parse_empty_response_not_recovered() {
        let r = SubRecipeRecovery::new();
        let result = r.parse_recovery_response("", 0);
        assert!(!result.recovered);
    }

    #[test]
    fn custom_max_attempts() {
        let r = SubRecipeRecovery::with_max_attempts(5);
        let ctx = FailureContext {
            recipe_name: "test".into(),
            step_id: "s1".into(),
            error_message: "failed".into(),
            exit_code: None,
            failure_class: FailureClass::Recoverable,
            attempt: 4,
        };
        assert!(r.should_attempt_recovery(&ctx));
    }

    // ── Property-based tests (PR5: audit/proptest-state-machines) ───────

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        // SR-1: classify_failure never panics on arbitrary input
        proptest! {
            #[test]
            fn classify_failure_no_panic(
                error in "\\PC{0,500}",
                exit_code in proptest::option::of(0..256i32),
            ) {
                let r = SubRecipeRecovery::new();
                let _ = r.classify_failure(&error, exit_code);
            }
        }

        // SR-2: classify_failure always returns a valid FailureClass variant
        proptest! {
            #[test]
            fn classify_failure_returns_valid_class(
                error in "\\PC{0,200}",
                exit_code in proptest::option::of(0..256i32),
            ) {
                let r = SubRecipeRecovery::new();
                let class = r.classify_failure(&error, exit_code);
                // Exhaustive match ensures totality — compiler would catch missing variants
                match class {
                    FailureClass::Recoverable | FailureClass::Unrecoverable | FailureClass::Unknown => {}
                }
            }
        }

        // SR-3: classify_failure is case-insensitive for known patterns
        proptest! {
            #[test]
            fn classify_case_insensitive(
                pattern in prop_oneof![
                    Just("permission denied"),
                    Just("test failed"),
                    Just("fatal:"),
                    Just("syntax error"),
                ],
            ) {
                let r = SubRecipeRecovery::new();
                let lower = r.classify_failure(pattern, None);
                let upper = r.classify_failure(&pattern.to_uppercase(), None);
                prop_assert_eq!(
                    lower, upper,
                    "classify_failure should be case-insensitive: '{}' vs '{}'",
                    pattern, pattern.to_uppercase(),
                );
            }
        }

        // SR-4: Unrecoverable failures are never retried via should_attempt_recovery()
        proptest! {
            #[test]
            fn unrecoverable_never_retried(attempt in 0..10u32) {
                let r = SubRecipeRecovery::new();
                let ctx = FailureContext {
                    recipe_name: "test".into(),
                    step_id: "s1".into(),
                    error_message: "error".into(),
                    exit_code: None,
                    failure_class: FailureClass::Unrecoverable,
                    attempt,
                };
                prop_assert!(
                    !r.should_attempt_recovery(&ctx),
                    "Unrecoverable failures should never be retried (attempt {})",
                    attempt,
                );
            }
        }

        // SR-5: should_attempt_recovery() respects max_attempts for non-Unrecoverable
        proptest! {
            #[test]
            fn max_attempts_respected(
                max in 1..10u32,
                attempt in 0..20u32,
                class in prop_oneof![
                    Just(FailureClass::Recoverable),
                    Just(FailureClass::Unknown),
                ],
            ) {
                let r = SubRecipeRecovery::with_max_attempts(max);
                let ctx = FailureContext {
                    recipe_name: "test".into(),
                    step_id: "s1".into(),
                    error_message: "error".into(),
                    exit_code: None,
                    failure_class: class,
                    attempt,
                };
                let should = r.should_attempt_recovery(&ctx);
                if attempt >= max {
                    prop_assert!(
                        !should,
                        "attempt {} >= max {} should not be retried",
                        attempt, max,
                    );
                } else {
                    prop_assert!(
                        should,
                        "attempt {} < max {} should be retried for {:?}",
                        attempt, max, class,
                    );
                }
            }
        }

        // SR-6: parse_recovery_response never panics on arbitrary input
        proptest! {
            #[test]
            fn parse_response_no_panic(
                response in "\\PC{0,500}",
                attempt in 0..100u32,
            ) {
                let r = SubRecipeRecovery::new();
                let _ = r.parse_recovery_response(&response, attempt);
            }
        }

        // SR-7: parse_recovery_response marks "UNRECOVERABLE" as not recovered
        proptest! {
            #[test]
            fn unrecoverable_keyword_detected(
                prefix in "[a-zA-Z ]{0,50}",
                suffix in "[a-zA-Z ]{0,50}",
            ) {
                let r = SubRecipeRecovery::new();
                let response = format!("{}UNRECOVERABLE{}", prefix, suffix);
                let result = r.parse_recovery_response(&response, 0);
                prop_assert!(
                    !result.recovered,
                    "Response containing 'UNRECOVERABLE' should not be marked recovered: {}",
                    response,
                );
                prop_assert_eq!(
                    result.strategy,
                    "agent_declared_unrecoverable",
                    "Strategy should be 'agent_declared_unrecoverable'",
                );
            }
        }

        // SR-8: build_recovery_prompt includes all context fields
        proptest! {
            #[test]
            fn recovery_prompt_contains_context(
                recipe_name in "[a-zA-Z][a-zA-Z0-9_-]{0,20}",
                step_id in "[a-zA-Z][a-zA-Z0-9_-]{0,20}",
                error_msg in "[a-zA-Z0-9 .,!?]{1,50}",
                attempt in 0..5u32,
            ) {
                let r = SubRecipeRecovery::new();
                let ctx = FailureContext {
                    recipe_name: recipe_name.clone(),
                    step_id: step_id.clone(),
                    error_message: error_msg.clone(),
                    exit_code: None,
                    failure_class: FailureClass::Recoverable,
                    attempt,
                };
                let prompt = r.build_recovery_prompt(&ctx);
                prop_assert!(prompt.contains(&recipe_name), "Prompt missing recipe_name");
                prop_assert!(prompt.contains(&step_id), "Prompt missing step_id");
                prop_assert!(prompt.contains(&error_msg), "Prompt missing error_message");
            }
        }

        // SR-9: Exit codes 126, 127, 137 are always classified as Unrecoverable
        proptest! {
            #[test]
            fn fatal_exit_codes_unrecoverable(
                code in prop_oneof![Just(126), Just(127), Just(137)],
                error in "[a-zA-Z0-9 ]{0,50}",
            ) {
                let r = SubRecipeRecovery::new();
                let class = r.classify_failure(&error, Some(code));
                prop_assert_eq!(
                    class,
                    FailureClass::Unrecoverable,
                    "Exit code {} should always be Unrecoverable regardless of error text '{}'",
                    code, error,
                );
            }
        }
    }
}
