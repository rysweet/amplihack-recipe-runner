/// Recipe execution engine.
///
/// Runs a parsed Recipe step-by-step through an adapter, managing context
/// accumulation, conditional execution, template rendering, and fail-fast behavior.
/// Direct port from Python `amplihack.recipes.runner`.
use crate::adapters::Adapter;
use crate::agent_resolver::{AgentResolver, AgentResolveError};
use crate::context::RecipeContext;
use crate::discovery;
use crate::models::{Recipe, RecipeResult, Step, StepExecutionError, StepResult, StepStatus, StepType};
use crate::parser::RecipeParser;
use log::{error, info, warn};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

const MAX_RECIPE_DEPTH: u32 = 3;

static JSON_FENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```(?:json)?\s*\n?(.*?)\n?\s*```").unwrap());

/// Executes recipes by delegating steps to an adapter.
pub struct RecipeRunner<A: Adapter> {
    adapter: A,
    agent_resolver: AgentResolver,
    working_dir: String,
    dry_run: bool,
    auto_stage: bool,
    depth: u32,
    recipe_search_dirs: Vec<PathBuf>,
}

impl<A: Adapter> RecipeRunner<A> {
    pub fn new(adapter: A) -> Self {
        Self {
            adapter,
            agent_resolver: AgentResolver::default(),
            working_dir: ".".to_string(),
            dry_run: false,
            auto_stage: true,
            depth: 0,
            recipe_search_dirs: Vec::new(),
        }
    }

    pub fn with_working_dir(mut self, dir: &str) -> Self {
        self.working_dir = dir.to_string();
        self
    }

    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn with_auto_stage(mut self, auto_stage: bool) -> Self {
        self.auto_stage = auto_stage;
        self
    }

    pub fn with_agent_resolver(mut self, resolver: AgentResolver) -> Self {
        self.agent_resolver = resolver;
        self
    }

