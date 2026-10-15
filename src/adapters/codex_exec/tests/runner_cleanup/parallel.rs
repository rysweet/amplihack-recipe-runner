use super::fixtures::*;
use crate::adapters::Adapter;
use std::{
    collections::HashMap,
    sync::{Condvar, Mutex},
    time::Duration,
};

mod workers;

struct PairedCleanup {
    scripted: Scripted,
    entered: Mutex<usize>,
    wake: Condvar,
}
impl Adapter for PairedCleanup {
    fn execute_agent_step(
        &self,
        prompt: &str,
        agent: Option<&str>,
        system: Option<&str>,
        mode: Option<&str>,
        wd: &str,
        model: Option<&str>,
        timeout: Option<u64>,
    ) -> anyhow::Result<String> {
        self.scripted
            .execute_agent_step(prompt, agent, system, mode, wd, model, timeout)
    }
    fn execute_bash_step(
        &self,
        command: &str,
        wd: &str,
        timeout: Option<u64>,
        env: &HashMap<String, String>,
    ) -> anyhow::Result<String> {
        let result = self.scripted.execute_bash_step(command, wd, timeout, env);
        if command.starts_with("cleanup-") {
            let mut entered = self.entered.lock().unwrap();
            *entered += 1;
            self.wake.notify_all();
            let (entered, timeout) = self
                .wake
                .wait_timeout_while(entered, Duration::from_secs(5), |n| *n < 2)
                .unwrap();
            assert_eq!(
                *entered, 2,
                "both workers must be admitted before either returns cleanup"
            );
            assert!(!timeout.timed_out(), "paired worker watchdog");
        }
        result
    }
    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "codex"
    }
}

fn synchronous_cleanup(nested: bool) {
    let root = tempfile::tempdir().unwrap();
    let adapter = Scripted::new([Reply::Cleanup]);
    let state = adapter.state.clone();
    let dispatch = if nested {
        write_child(
            root.path(),
            "child",
            "  - id: cleanup\n    prompt: cleanup\n    fatal: false\n",
        );
        "    type: recipe\n    recipe: child\n    recovery_on_failure: true\n"
    } else {
        "    prompt: cleanup\n"
    };
    let recipe = recipe(
        &format!(
            "  - id: skipped\n    prompt: forbidden-skipped\n    condition: 'False'\n    parallel_group: g\n  - id: cleanup\n{dispatch}    fatal: false\n    parallel_group: g\n  - id: pending-agent\n    prompt: forbidden-group\n    parallel_group: g\n  - id: pending-bash\n    type: bash\n    command: forbidden-group-bash\n    parallel_group: g\n{AFTER}"
        ),
        "",
    );
    let result = runner(adapter, root.path()).execute(&recipe, None);
    assert_terminal(&result, &state, 1);
    let failed = result
        .step_results
        .iter()
        .find(|r| r.status == crate::models::StepStatus::Failed)
        .unwrap();
    assert_eq!(
        failed.step_id, "cleanup",
        "partial scheduling lost original index"
    );
    let observed = state.completed.lock().unwrap();
    assert_eq!(
        observed
            .iter()
            .find(|r| r.status == crate::models::StepStatus::Failed)
            .unwrap()
            .step_id,
        "cleanup"
    );
}

#[test]
fn synchronous_parallel_agent_cleanup_stops_pending_members() {
    synchronous_cleanup(false);
}
#[test]
fn synchronous_parallel_nested_cleanup_stops_pending_members() {
    synchronous_cleanup(true);
}

#[test]
fn threaded_cleanup_nonfatal_cannot_dispatch_next_group() {
    for policy in ["continue_on_error: true", "fatal: false"] {
        let root = tempfile::tempdir().unwrap();
        let adapter = Scripted::new([]).bash("cleanup", Reply::Cleanup);
        let state = adapter.state.clone();
        let result = runner(adapter, root.path()).execute(&recipe(&format!(
            "  - id: cleanup\n    type: bash\n    command: cleanup\n    {policy}\n    parallel_group: g\n  - id: next-group\n    prompt: forbidden-next-group\n    parallel_group: h\n{AFTER}"), ""), None);
        assert_terminal(&result, &state, 0);
    }
}

#[test]
fn threaded_ordinary_failure_preserves_nonfatal_policy() {
    let root = tempfile::tempdir().unwrap();
    let adapter = Scripted::new([]).bash("ordinary", Reply::Ordinary);
    let state = adapter.state.clone();
    let result = runner(adapter, root.path()).execute(&recipe(&format!(
        "  - id: ordinary\n    type: bash\n    command: ordinary\n    fatal: false\n    parallel_group: g\n{AFTER}"), ""), None);
    assert!(
        result.success,
        "ordinary threaded policy changed: {result:?}"
    );
    assert_eq!(state.agents(), 1);
    assert!(state.events().contains(&"bash:forbidden-bash".into()));
}

#[test]
fn started_workers_preserve_distinct_cleanup_causes() {
    let root = tempfile::tempdir().unwrap();
    let adapter = Scripted::new([])
        .bash("cleanup-first", Reply::Cleanup)
        .bash("cleanup-second", Reply::CleanupSecond);
    let state = adapter.state.clone();
    let paired = PairedCleanup {
        scripted: adapter,
        entered: Mutex::new(0),
        wake: Condvar::new(),
    };
    let result = crate::runner::RecipeRunner::new(paired).with_working_dir(root.path().to_str().unwrap())
        .with_auto_stage(false).execute(&recipe(&format!(
        "  - id: first\n    type: bash\n    command: cleanup-first\n    fatal: false\n    parallel_group: g\n  - id: second\n    type: bash\n    command: cleanup-second\n    fatal: false\n    parallel_group: g\n{AFTER}"), ""), None);
    assert_terminal(&result, &state, 0);
    assert!(state.events().contains(&"bash:cleanup-second".into()));
    assert!(format!("{result:?}").contains("second worker resource teardown failed"));
    assert!(
        result
            .step_results
            .iter()
            .any(|r| r.step_id == "second" && r.status == crate::models::StepStatus::Failed)
    );
}

#[test]
fn completed_parallel_sibling_keeps_status_and_cleanup_keeps_original_index() {
    let root = tempfile::tempdir().unwrap();
    let adapter = Scripted::new([Reply::Success, Reply::Cleanup]);
    let state = adapter.state.clone();
    let result = runner(adapter, root.path()).execute(&recipe(&format!(
        "  - id: completed\n    prompt: success\n    parallel_group: g\n  - id: cleanup\n    prompt: cleanup\n    fatal: false\n    parallel_group: g\n{AFTER}"), ""), None);
    assert_terminal(&result, &state, 2);
    assert_eq!(result.step_results[0].step_id, "completed");
    assert_eq!(
        result.step_results[0].status,
        crate::models::StepStatus::Completed
    );
    assert_eq!(result.step_results[1].step_id, "cleanup");
    assert_eq!(
        result.step_results[1].status,
        crate::models::StepStatus::Failed
    );
}
