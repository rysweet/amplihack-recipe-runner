use super::fixtures::*;
use crate::models::StepStatus;

fn primary(policy: &str, bash: bool) {
    let root = tempfile::tempdir().unwrap();
    let adapter = if bash {
        Scripted::new([]).bash("cleanup", Reply::Cleanup)
    } else {
        Scripted::new([Reply::Cleanup])
    };
    let state = adapter.state.clone();
    let dispatch = if bash {
        "    type: bash\n    command: cleanup\n"
    } else {
        "    prompt: cleanup\n"
    };
    let recipe = recipe(
        &format!("  - id: first\n{dispatch}    {policy}\n{AFTER}"),
        "",
    );
    let result = runner(adapter, root.path()).execute(&recipe, None);
    assert_terminal(&result, &state, usize::from(!bash));
    assert_eq!(result.step_results.len(), 1);
}

macro_rules! primary_case {
    ($name:ident, $policy:literal, $bash:literal) => {
        #[test]
        fn $name() {
            primary($policy, $bash);
        }
    };
}
primary_case!(fatal_agent_cleanup_stops_dispatch, "fatal: true", false);
primary_case!(
    continue_agent_cleanup_stops_dispatch,
    "continue_on_error: true",
    false
);
primary_case!(nonfatal_agent_cleanup_stops_dispatch, "fatal: false", false);
primary_case!(fatal_bash_cleanup_stops_dispatch, "fatal: true", true);
primary_case!(
    continue_bash_cleanup_stops_dispatch,
    "continue_on_error: true",
    true
);
primary_case!(nonfatal_bash_cleanup_stops_dispatch, "fatal: false", true);

fn hook_boundary(hook: &str, last: bool, group: bool) {
    let root = tempfile::tempdir().unwrap();
    let first_reply = if hook == "on_error" {
        Reply::Ordinary
    } else {
        Reply::Success
    };
    let adapter = Scripted::new([first_reply]).bash("cleanup-hook", Reply::Cleanup);
    let state = adapter.state.clone();
    let grouping = if group { "    parallel_group: g\n" } else { "" };
    let later = if last {
        "".into()
    } else if group {
        format!("  - id: pending\n    prompt: admitted-group\n    parallel_group: g\n{AFTER}")
    } else {
        AFTER.into()
    };
    let hooks = format!("hooks:\n  {hook}: cleanup-hook\n");
    let recipe = recipe(
        &format!("  - id: first\n    prompt: primary\n    fatal: false\n{grouping}{later}"),
        &hooks,
    );
    let result = runner(adapter, root.path()).execute(&recipe, None);
    // Group post/error hooks run after group primaries; already-dispatched work is allowed.
    let agents = if hook == "pre_step" {
        0
    } else if group {
        2
    } else {
        1
    };
    assert_terminal(&result, &state, agents);
    assert_eq!(
        state
            .events()
            .iter()
            .filter(|e| *e == "bash:cleanup-hook")
            .count(),
        1,
        "terminal hook must suppress later hooks"
    );
    let first = result
        .step_results
        .iter()
        .find(|r| r.step_id == "first")
        .unwrap();
    assert_eq!(
        first.status,
        StepStatus::Failed,
        "hook must change effective status"
    );
    assert!(first.error.contains("owned resource teardown failed"));
    if hook == "on_error" {
        assert!(first.error.contains("ordinary cleanup/cancelled failure"));
    }
    let recorded = state.completed.lock().unwrap();
    assert_eq!(recorded.first().unwrap().status, StepStatus::Failed);
}

#[test]
fn pre_hook_cleanup_blocks_primary_and_later_hooks() {
    hook_boundary("pre_step", false, false);
}
#[test]
fn post_hook_cleanup_blocks_later_dispatch() {
    hook_boundary("post_step", false, false);
}
#[test]
fn last_post_hook_cleanup_changes_recipe_outcome() {
    hook_boundary("post_step", true, false);
}
#[test]
fn error_hook_cleanup_retains_primary_and_stops_dispatch() {
    hook_boundary("on_error", false, false);
}
#[test]
fn group_pre_hook_cleanup_blocks_group_dispatch() {
    hook_boundary("pre_step", false, true);
}
#[test]
fn group_post_hook_cleanup_blocks_later_hooks_and_groups() {
    hook_boundary("post_step", false, true);
}
#[test]
fn group_error_hook_cleanup_blocks_later_hooks_and_groups() {
    hook_boundary("on_error", false, true);
}

