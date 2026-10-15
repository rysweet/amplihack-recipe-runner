use super::fixtures::run;
use serde_json::json;

#[test]
fn deliberately_empty_final_succeeds() {
    let (_, result, _) = run("empty", "task", None, "codex");
    assert_eq!(result, json!({"ok":""}));
}

fn assert_failure(mode: &str) {
    let prompt = if matches!(mode, "partial" | "blocked") {
        "x".repeat(1_000_000)
    } else {
        "task".into()
    };
    let (_, result, record) = run(mode, &prompt, None, "codex");
    assert!(result.get("error").is_some(), "{mode}: {result}");
    assert_ne!(result["error"], "", "{mode} needs contextual diagnostics");
    if let Some(path) = record["final"].as_str() {
        assert!(!std::path::Path::new(path).exists(), "{mode} leaked output");
    }
}
macro_rules! failure_case {
    ($name:ident, $mode:literal) => {
        #[test]
        fn $name() {
            assert_failure($mode);
        }
    };
}
failure_case!(missing_final_fails, "missing");
failure_case!(nonzero_with_final_fails, "nonzero");
failure_case!(directory_final_fails, "directory");
failure_case!(symlink_final_fails, "symlink");
failure_case!(fifo_final_fails_without_blocking, "fifo");
failure_case!(unsafe_permissions_fail, "unsafe_mode");
failure_case!(invalid_utf8_fails, "invalid");
failure_case!(oversized_final_fails, "oversize");
failure_case!(partial_stdin_with_zero_exit_fails, "partial");
failure_case!(blocked_stdin_respects_timeout, "blocked");

#[test]
fn exact_existing_output_limit_is_verbatim() {
    let (_, result, _) = run("limit", "task", None, "codex");
    assert!(result.get("ok").is_some(), "{result}");
    assert_eq!(result["ok"].as_str().unwrap().len(), 10_000_000);
}

#[test]
fn failure_diagnostics_are_actionable_and_redacted() {
    let (_, result, _) = run("auth_failure", "PRIVATE_PROMPT", None, "codex");
    let error = result["error"].as_str().unwrap();
    assert!(error.contains("authentication rejected"));
    assert!(error.contains("check Codex login"));
    assert!(!error.contains("SECRET_TOKEN"));
    assert!(!error.contains("PRIVATE_PROMPT"));
}
