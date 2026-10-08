//! Runner workflow contract independent of subprocess transport implementation.
use recipe_runner_rs::{
    adapters::Adapter, agent_resolver::AgentResolver, parser::RecipeParser, runner::RecipeRunner,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, PartialEq)]
struct Call {
    prompt: String,
    agent: Option<String>,
    system: Option<String>,
    mode: Option<String>,
    cwd: String,
    model: Option<String>,
    timeout: Option<u64>,
}
struct RecordingAdapter(Arc<Mutex<Vec<Call>>>, &'static str);
impl Adapter for RecordingAdapter {
    fn execute_agent_step(
        &self,
        prompt: &str,
        agent: Option<&str>,
        system: Option<&str>,
        mode: Option<&str>,
        cwd: &str,
        model: Option<&str>,
        timeout: Option<u64>,
    ) -> anyhow::Result<String> {
        let mut calls = self.0.lock().unwrap();
        calls.push(Call {
            prompt: prompt.into(),
            agent: agent.map(str::to_owned),
            system: system.map(str::to_owned),
            mode: mode.map(str::to_owned),
            cwd: cwd.into(),
            model: model.map(str::to_owned),
            timeout,
        });
        Ok(if calls.len() == 1 {
            "invalid json"
        } else {
            r#"{"repaired":true}"#
        }
        .into())
    }
    fn execute_bash_step(
        &self,
        _: &str,
        _: &str,
        _: Option<u64>,
        _: &HashMap<String, String>,
    ) -> anyhow::Result<String> {
        unreachable!()
    }
    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        self.1
    }
}

#[test]
fn json_repair_retains_effective_persona_model_mode_cwd_and_timeout() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("persona.md"), "PERSONA_SENTINEL").unwrap();
    let recipe = RecipeParser::new()
        .parse(
            r#"
name: retry-context
steps:
  - id: repair
    agent: test:persona
    prompt: "TASK_SENTINEL"
    mode: autonomous
    model: explicit-model
    timeout: 17
    parse_json: true
    parse_json_required: true
    auto_stage: false
"#,
        )
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let result = RecipeRunner::new(RecordingAdapter(calls.clone(), "codex"))
        .with_working_dir(root.path().to_str().unwrap())
        .with_auto_stage(false)
        .with_agent_resolver(AgentResolver::new(Some(vec![root.path().to_owned()])))
        .execute(&recipe, None);
    assert!(result.success);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(
        calls[0]
            .system
            .as_deref()
            .unwrap()
            .contains("PERSONA_SENTINEL")
    );
    let mut repair = calls[1].clone();
    assert!(repair.prompt.starts_with(&calls[0].prompt));
    assert!(repair.prompt.contains("valid JSON"));
    repair.prompt = calls[0].prompt.clone();
    assert_eq!(
        repair, calls[0],
        "JSON repair must preserve original context"
    );
}

struct OutputAdapter(String);
impl Adapter for OutputAdapter {
    fn execute_agent_step(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
        _: Option<&str>,
        _: Option<u64>,
    ) -> anyhow::Result<String> {
        Ok(self.0.clone())
    }
    fn execute_bash_step(
        &self,
        _: &str,
        _: &str,
        _: Option<u64>,
        _: &HashMap<String, String>,
    ) -> anyhow::Result<String> {
        Ok(self.0.clone())
    }
    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "codex"
    }
}

#[test]
fn runner_rejects_oversized_codex_success_instead_of_truncating() {
    let recipe = RecipeParser::new()
        .parse(
            "name: output-bound\nsteps:\n  - id: result\n    prompt: task\n    auto_stage: false\n",
        )
        .unwrap();
    let output = "x".repeat(recipe_runner_rs::runner::MAX_STEP_OUTPUT_BYTES + 1);
    let result = RecipeRunner::new(OutputAdapter(output))
        .with_auto_stage(false)
        .execute(&recipe, None);
    assert!(
        !result.success,
        "oversized Codex success must become a clear failure"
    );
}

#[test]
fn other_providers_keep_historical_json_repair_context() {
    for provider in ["claude", "copilot"] {
        let recipe = RecipeParser::new().parse(
            "name: repair\nsteps:\n  - id: repair\n    prompt: task\n    agent: missing:persona\n    mode: autonomous\n    model: explicit\n    timeout: 17\n    parse_json: true\n    auto_stage: false\n"
        ).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        assert!(
            RecipeRunner::new(RecordingAdapter(calls.clone(), provider))
                .with_auto_stage(false)
                .execute(&recipe, None)
                .success
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        let repair = &calls[1];
        assert_eq!(
            (
                &repair.agent,
                &repair.system,
                &repair.mode,
                &repair.model,
                repair.timeout
            ),
            (&None, &None, &None, &None, None)
        );
    }
}

#[test]
fn oversized_bash_under_codex_keeps_truncation() {
    let recipe = RecipeParser::new().parse(
        "name: bash-bound\nsteps:\n  - id: result\n    type: bash\n    command: ignored\n    auto_stage: false\n"
    ).unwrap();
    let result = RecipeRunner::new(OutputAdapter(
        "x".repeat(recipe_runner_rs::runner::MAX_STEP_OUTPUT_BYTES + 1),
    ))
    .with_auto_stage(false)
    .execute(&recipe, None);
    assert!(result.success);
    assert_eq!(
        result.step_results[0].output.len(),
        recipe_runner_rs::runner::MAX_STEP_OUTPUT_BYTES
    );
}