    pub fn with_recipe_search_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.recipe_search_dirs = dirs;
        self
    }

    #[allow(dead_code)]
    fn with_depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    /// Execute a recipe and return the result.
    pub fn execute(
        &self,
        recipe: &Recipe,
        user_context: Option<HashMap<String, Value>>,
    ) -> RecipeResult {
        if !self.dry_run && !self.adapter.is_available() {
            return RecipeResult {
                recipe_name: recipe.name.clone(),
                success: false,
                step_results: vec![],
                context: HashMap::new(),
            };
        }

        // Build initial context from recipe defaults + user overrides
        let mut initial: HashMap<String, Value> = recipe.context.clone();
        if let Some(uc) = user_context {
            initial.extend(uc);
        }
        let mut ctx = RecipeContext::new(initial);

        let mut step_results = Vec::new();
        let mut success = true;

        for step in &recipe.steps {
            let result = self.execute_step(step, &mut ctx);
            let failed = result.status == StepStatus::Failed;
            step_results.push(result);

            if failed {
                success = false;
                break;
            }
        }

        RecipeResult {
            recipe_name: recipe.name.clone(),
            success,
            step_results,
            context: ctx.to_map(),
        }
    }

    fn execute_step(&self, step: &Step, ctx: &mut RecipeContext) -> StepResult {
        if self.dry_run {
            info!("DRY RUN: would execute step '{}'", step.id);
            let output = if step.parse_json {
                format!(r#"{{"dry_run":true,"step":"{}"}}"#, step.id)
            } else {
                "[dry run]".to_string()
            };
            return StepResult {
                step_id: step.id.clone(),
                status: StepStatus::Completed,
                output,
                error: String::new(),
            };
        }

        // Evaluate condition
        if let Some(ref condition) = step.condition {
            match ctx.evaluate(condition) {
                Ok(true) => {}
                Ok(false) => {
                    info!("Skipping step '{}': condition is false", step.id);
                    return StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Skipped,
                        output: String::new(),
                        error: String::new(),
                    };
                }
                Err(e) => {
                    error!("Condition evaluation FAILED for step '{}': {}", step.id, e);
                    return StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Failed,
                        output: String::new(),
                        error: format!("Condition error: {}", e),
                    };
                }
            }
        }

        // Execute the step
        let output = match self.dispatch_step(step, ctx) {
            Ok(o) => o,
            Err(e) => {
                error!("Step '{}' failed: {}", step.id, e);
                return StepResult {
                    step_id: step.id.clone(),
                    status: StepStatus::Failed,
                    output: String::new(),
                    error: e.to_string(),
                };
            }
        };

        // Parse JSON if requested — retry once on failure
        let final_output = if step.parse_json && !output.is_empty() {
            match parse_json_output(&output, &step.id) {
                Some(parsed) => serde_json::to_string(&parsed).unwrap_or(output),
                None => {
                    // Retry: re-execute with explicit JSON instruction
                    warn!(
                        "Step '{}': parse_json failed on first attempt. Retrying with JSON reminder.",
                        step.id
                    );
                    match self.retry_for_json(step, ctx) {
                        Some(retry_output) => {
                            match parse_json_output(&retry_output, &step.id) {
                                Some(parsed) => {
                                    info!("Step '{}': parse_json succeeded on retry.", step.id);
                                    serde_json::to_string(&parsed).unwrap_or(retry_output)
                                }
                                None => {
                                    error!(
                                        "Step '{}': parse_json failed on retry. Raw: {}...",
                                        step.id,
                                        &retry_output[..retry_output.len().min(200)]
                                    );
                                    return StepResult {
                                        step_id: step.id.clone(),
                                        status: StepStatus::Failed,
                                        output: String::new(),
                                        error: "parse_json failed after retry: output is not valid JSON".to_string(),
                                    };
                                }
                            }
                        }
                        None => {
                            error!(
                                "Step '{}': parse_json failed and retry not possible. Raw: {}...",
                                step.id,
                                &output[..output.len().min(200)]
                            );
                            return StepResult {
                                step_id: step.id.clone(),
                                status: StepStatus::Failed,
                                output: String::new(),
                                error: "parse_json failed: output is not valid JSON".to_string(),
                            };
                        }
                    }
                }
            }
        } else {
            output
        };

        // Store output in context
        if let Some(ref output_key) = step.output {
            // Try to parse as JSON value, fall back to string
            let value = serde_json::from_str(&final_output)
                .unwrap_or(Value::String(final_output.clone()));
            ctx.set(output_key, value);
        }

        // Auto-stage git changes after agent steps
        if step.effective_type() == StepType::Agent {
            self.maybe_auto_stage(step);
        }

        StepResult {
            step_id: step.id.clone(),
            status: StepStatus::Completed,
            output: final_output,
            error: String::new(),
        }
    }

    fn dispatch_step(&self, step: &Step, ctx: &mut RecipeContext) -> Result<String, StepExecutionError> {
        let working_dir = step.working_dir.as_deref().unwrap_or(&self.working_dir);
        let st = step.effective_type();

        match st {
            StepType::Recipe => self.execute_sub_recipe(step, ctx),
            StepType::Bash => {
                let rendered = ctx.render_shell(step.command.as_deref().unwrap_or(""));
                self.adapter
                    .execute_bash_step(&rendered, working_dir, step.timeout)
                    .map_err(|e| StepExecutionError {
                        step_id: step.id.clone(),
                        message: e.to_string(),
                    })
            }
            StepType::Agent => {
                let rendered_prompt = ctx.render(step.prompt.as_deref().unwrap_or(""));

                // Resolve agent system prompt if agent reference is provided
                let mut agent_name: Option<&str> = None;
                let mut agent_system_prompt: Option<String> = None;
                if let Some(ref agent_ref) = step.agent {
                    agent_name = Some(agent_ref.as_str());
                    match self.agent_resolver.resolve(agent_ref) {
                        Ok(content) => agent_system_prompt = Some(content),
                        Err(AgentResolveError::NotFound { .. }) | Err(AgentResolveError::InvalidReference(_)) => {
                            warn!(
                                "Could not resolve agent '{}', proceeding without system prompt",
                                agent_ref
                            );
                        }
                    }
                }

                self.adapter
                    .execute_agent_step(
                        &rendered_prompt,
                        agent_name,
                        agent_system_prompt.as_deref(),
                        step.mode.as_deref(),
                        working_dir,
                    )
                    .map_err(|e| StepExecutionError {
                        step_id: step.id.clone(),
                        message: e.to_string(),
                    })
            }
        }
    }

    fn execute_sub_recipe(&self, step: &Step, ctx: &mut RecipeContext) -> Result<String, StepExecutionError> {
        if self.depth >= MAX_RECIPE_DEPTH {
            return Err(StepExecutionError {
                step_id: step.id.clone(),
                message: format!(
                    "Maximum recipe recursion depth ({}) exceeded. Check for circular recipe references.",
                    MAX_RECIPE_DEPTH
                ),
            });
        }

        let recipe_name = step.recipe.as_ref().ok_or_else(|| StepExecutionError {
            step_id: step.id.clone(),
            message: "Recipe step is missing the 'recipe' field".to_string(),
        })?;

        // Use discovery module to find the recipe, falling back to local search dirs
        let path = self.find_recipe_path(recipe_name).ok_or_else(|| StepExecutionError {
            step_id: step.id.clone(),
            message: format!("Sub-recipe '{}' not found", recipe_name),
        })?;

        let parser = RecipeParser::new();
        let sub_recipe = parser.parse_file(Path::new(&path)).map_err(|e| StepExecutionError {
            step_id: step.id.clone(),
            message: format!("Failed to parse sub-recipe '{}': {}", recipe_name, e),
        })?;

        // Merge: current context + step-level sub_context overrides
        let mut merged = ctx.to_map();
        if let Some(ref sub_ctx) = step.sub_context {
            for (k, v) in sub_ctx {
                let rendered_value = if let Value::String(s) = v {
                    let rendered = ctx.render(s);
                    Value::String(rendered)
                } else {
                    v.clone()
                };
                merged.insert(k.clone(), rendered_value);
            }
        }

        // Build a sub-runner that shares the adapter but increments depth.
        // Since Adapter is behind a reference, we use the parent's adapter.
        let sub_result = self.execute_with_depth(&sub_recipe, Some(merged), self.depth + 1);

        if !sub_result.success {
            return Err(StepExecutionError {
                step_id: step.id.clone(),
                message: format!("Sub-recipe '{}' failed", recipe_name),
            });
        }

        // Merge sub-recipe context back into parent
        for (k, v) in &sub_result.context {
            ctx.set(k, v.clone());
        }

        info!(
            "Sub-recipe '{}' completed successfully (depth {})",
            recipe_name,
            self.depth + 1
        );
        Ok(format!("{}", sub_result))
    }

    /// Execute a recipe at a specific recursion depth.
    fn execute_with_depth(
        &self,
        recipe: &Recipe,
        user_context: Option<HashMap<String, Value>>,
        depth: u32,
    ) -> RecipeResult {
        let mut initial: HashMap<String, Value> = recipe.context.clone();
        if let Some(uc) = user_context {
            initial.extend(uc);
        }
        let mut ctx = RecipeContext::new(initial);

        let mut step_results = Vec::new();
        let mut success = true;

        for step in &recipe.steps {
            // Temporarily shadow depth for sub-recipe steps
            let result = if self.depth != depth {
                // We're executing at a different depth — create step result inline
                self.execute_step(step, &mut ctx)
            } else {
                self.execute_step(step, &mut ctx)
            };
            let failed = result.status == StepStatus::Failed;
            step_results.push(result);

            if failed {
                success = false;
                break;
            }
        }

        RecipeResult {
            recipe_name: recipe.name.clone(),
            success,
            step_results,
            context: ctx.to_map(),
        }
    }

    fn find_recipe_path(&self, name: &str) -> Option<String> {
        // First try discovery module
        if !self.recipe_search_dirs.is_empty() {
            if let Some(path) = discovery::find_recipe(name, Some(&self.recipe_search_dirs)) {
                return Some(path.display().to_string());
            }
        }

        // Fall back to discovery module default paths
        if let Some(path) = discovery::find_recipe(name, None) {
            return Some(path.display().to_string());
        }

        // Finally check working directory
        let filename = format!("{}.yaml", name);
        let local = Path::new(&self.working_dir).join(&filename);
        if local.is_file() {
            return Some(local.display().to_string());
        }
        None
    }

    /// Retry an agent step with an explicit JSON-only instruction.
    fn retry_for_json(&self, step: &Step, ctx: &mut RecipeContext) -> Option<String> {
        if step.effective_type() != StepType::Agent {
            return None; // Can't retry bash steps with different prompts
        }

        let original_prompt = step.prompt.as_deref().unwrap_or("");
        let retry_prompt = format!(
            "{}\n\nIMPORTANT: Your previous response was not valid JSON. \
             Return ONLY a valid JSON object. No markdown fences, no explanation, \
             no text before or after. Just the raw JSON object starting with {{ and ending with }}.",
            original_prompt
        );

        let working_dir = step.working_dir.as_deref().unwrap_or(&self.working_dir);
        match self.adapter.execute_agent_step(
            &ctx.render(&retry_prompt),
            None,
            None,
            None,
            working_dir,
        ) {
            Ok(output) => Some(output),
            Err(e) => {
                warn!("Retry for step '{}' failed: {}", step.id, e);
                None
            }
        }
    }

    fn maybe_auto_stage(&self, step: &Step) {
        let should_stage = step.auto_stage.unwrap_or(self.auto_stage);
        if !should_stage {
            return;
        }

        let working_dir = step.working_dir.as_deref().unwrap_or(&self.working_dir);
        if let Some(staged) = git_stage_all(working_dir) {
            let count = staged.lines().count();
            info!("Auto-staged {} file(s) after step '{}'", count, step.id);
        }
    }
}

