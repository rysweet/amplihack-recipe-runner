use super::super::fixtures::*;
use crate::{adapters::Adapter, runner::RecipeRunner};
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, mpsc},
    time::Duration,
};

// Watchdogs bound deadlocks; Condvar/channel acknowledgments establish order.
const WATCHDOG: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool, bool)>, // held worker entered, release it, fence entered
    wake: Condvar,
}
impl Gate {
    fn hold(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.wake.notify_all();
        let (state, timeout) = self
            .wake
            .wait_timeout_while(state, WATCHDOG, |s| !s.1)
            .unwrap();
        assert!(state.1 && !timeout.timed_out(), "worker release watchdog");
    }
    fn await_entered(&self) {
        let state = self.state.lock().unwrap();
        let (state, timeout) = self
            .wake
            .wait_timeout_while(state, WATCHDOG, |s| !s.0)
            .unwrap();
        assert!(state.0 && !timeout.timed_out(), "worker admission watchdog");
    }
    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.wake.notify_all();
    }
    fn fence_entered(&self) {
        self.state.lock().unwrap().2 = true;
        self.wake.notify_all();
    }
    fn await_fence(&self) {
        let state = self.state.lock().unwrap();
        let (state, timeout) = self
            .wake
            .wait_timeout_while(state, WATCHDOG, |s| !s.2)
            .unwrap();
        assert!(
            state.2 && !timeout.timed_out(),
            "in-flight agent admission watchdog"
        );
    }
}
struct ReleaseOnDrop(Arc<Gate>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct Workers {
    scripted: Scripted,
    gate: Arc<Gate>,
    conversion_tx: mpsc::Sender<()>,
    conversion_rx: Mutex<mpsc::Receiver<()>>,
    early: bool,
}
impl Adapter for Workers {
    fn execute_agent_step(
        &self,
        prompt: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
        _: Option<&str>,
        _: Option<u64>,
    ) -> anyhow::Result<String> {
        self.scripted.state.event(format!("agent:{prompt}"));
        if prompt == "fence" {
            // Always release running work, even if the boundary acknowledgment fails.
            let _release = ReleaseOnDrop(self.gate.clone());
            self.gate.fence_entered();
            self.gate.await_entered();
            if self.early {
                self.conversion_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(WATCHDOG)
                    .expect("worker must reach conversion before fence returns");
                self.scripted.state.event("observed-worker-conversion");
                Ok("invalid JSON from admitted agent".into())
            } else {
                Err(cleanup_error())
            }
        } else if prompt == "forbidden-nested" {
            Err(anyhow::anyhow!("ordinary nested failure"))
        } else {
            Ok("STATUS: COMPLETE".into())
        }
    }
    fn execute_bash_step(
        &self,
        command: &str,
        _: &str,
        _: Option<u64>,
        _: &HashMap<String, String>,
    ) -> anyhow::Result<String> {
        self.scripted.state.event(format!("bash:{command}"));
        match command {
            "held-worker" => {
                self.gate.hold();
                self.scripted.state.event("finished-held-worker");
                Ok("joined".into())
            }
            "notified-cleanup" => {
                self.gate.await_entered();
                self.gate.await_fence();
                let error = cleanup_error();
                crate::runner::cleanup_test_observer::install(self.conversion_tx.clone());
                Err(error)
            }
            _ => {
                self.scripted.state.event(format!("finished:{command}"));
                Ok("STATUS: COMPLETE".into())
            }
        }
    }
    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "codex"
    }
}

fn worker_admission(early: bool, excess_bash: bool) {
    let root = tempfile::tempdir().unwrap();
    let scripted = Scripted::new([]);
    let state = scripted.state.clone();
    let gate = Arc::new(Gate::default());
    let _release = ReleaseOnDrop(gate.clone());
    let (tx, rx) = mpsc::channel();
    let adapter = Workers {
        scripted,
        gate,
        conversion_tx: tx,
        conversion_rx: Mutex::new(rx),
        early,
    };
    let failure = if early {
        "  - id: worker-cleanup\n    type: bash\n    command: notified-cleanup\n    fatal: false\n    parallel_group: g\n"
    } else {
        ""
    };
    let admitted = if excess_bash {
        (0..48).map(|n| format!("  - id: admitted-{n}\n    type: bash\n    command: admitted-{n}\n    parallel_group: g\n")).collect::<String>()
    } else {
        String::new()
    };
    let json = if early { "    parse_json: true\n" } else { "" };
    write_child(
        root.path(),
        "pending-child",
        "  - id: nested-primary\n    prompt: forbidden-nested\n",
    );
    let recipe = recipe(
        &format!(
            "  - id: held\n    type: bash\n    command: held-worker\n    parallel_group: g\n{failure}{admitted}  - id: fence\n    prompt: fence\n{json}    fatal: false\n    parallel_group: g\n  - id: pending-recipe\n    type: recipe\n    recipe: pending-child\n    recovery_on_failure: true\n    fatal: false\n    parallel_group: g\n  - id: pending-agent\n    prompt: forbidden-group\n    parse_json: true\n    parallel_group: g\n  - id: pending-bash\n    type: bash\n    command: forbidden-group-bash\n    parallel_group: g\n{AFTER}"
        ),
        "hooks:\n  post_step: forbidden-post-hook\n  on_error: forbidden-error-hook\n",
    );
    let result = RecipeRunner::new(adapter)
        .with_working_dir(root.path().to_str().unwrap())
        .with_auto_stage(false)
        .execute(&recipe, None);
    let events = state.events();
    // These checks precede failure assertions, proving the ordering oracle ran.
    assert!(
        events.contains(&"finished-held-worker".into()),
        "started worker not joined: {events:?}"
    );
    assert!(result.step_results.iter().any(|r| r.step_id == "held"));
    if early {
        assert!(events.contains(&"observed-worker-conversion".into()));
    }
    if excess_bash {
        assert_eq!(
            events
                .iter()
                .filter(|e| e.starts_with("finished:admitted-"))
                .count(),
            48
        );
        assert_eq!(
            result
                .step_results
                .iter()
                .filter(|r| r.step_id.starts_with("admitted-"))
                .count(),
            48,
            "all admitted workers must be joined and retained"
        );
    }
    assert_terminal(&result, &state, 1);
    let failed_id = if early { "worker-cleanup" } else { "fence" };
    assert!(
        result
            .step_results
            .iter()
            .any(|r| r.step_id == failed_id && r.status == crate::models::StepStatus::Failed),
        "triggering worker misattributed: {result:?}"
    );
}

#[test]
fn worker_cleanup_publishes_before_join_and_stops_pending_dispatch() {
    worker_admission(true, false);
}

#[test]
fn synchronous_cleanup_joins_started_worker_and_blocks_pending_dispatch() {
    worker_admission(false, false);
}

#[test]
fn worker_cleanup_blocks_excess_bash_fallback_and_joins_all_50_workers() {
    worker_admission(true, true);
}
