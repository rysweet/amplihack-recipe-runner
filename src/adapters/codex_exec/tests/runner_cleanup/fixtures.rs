use super::super::super::{CleanupFailure, Interruption, finish_resources};
use crate::{
    adapters::Adapter,
    models::{Recipe, RecipeResult, StepResult, StepStatus},
    parser::RecipeParser,
    runner::{ExecutionListener, RecipeRunner},
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy)]
pub(super) enum Reply {
    Success,
    Cleanup,
    CleanupSecond,
    Ordinary,
    InvalidJson,
}

pub(super) fn cleanup_error() -> anyhow::Error {
    cleanup_error_with("owned resource teardown failed")
}

fn cleanup_error_with(detail: &'static str) -> anyhow::Error {
    // Compose exactly as the production facade does; never add Interruption.
    let error = finish_resources(
        Ok("successful primary execution".into()),
        Err(anyhow::anyhow!(detail)),
    )
    .unwrap_err()
    .context("safe adapter boundary");
    assert!(error.downcast_ref::<CleanupFailure>().is_some());
    assert!(error.downcast_ref::<Interruption>().is_none());
    error
}

fn respond(reply: Reply) -> anyhow::Result<String> {
    match reply {
        Reply::Success => Ok("STATUS: COMPLETE".into()),
        Reply::Cleanup => Err(cleanup_error()),
        Reply::CleanupSecond => Err(cleanup_error_with("second worker resource teardown failed")),
        // Marker-like text must never create typed terminality.
        Reply::Ordinary => Err(anyhow::anyhow!("ordinary cleanup/cancelled failure")),
        Reply::InvalidJson => Ok("invalid JSON".into()),
    }
}

#[derive(Default)]
pub(super) struct State {
    pub events: Mutex<Vec<String>>,
    pub completed: Mutex<Vec<StepResult>>,
}

impl State {
    pub fn event(&self, event: impl Into<String>) {
        self.events.lock().unwrap().push(event.into());
    }
    pub fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
    pub fn agents(&self) -> usize {
        self.events()
            .iter()
            .filter(|e| e.starts_with("agent:"))
            .count()
    }
}

#[derive(Clone)]
pub(super) struct Scripted {
    pub state: Arc<State>,
    replies: Arc<Mutex<VecDeque<Reply>>>,
    bash: Arc<HashMap<String, Reply>>,
    provider: &'static str,
}

impl Scripted {
    pub fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self {
            state: Arc::default(),
            replies: Arc::new(Mutex::new(replies.into_iter().collect())),
            bash: Arc::default(),
            provider: "codex",
        }
    }
    pub fn provider(mut self, provider: &'static str) -> Self {
        self.provider = provider;
        self
    }
    pub fn bash(mut self, command: &str, reply: Reply) -> Self {
        Arc::make_mut(&mut self.bash).insert(command.into(), reply);
        self
    }
}

impl Adapter for Scripted {
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
        self.state
            .event(format!("agent:{}", prompt.lines().next().unwrap_or("")));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Reply::Success);
        respond(reply)
    }
    fn execute_bash_step(
        &self,
        command: &str,
        _: &str,
        _: Option<u64>,
        _: &HashMap<String, String>,
    ) -> anyhow::Result<String> {
        self.state.event(format!("bash:{command}"));
        respond(self.bash.get(command).copied().unwrap_or(Reply::Success))
    }
    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        self.provider
    }
}

struct Recording(Arc<State>);
impl ExecutionListener for Recording {
    fn on_step_complete(&self, result: &StepResult) {
        self.0.completed.lock().unwrap().push(result.clone());
    }
}

pub(super) fn runner(adapter: Scripted, root: &std::path::Path) -> RecipeRunner<Scripted> {
    let listener = Recording(adapter.state.clone());
    RecipeRunner::new(adapter)
        .with_working_dir(root.to_str().unwrap())
        .with_auto_stage(false)
        .with_listener(Box::new(listener))
}

pub(super) fn recipe(steps: &str, hooks: &str) -> Recipe {
    RecipeParser::new()
        .parse(&format!("name: cleanup-contract\n{hooks}steps:\n{steps}"))
        .unwrap()
}

pub(super) const AFTER: &str = "  - id: later-agent\n    prompt: forbidden-agent\n  - id: later-bash\n    type: bash\n    command: forbidden-bash\n";

pub(super) fn assert_terminal(result: &RecipeResult, state: &State, agents: usize) {
    let events = state.events();
    assert_eq!(
        state.agents(),
        agents,
        "unexpected repair/recovery/later agent: {events:?}"
    );
    assert!(
        !events.iter().any(|e| e.contains("forbidden")),
        "forbidden dispatch: {events:?}"
    );
    assert!(!result.success, "cleanup must fail recipe: {result:?}");
    assert!(
        result
            .step_results
            .iter()
            .any(|s| s.status == StepStatus::Failed)
    );
    assert!(
        format!("{result:?}").contains("owned resource teardown failed"),
        "lost cleanup: {result:?}"
    );
}

pub(super) fn write_child(root: &std::path::Path, name: &str, steps: &str) {
    std::fs::write(
        root.join(format!("{name}.yaml")),
        format!("name: {name}\nsteps:\n{steps}"),
    )
    .unwrap();
}