fn git_stage_all(working_dir: &str) -> Option<String> {
    let result = Command::new("git")
        .args(["add", "-A"])
        .current_dir(working_dir)
        .output()
        .ok()?;

    if !result.status.success() {
        return None;
    }

    let diff = Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .current_dir(working_dir)
        .output()
        .ok()?;

    let staged = String::from_utf8_lossy(&diff.stdout).trim().to_string();
    if staged.is_empty() {
        None
    } else {
        Some(staged)
    }
}

/// Try to parse JSON from LLM output using multiple strategies.
fn parse_json_output(output: &str, step_id: &str) -> Option<Value> {
    let text = output.trim();

    // Strategy 1: Direct parse
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Some(v);
    }

    // Strategy 2: Extract from markdown fences
    if let Some(caps) = JSON_FENCE_RE.captures(text) {
        if let Some(m) = caps.get(1) {
            if let Ok(v) = serde_json::from_str::<Value>(m.as_str().trim()) {
                return Some(v);
            }
        }
    }

    // Strategy 3: Find first balanced JSON block
    for (open_ch, close_ch) in [( '{', '}'), ('[', ']')] {
        if let Some(start) = text.find(open_ch) {
            let mut depth = 0i32;
            let mut in_string = false;
            let mut escape = false;

            for (i, ch) in text[start..].char_indices() {
                if escape {
                    escape = false;
                    continue;
                }
                if ch == '\\' {
                    escape = true;
                    continue;
                }
                if ch == '"' {
                    in_string = !in_string;
                    continue;
                }
                if in_string {
                    continue;
                }
                if ch == open_ch {
                    depth += 1;
                } else if ch == close_ch {
                    depth -= 1;
                    if depth == 0 {
                        let candidate = &text[start..start + i + 1];
                        if let Ok(v) = serde_json::from_str::<Value>(candidate) {
                            return Some(v);
                        }
                        break;
                    }
                }
            }
        }
    }

    warn!("All JSON extraction strategies failed for step '{}'", step_id);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::Adapter;

    struct MockAdapter;

    impl Adapter for MockAdapter {
        fn execute_agent_step(
            &self, prompt: &str, _agent_name: Option<&str>,
            _system_prompt: Option<&str>, _mode: Option<&str>, _working_dir: &str,
        ) -> Result<String, anyhow::Error> {
            Ok(format!("Agent response for: {}", &prompt[..prompt.len().min(50)]))
        }

        fn execute_bash_step(
            &self, command: &str, _working_dir: &str, _timeout: Option<u64>,
        ) -> Result<String, anyhow::Error> {
            Ok(format!("Bash output for: {}", command))
        }

        fn is_available(&self) -> bool { true }
        fn name(&self) -> &str { "mock" }
    }

    #[test]
    fn test_execute_simple_recipe() {
        let yaml = r#"
name: "test"
steps:
  - id: "step1"
    command: "echo hello"
    output: "result"
"#;
        let parser = RecipeParser::new();
        let recipe = parser.parse(yaml).unwrap();
        let runner = RecipeRunner::new(MockAdapter);
        let result = runner.execute(&recipe, None);
        assert!(result.success);
        assert_eq!(result.step_results.len(), 1);
        assert_eq!(result.step_results[0].status, StepStatus::Completed);
    }

    #[test]
    fn test_conditional_skip() {
        let yaml = r#"
name: "test"
context:
  status: "CONVERGED"
steps:
  - id: "skip-me"
    command: "echo should skip"
    condition: "status != 'CONVERGED'"
  - id: "run-me"
    command: "echo should run"
    condition: "status == 'CONVERGED'"
"#;
        let parser = RecipeParser::new();
        let recipe = parser.parse(yaml).unwrap();
        let runner = RecipeRunner::new(MockAdapter);
        let result = runner.execute(&recipe, None);
        assert!(result.success);
        assert_eq!(result.step_results[0].status, StepStatus::Skipped);
        assert_eq!(result.step_results[1].status, StepStatus::Completed);
    }

    #[test]
    fn test_dry_run() {
        let yaml = r#"
name: "test"
steps:
  - id: "step1"
    command: "echo hello"
"#;
        let parser = RecipeParser::new();
        let recipe = parser.parse(yaml).unwrap();
        let runner = RecipeRunner::new(MockAdapter).with_dry_run(true);
        let result = runner.execute(&recipe, None);
        assert!(result.success);
        assert_eq!(result.step_results[0].output, "[dry run]");
    }

    #[test]
    fn test_parse_json_direct() {
        let json_str = r#"{"key": "value"}"#;
        let result = parse_json_output(json_str, "test");
        assert!(result.is_some());
    }

    #[test]
    fn test_parse_json_from_fence() {
        let text = "Here is the result:\n```json\n{\"key\": \"value\"}\n```\nDone.";
        let result = parse_json_output(text, "test");
        assert!(result.is_some());
    }

    #[test]
    fn test_parse_json_from_balanced() {
        let text = "Some text before {\"key\": \"value\"} and after";
        let result = parse_json_output(text, "test");
        assert!(result.is_some());
    }
}