#[test]
fn final_post_hook_cleanup_has_failed_audit_and_no_success_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let suffix = root
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .replace('.', "_");
    let name = format!("cleanup-post-{suffix}");
    let mut recipe = recipe(
        "  - id: first\n    prompt: primary\n",
        "hooks:\n  post_step: cleanup-hook\n",
    );
    recipe.name = name.clone();
    let adapter = Scripted::new([]).bash("cleanup-hook", Reply::Cleanup);
    let state = adapter.state.clone();
    let result = runner(adapter, root.path())
        .with_audit_dir(root.path().join("audit"))
        .with_checkpoints(true)
        .execute(&recipe, None);
    let checkpoint = std::env::temp_dir().join(format!(
        "amplihack-checkpoint-{name}-{}.json",
        std::process::id()
    ));
    // Remove even the erroneous baseline checkpoint before assertions.
    let saved = std::fs::read(&checkpoint).ok();
    if saved.is_some() {
        std::fs::remove_file(checkpoint).unwrap();
    }
    let audit_file = std::fs::read_dir(root.path().join("audit"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let audit: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(audit_file).unwrap().trim()).unwrap();
    assert!(!result.success, "terminal final hook must fail recipe");
    assert!(
        saved.is_none(),
        "terminal hook wrote a successful checkpoint"
    );
    assert_eq!(audit["status"], format!("{}", StepStatus::Failed));
    assert_eq!(
        state.completed.lock().unwrap()[0].status,
        StepStatus::Failed
    );
}

#[test]
fn ordinary_errors_keep_both_nonfatal_policies_and_hook_warnings() {
    for policy in ["continue_on_error: true", "fatal: false"] {
        let root = tempfile::tempdir().unwrap();
        let adapter = Scripted::new([Reply::Ordinary]).bash("ordinary-hook", Reply::Ordinary);
        let state = adapter.state.clone();
        let result = runner(adapter, root.path()).execute(&recipe(
            &format!("  - id: first\n    prompt: ordinary\n    {policy}\n{AFTER}"),
            "hooks:\n  pre_step: ordinary-hook\n  on_error: ordinary-hook\n  post_step: ordinary-hook\n"), None);
        assert!(result.success, "ordinary policy changed: {result:?}");
        assert_eq!(state.agents(), 2);
        assert!(state.events().contains(&"bash:forbidden-bash".into()));
    }
}

#[test]
fn independent_execute_resets_cleanup_terminality() {
    let root = tempfile::tempdir().unwrap();
    let adapter = Scripted::new([Reply::Cleanup, Reply::Success]);
    let state = adapter.state.clone();
    let runner = runner(adapter, root.path());
    let recipe = recipe("  - id: first\n    prompt: primary\n", "");
    assert_terminal(&runner.execute(&recipe, None), &state, 1);
    let second = runner.execute(&recipe, None);
    assert!(
        second.success,
        "root execution must reset terminal state: {second:?}"
    );
    assert_eq!(state.agents(), 2);
}

#[test]
fn ordinary_bash_errors_keep_both_nonfatal_policies() {
    for policy in ["continue_on_error: true", "fatal: false"] {
        let root = tempfile::tempdir().unwrap();
        let adapter = Scripted::new([]).bash("ordinary", Reply::Ordinary);
        let state = adapter.state.clone();
        let result = runner(adapter, root.path()).execute(
            &recipe(
                &format!(
                    "  - id: first\n    type: bash\n    command: ordinary\n    {policy}\n{AFTER}"
                ),
                "",
            ),
            None,
        );
        assert!(result.success, "ordinary Bash policy changed: {result:?}");
        assert_eq!(state.agents(), 1);
        assert!(state.events().contains(&"bash:forbidden-bash".into()));
    }
}

#[test]
fn terminal_post_hook_on_skipped_primary_reports_effective_failure() {
    let root = tempfile::tempdir().unwrap();
    let adapter = Scripted::new([]).bash("cleanup-hook", Reply::Cleanup);
    let state = adapter.state.clone();
    let result = runner(adapter, root.path()).execute(
        &recipe(
            "  - id: skipped\n    prompt: forbidden-primary\n    condition: 'False'\n",
            "hooks:\n  post_step: cleanup-hook\n",
        ),
        None,
    );
    assert_terminal(&result, &state, 0);
    assert_eq!(result.step_results[0].step_id, "skipped");
    assert_eq!(result.step_results[0].status, StepStatus::Failed);
    assert_eq!(
        state.completed.lock().unwrap()[0].status,
        StepStatus::Failed
    );
}
