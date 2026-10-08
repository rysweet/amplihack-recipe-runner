use super::fixtures::*;
use crate::models::StepStatus;

fn repair_failure(provider: &'static str, required: bool, policy: &str, nested: bool) {
    let root = tempfile::tempdir().unwrap();
    let adapter = Scripted::new([Reply::InvalidJson, Reply::Cleanup]).provider(provider);
    let state = adapter.state.clone();
    let steps = format!(
        "  - id: json\n    prompt: primary\n    parse_json: true\n    parse_json_required: {required}\n    output: parsed\n    {policy}\n{AFTER}"
    );
    let top_steps = if nested {
        write_child(root.path(), "child", &steps);
        format!(
            "  - id: child\n    type: recipe\n    recipe: child\n    recovery_on_failure: true\n    fatal: false\n{AFTER}"
        )
    } else {
        steps
    };
    let result = runner(adapter, root.path()).execute(&recipe(&top_steps, ""), None);
    assert_terminal(&result, &state, 2);
    assert!(
        !result.context.contains_key("parsed"),
        "terminal repair stored fallback output"
    );
    assert!(
        result
            .step_results
            .iter()
            .all(|r| r.status != StepStatus::Degraded)
    );
}

macro_rules! repair_case {
    ($name:ident, $provider:literal, $required:literal, $policy:literal) => {
        #[test]
        fn $name() {
            repair_failure($provider, $required, $policy, false);
        }
    };
}
repair_case!(
    codex_optional_continue_repair_cleanup,
    "codex",
    false,
    "continue_on_error: true"
);
repair_case!(
    codex_optional_nonfatal_repair_cleanup,
    "codex",
    false,
    "fatal: false"
);
repair_case!(
    codex_required_continue_repair_cleanup,
    "codex",
    true,
    "continue_on_error: true"
);
repair_case!(
    codex_required_nonfatal_repair_cleanup,
    "codex",
    true,
    "fatal: false"
);
repair_case!(
    claude_optional_continue_repair_cleanup,
    "claude",
    false,
    "continue_on_error: true"
);
repair_case!(
    claude_optional_nonfatal_repair_cleanup,
    "claude",
    false,
    "fatal: false"
);
repair_case!(
    claude_required_continue_repair_cleanup,
    "claude",
    true,
    "continue_on_error: true"
);
repair_case!(
    claude_required_nonfatal_repair_cleanup,
    "claude",
    true,
    "fatal: false"
);
repair_case!(
    copilot_optional_continue_repair_cleanup,
    "copilot",
    false,
    "continue_on_error: true"
);
repair_case!(
    copilot_optional_nonfatal_repair_cleanup,
    "copilot",
    false,
    "fatal: false"
);
repair_case!(
    copilot_required_continue_repair_cleanup,
    "copilot",
    true,
    "continue_on_error: true"
);
repair_case!(
    copilot_required_nonfatal_repair_cleanup,
    "copilot",
    true,
    "fatal: false"
);

#[test]
fn codex_nested_repair_cleanup_blocks_recovery() {
    repair_failure("codex", false, "continue_on_error: true", true);
}
#[test]
fn claude_nested_repair_cleanup_blocks_recovery() {
    repair_failure("claude", false, "fatal: false", true);
}
#[test]
fn copilot_nested_repair_cleanup_blocks_recovery() {
    repair_failure("copilot", true, "fatal: false", true);
}

#[test]
fn primary_cleanup_does_not_dispatch_json_repair() {
    let root = tempfile::tempdir().unwrap();
    let adapter = Scripted::new([Reply::Cleanup]);
    let state = adapter.state.clone();
    let result = runner(adapter, root.path()).execute(
        &recipe(
            &format!(
                "  - id: json\n    prompt: cleanup\n    parse_json: true\n    fatal: false\n{AFTER}"
            ),
            "",
        ),
        None,
    );
    assert_terminal(&result, &state, 1);
}

#[test]
fn ordinary_optional_json_repair_errors_still_degrade() {
    for provider in ["codex", "claude", "copilot"] {
        let root = tempfile::tempdir().unwrap();
        let adapter = Scripted::new([Reply::InvalidJson, Reply::Ordinary]).provider(provider);
        let state = adapter.state.clone();
        let result = runner(adapter, root.path()).execute(&recipe(&format!(
            "  - id: json\n    prompt: primary\n    parse_json: true\n    output: parsed\n{AFTER}"), ""), None);
        assert!(
            result.success,
            "ordinary JSON policy changed for {provider}: {result:?}"
        );
        assert_eq!(result.step_results[0].status, StepStatus::Degraded);
        assert_eq!(result.context["parsed"], "invalid JSON");
        assert_eq!(state.agents(), 3);
        assert!(state.events().contains(&"bash:forbidden-bash".into()));
    }
}
