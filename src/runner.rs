/// Recipe execution engine.
///
/// Runs a parsed Recipe step-by-step through an adapter, managing context
/// accumulation, conditional execution, template rendering, and fail-fast behavior.
///
use crate::adapters::Adapter;
use crate::agent_resolver::{AgentResolveError, AgentResolver};
use crate::context::RecipeContext;
use crate::discovery;
use crate::models::{
    Recipe, RecipeResult, Step, StepExecutionError, StepResult, StepStatus, StepType,
};
use crate::parser::{RecipeParser, resolve_extends};
use log::{error, info, warn};
use regex::Regex;
use serde_json::Value;
use std::cell::Cell;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::Instant;

use crate::models::DEFAULT_MAX_DEPTH;

static JSON_FENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```(?:json)?\s*\n?(.*?)\n?\s*```").unwrap());

/// Callback trait for step execution progress events.
pub trait ExecutionListener {
    fn on_step_start(&self, step_id: &str, step_type: StepType) {
        let _ = (step_id, step_type);
    }
    fn on_step_complete(&self, result: &StepResult) {
        let _ = result;
    }
    fn on_output(&self, step_id: &str, line: &str) {
        let _ = (step_id, line);
    }
}

/// No-op listener.
pub struct NullListener;
impl ExecutionListener for NullListener {}

/// Stderr progress listener (for --progress flag).
pub struct StderrListener;
impl ExecutionListener for StderrListener {
    fn on_step_start(&self, step_id: &str, step_type: StepType) {
        eprintln!("▶ {} ({:?})", step_id, step_type);
    }
    fn on_step_complete(&self, result: &StepResult) {
        let icon = match result.status {
            StepStatus::Completed => "✓",
            StepStatus::Skipped => "⊘",
            StepStatus::Failed => "✗",
            _ => "?",
        };
        let dur = result
            .duration
            .map(|d| format!(" ({:.1}s)", d.as_secs_f64()))
            .unwrap_or_default();
        eprintln!("  {} {}{}", icon, result.step_id, dur);
    }
}

/// Executes recipes by delegating steps to an adapter.
pub struct RecipeRunner<A: Adapter> {
    adapter: A,
    agent_resolver: AgentResolver,
    working_dir: String,
    dry_run: bool,
    auto_stage: bool,
    depth: Cell<u32>,
    total_steps: Cell<u32>,
    max_depth: Cell<u32>,
    max_total_steps: Cell<u32>,
    recipe_search_dirs: Vec<PathBuf>,
    audit_dir: Option<PathBuf>,
    active_tags: Vec<String>,
    exclude_tags: Vec<String>,
    listener: Box<dyn ExecutionListener>,
}

impl<A: Adapter> RecipeRunner<A> {
    pub fn new(adapter: A) -> Self {
        Self {
            adapter,
            agent_resolver: AgentResolver::default(),
            working_dir: ".".to_string(),
            dry_run: false,
            auto_stage: true,
            depth: Cell::new(0),
            total_steps: Cell::new(0),
            max_depth: Cell::new(DEFAULT_MAX_DEPTH),
            max_total_steps: Cell::new(200),
            recipe_search_dirs: Vec::new(),
            audit_dir: None,
            active_tags: Vec::new(),
            exclude_tags: Vec::new(),
            listener: Box::new(NullListener),
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

    #[cfg(test)]
    pub fn with_depth(self, depth: u32) -> Self {
        self.depth.set(depth);
        self
    }

    pub fn with_audit_dir(mut self, dir: PathBuf) -> Self {
        self.audit_dir = Some(dir);
        self
    }

    pub fn with_tags(mut self, include: Vec<String>, exclude: Vec<String>) -> Self {
        self.active_tags = include;
        self.exclude_tags = exclude;
        self
    }

    pub fn with_listener(mut self, listener: Box<dyn ExecutionListener>) -> Self {
        self.listener = listener;
        self
    }

    /// Execute a recipe and return the result.
    pub fn execute(
        &self,
        recipe: &Recipe,
        user_context: Option<HashMap<String, Value>>,
    ) -> RecipeResult {
        // Resolve extends (single-level inheritance) if set
        let mut recipe = recipe.clone();
        if recipe.extends.is_some()
            && let Err(e) = resolve_extends(&mut recipe, &self.recipe_search_dirs)
        {
            error!("Failed to resolve extends: {}", e);
            return RecipeResult {
                recipe_name: recipe.name.clone(),
                success: false,
                step_results: vec![],
                context: HashMap::new(),
                duration: None,
            };
        }

        // Apply recipe-level recursion limits
        self.max_depth.set(recipe.recursion.max_depth);
        self.max_total_steps.set(recipe.recursion.max_total_steps);

        if !self.dry_run && !self.adapter.is_available() {
            return RecipeResult {
                recipe_name: recipe.name.clone(),
                success: false,
                step_results: vec![],
                context: HashMap::new(),
                duration: None,
            };
        }

        let start = Instant::now();

        // Build initial context from recipe defaults + user overrides
        let mut initial: HashMap<String, Value> = recipe.context.clone();
        if let Some(uc) = user_context {
            initial.extend(uc);
        }
        let mut ctx = RecipeContext::new(initial);

        let mut step_results = Vec::new();
        let mut success = true;

        // Initialize audit log
        let audit_file = self.open_audit_log(&recipe.name);

        let mut step_idx = 0;
        while step_idx < recipe.steps.len() {
            if recipe.steps[step_idx].parallel_group.is_some() {
                // Collect consecutive steps sharing the same parallel_group
                let group_name = recipe.steps[step_idx]
                    .parallel_group
                    .as_ref()
                    .unwrap()
                    .clone();
                let group_start = step_idx;
                while step_idx < recipe.steps.len()
                    && recipe.steps[step_idx].parallel_group.as_deref() == Some(&group_name)
                {
                    step_idx += 1;
                }
                let group_steps: Vec<&Step> = recipe.steps[group_start..step_idx].iter().collect();

                // Check total step limit before the group
                if self.total_steps.get() >= self.max_total_steps.get() {
                    error!(
                        "Total step limit ({}) reached, stopping execution",
                        self.max_total_steps.get()
                    );
                    step_results.push(StepResult {
                        step_id: group_steps[0].id.clone(),
                        status: StepStatus::Failed,
                        output: String::new(),
                        error: format!(
                            "Total step limit ({}) exceeded",
                            self.max_total_steps.get()
                        ),
                        duration: None,
                    });
                    success = false;
                    break;
                }

                // Notify listeners and run pre_step hooks for all steps in the group
                for gs in &group_steps {
                    self.listener.on_step_start(&gs.id, gs.effective_type());
                    self.run_hook(&recipe.hooks.pre_step, "pre_step", &gs.id, &ctx);
                }

                // Execute group (bash steps in parallel, others sequential)
                let group_results =
                    self.execute_parallel_group(&group_steps, &recipe, &ctx, &*self.listener);

                // Merge results into context in step order for determinism
                let mut group_failed = false;
                for (gs, result) in group_steps.iter().zip(group_results.into_iter()) {
                    self.total_steps.set(self.total_steps.get() + 1);
                    let failed = result.status == StepStatus::Failed;

                    if failed {
                        self.run_hook(&recipe.hooks.on_error, "on_error", &gs.id, &ctx);
                    } else {
                        self.run_hook(&recipe.hooks.post_step, "post_step", &gs.id, &ctx);
                    }

                    self.listener.on_step_complete(&result);
                    self.write_audit_entry(&audit_file, &result);

                    // Store output in context in step order
                    if !failed && let Some(ref output_key) = gs.output {
                        let value = serde_json::from_str(&result.output)
                            .unwrap_or(Value::String(result.output.clone()));
                        ctx.set(output_key, value);
                    }

                    if failed && !gs.continue_on_error {
                        group_failed = true;
                    }

                    if failed && gs.continue_on_error {
                        warn!(
                            "Step '{}' failed but continue_on_error is set, continuing",
                            gs.id
                        );
                    }

                    step_results.push(result);
                }

                if group_failed {
                    success = false;
                    break;
                }
            } else {
                let step = &recipe.steps[step_idx];

                // Check total step limit
                if self.total_steps.get() >= self.max_total_steps.get() {
                    error!(
                        "Total step limit ({}) reached, stopping execution",
                        self.max_total_steps.get()
                    );
                    step_results.push(StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Failed,
                        output: String::new(),
                        error: format!(
                            "Total step limit ({}) exceeded",
                            self.max_total_steps.get()
                        ),
                        duration: None,
                    });
                    success = false;
                    break;
                }

                // Tag filtering
                if self.should_skip_by_tags(step) {
                    info!("Skipping step '{}': excluded by tag filter", step.id);
                    step_results.push(StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Skipped,
                        output: String::new(),
                        error: String::new(),
                        duration: None,
                    });
                    step_idx += 1;
                    continue;
                }

                self.listener.on_step_start(&step.id, step.effective_type());

                // Run pre_step hook
                self.run_hook(&recipe.hooks.pre_step, "pre_step", &step.id, &ctx);

                let result = self.execute_step(step, &mut ctx);
                self.total_steps.set(self.total_steps.get() + 1);

                let failed = result.status == StepStatus::Failed;

                // Run post_step or on_error hook
                if failed {
                    self.run_hook(&recipe.hooks.on_error, "on_error", &step.id, &ctx);
                } else {
                    self.run_hook(&recipe.hooks.post_step, "post_step", &step.id, &ctx);
                }

                self.listener.on_step_complete(&result);
                self.write_audit_entry(&audit_file, &result);

                if failed && !step.continue_on_error {
                    step_results.push(result);
                    success = false;
                    break;
                }

                if failed && step.continue_on_error {
                    warn!(
                        "Step '{}' failed but continue_on_error is set, continuing",
                        step.id
                    );
                }

                step_results.push(result);
                step_idx += 1;
            }
        }

        RecipeResult {
            recipe_name: recipe.name.clone(),
            success,
            step_results,
            context: ctx.to_map(),
            duration: Some(start.elapsed()),
        }
    }

    fn should_skip_by_tags(&self, step: &Step) -> bool {
        if step.when_tags.is_empty() {
            return false;
        }
        // If exclude_tags match any step tag, skip
        if !self.exclude_tags.is_empty() {
            for tag in &step.when_tags {
                if self.exclude_tags.contains(tag) {
                    return true;
                }
            }
        }
        // If active_tags is set, step must have at least one matching tag
        if !self.active_tags.is_empty() {
            return !step.when_tags.iter().any(|t| self.active_tags.contains(t));
        }
        false
    }

    fn run_hook(&self, hook: &Option<String>, hook_name: &str, step_id: &str, ctx: &RecipeContext) {
        if let Some(cmd) = hook {
            let rendered = ctx.render_shell(cmd);
            info!("Running {} hook for step '{}'", hook_name, step_id);
            if let Err(e) = self
                .adapter
                .execute_bash_step(&rendered, &self.working_dir, Some(30))
            {
                warn!("{} hook failed for step '{}': {}", hook_name, step_id, e);
            }
        }
    }

    fn open_audit_log(&self, recipe_name: &str) -> Option<std::fs::File> {
        let dir = self.audit_dir.as_ref()?;
        if let Err(e) = std::fs::create_dir_all(dir) {
            warn!("Failed to create audit log directory: {}", e);
            return None;
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let path = dir.join(format!("{}-{}.jsonl", recipe_name, ts));
        match std::fs::File::create(&path) {
            Ok(f) => Some(f),
            Err(e) => {
                warn!("Failed to create audit log file: {}", e);
                None
            }
        }
    }

    fn write_audit_entry(&self, file: &Option<std::fs::File>, result: &StepResult) {
        if let Some(mut f) = file.as_ref().and_then(|f| f.try_clone().ok()) {
            let entry = serde_json::json!({
                "step_id": result.step_id,
                "status": format!("{}", result.status),
                "duration_ms": result.duration.map(|d| d.as_millis()),
                "error": if result.error.is_empty() { None } else { Some(&result.error) },
                "output_len": result.output.len(),
            });
            let _ = writeln!(f, "{}", entry);
        }
    }

    fn execute_step(&self, step: &Step, ctx: &mut RecipeContext) -> StepResult {
        let step_start = Instant::now();

        if self.dry_run {
            info!("DRY RUN: would execute step '{}'", step.id);
            let output = if step.parse_json {
                format!(r#"{{"dry_run":true,"step":"{}"}}"#, step.id)
            } else {
                "[dry run]".to_string()
            };
            // Populate context placeholder so downstream steps can reference this output
            if let Some(ref output_key) = step.output {
                ctx.set(
                    output_key,
                    serde_json::Value::String("(dry-run)".to_string()),
                );
            }
            return StepResult {
                step_id: step.id.clone(),
                status: StepStatus::Skipped,
                output,
                error: String::new(),
                duration: Some(step_start.elapsed()),
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
                        duration: Some(step_start.elapsed()),
                    };
                }
                Err(e) => {
                    error!("Condition evaluation FAILED for step '{}': {}", step.id, e);
                    return StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Failed,
                        output: String::new(),
                        error: format!("Condition error: {}", e),
                        duration: Some(step_start.elapsed()),
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
                    duration: Some(step_start.elapsed()),
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
                                        crate::safe_truncate(&retry_output, 200)
                                    );
                                    return StepResult {
                                        step_id: step.id.clone(),
                                        status: StepStatus::Failed,
                                        output: String::new(),
                                        error: "parse_json failed after retry: output is not valid JSON".to_string(),
                                        duration: Some(step_start.elapsed()),
                                    };
                                }
                            }
                        }
                        None => {
                            error!(
                                "Step '{}': parse_json failed and retry not possible. Raw: {}...",
                                step.id,
                                crate::safe_truncate(&output, 200)
                            );
                            return StepResult {
                                step_id: step.id.clone(),
                                status: StepStatus::Failed,
                                output: String::new(),
                                error: "parse_json failed: output is not valid JSON".to_string(),
                                duration: Some(step_start.elapsed()),
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
            let value =
                serde_json::from_str(&final_output).unwrap_or(Value::String(final_output.clone()));
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
            duration: Some(step_start.elapsed()),
        }
    }

    fn dispatch_step(
        &self,
        step: &Step,
        ctx: &mut RecipeContext,
    ) -> Result<String, StepExecutionError> {
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
                        Err(AgentResolveError::NotFound { .. })
                        | Err(AgentResolveError::InvalidReference(_)) => {
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
                        step.timeout,
                    )
                    .map_err(|e| StepExecutionError {
                        step_id: step.id.clone(),
                        message: e.to_string(),
                    })
            }
        }
    }

    fn execute_sub_recipe(
        &self,
        step: &Step,
        ctx: &mut RecipeContext,
    ) -> Result<String, StepExecutionError> {
        let current_depth = self.depth.get();
        if current_depth >= self.max_depth.get() {
            return Err(StepExecutionError {
                step_id: step.id.clone(),
                message: format!(
                    "Maximum recipe recursion depth ({}) exceeded. Check for circular recipe references.",
                    self.max_depth.get()
                ),
            });
        }

        let recipe_name = step.recipe.as_ref().ok_or_else(|| StepExecutionError {
            step_id: step.id.clone(),
            message: "Recipe step is missing the 'recipe' field".to_string(),
        })?;

        // Use discovery module to find the recipe, falling back to local search dirs
        let path = self
            .find_recipe_path(recipe_name)
            .ok_or_else(|| StepExecutionError {
                step_id: step.id.clone(),
                message: format!("Sub-recipe '{}' not found", recipe_name),
            })?;

        let parser = RecipeParser::new();
        let mut sub_recipe =
            parser
                .parse_file(Path::new(&path))
                .map_err(|e| StepExecutionError {
                    step_id: step.id.clone(),
                    message: format!("Failed to parse sub-recipe '{}': {}", recipe_name, e),
                })?;

        // Resolve extends (single-level inheritance) if the sub-recipe uses it
        if sub_recipe.extends.is_some() {
            resolve_extends(&mut sub_recipe, &self.recipe_search_dirs).map_err(|e| {
                StepExecutionError {
                    step_id: step.id.clone(),
                    message: format!(
                        "Failed to resolve extends for sub-recipe '{}': {}",
                        recipe_name, e
                    ),
                }
            })?;
        }

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

        // Increment depth, execute, then restore
        self.depth.set(current_depth + 1);
        let sub_result = self.execute_with_depth(&sub_recipe, Some(merged));
        self.depth.set(current_depth);

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
            current_depth + 1
        );
        Ok(format!("{}", sub_result))
    }

    /// Execute a recipe at the current recursion depth.
    ///
    /// Uses the same per-step logic as `execute()` — parallel groups, hooks,
    /// tag filtering, audit logging, and listener notifications are all honored.
    fn execute_with_depth(
        &self,
        recipe: &Recipe,
        user_context: Option<HashMap<String, Value>>,
    ) -> RecipeResult {
        let start = std::time::Instant::now();
        let mut initial: HashMap<String, Value> = recipe.context.clone();
        if let Some(uc) = user_context {
            initial.extend(uc);
        }
        let mut ctx = RecipeContext::new(initial);

        let mut step_results = Vec::new();
        let mut success = true;

        let audit_file = self.open_audit_log(&recipe.name);

        let mut step_idx = 0;
        while step_idx < recipe.steps.len() {
            if recipe.steps[step_idx].parallel_group.is_some() {
                let group_name = recipe.steps[step_idx]
                    .parallel_group
                    .as_ref()
                    .unwrap()
                    .clone();
                let group_start = step_idx;
                while step_idx < recipe.steps.len()
                    && recipe.steps[step_idx].parallel_group.as_deref() == Some(&group_name)
                {
                    step_idx += 1;
                }
                let group_steps: Vec<&Step> = recipe.steps[group_start..step_idx].iter().collect();

                if self.total_steps.get() >= self.max_total_steps.get() {
                    error!(
                        "Total step limit ({}) reached, stopping execution",
                        self.max_total_steps.get()
                    );
                    step_results.push(StepResult {
                        step_id: group_steps[0].id.clone(),
                        status: StepStatus::Failed,
                        output: String::new(),
                        error: format!(
                            "Total step limit ({}) exceeded",
                            self.max_total_steps.get()
                        ),
                        duration: None,
                    });
                    success = false;
                    break;
                }

                for gs in &group_steps {
                    self.listener.on_step_start(&gs.id, gs.effective_type());
                    self.run_hook(&recipe.hooks.pre_step, "pre_step", &gs.id, &ctx);
                }

                let group_results =
                    self.execute_parallel_group(&group_steps, recipe, &ctx, &*self.listener);

                let mut group_failed = false;
                for (gs, result) in group_steps.iter().zip(group_results.into_iter()) {
                    self.total_steps.set(self.total_steps.get() + 1);
                    let failed = result.status == StepStatus::Failed;

                    if failed {
                        self.run_hook(&recipe.hooks.on_error, "on_error", &gs.id, &ctx);
                    } else {
                        self.run_hook(&recipe.hooks.post_step, "post_step", &gs.id, &ctx);
                    }

                    self.listener.on_step_complete(&result);
                    self.write_audit_entry(&audit_file, &result);

                    if !failed && let Some(ref output_key) = gs.output {
                        let value = serde_json::from_str(&result.output)
                            .unwrap_or(Value::String(result.output.clone()));
                        ctx.set(output_key, value);
                    }

                    if failed && !gs.continue_on_error {
                        group_failed = true;
                    }

                    if failed && gs.continue_on_error {
                        warn!(
                            "Step '{}' failed but continue_on_error is set, continuing",
                            gs.id
                        );
                    }

                    step_results.push(result);
                }

                if group_failed {
                    success = false;
                    break;
                }
            } else {
                let step = &recipe.steps[step_idx];

                if self.total_steps.get() >= self.max_total_steps.get() {
                    error!(
                        "Total step limit ({}) reached, stopping execution",
                        self.max_total_steps.get()
                    );
                    step_results.push(StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Failed,
                        output: String::new(),
                        error: format!(
                            "Total step limit ({}) exceeded",
                            self.max_total_steps.get()
                        ),
                        duration: None,
                    });
                    success = false;
                    break;
                }

                if self.should_skip_by_tags(step) {
                    info!("Skipping step '{}': excluded by tag filter", step.id);
                    step_results.push(StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Skipped,
                        output: String::new(),
                        error: String::new(),
                        duration: None,
                    });
                    step_idx += 1;
                    continue;
                }

                self.listener.on_step_start(&step.id, step.effective_type());
                self.run_hook(&recipe.hooks.pre_step, "pre_step", &step.id, &ctx);

                let result = self.execute_step(step, &mut ctx);
                self.total_steps.set(self.total_steps.get() + 1);

                let failed = result.status == StepStatus::Failed;

                if failed {
                    self.run_hook(&recipe.hooks.on_error, "on_error", &step.id, &ctx);
                } else {
                    self.run_hook(&recipe.hooks.post_step, "post_step", &step.id, &ctx);
                }

                self.listener.on_step_complete(&result);
                self.write_audit_entry(&audit_file, &result);

                if failed && !step.continue_on_error {
                    step_results.push(result);
                    success = false;
                    break;
                }

                if failed && step.continue_on_error {
                    warn!(
                        "Step '{}' failed but continue_on_error is set, continuing",
                        step.id
                    );
                }

                step_results.push(result);
                step_idx += 1;
            }
        }

        RecipeResult {
            recipe_name: recipe.name.clone(),
            success,
            step_results,
            context: ctx.to_map(),
            duration: Some(start.elapsed()),
        }
    }

    fn find_recipe_path(&self, name: &str) -> Option<String> {
        // First try discovery module
        if !self.recipe_search_dirs.is_empty()
            && let Some(path) = discovery::find_recipe(name, Some(&self.recipe_search_dirs))
        {
            return Some(path.display().to_string());
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
            step.timeout,
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

    /// Execute a group of steps that share the same `parallel_group`.
    ///
    /// Bash steps run concurrently via `std::thread::scope`; non-bash steps
    /// (agent, recipe) fall back to sequential execution within the group
    /// since adapters may not be thread-safe for those step types.
    fn execute_parallel_group(
        &self,
        steps: &[&Step],
        _recipe: &Recipe,
        ctx: &RecipeContext,
        _listener: &dyn ExecutionListener,
    ) -> Vec<StepResult> {
        let adapter = &self.adapter;
        let default_wd = self.working_dir.as_str();
        let dry_run = self.dry_run;
        let mut results: Vec<Option<StepResult>> = vec![None; steps.len()];

        std::thread::scope(|s| {
            let mut handles = Vec::new();

            for (idx, step) in steps.iter().enumerate() {
                if self.should_skip_by_tags(step) {
                    results[idx] = Some(StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Skipped,
                        output: String::new(),
                        error: String::new(),
                        duration: None,
                    });
                    continue;
                }

                if step.effective_type() == StepType::Bash {
                    let ctx_clone = ctx.clone();
                    let handle = s.spawn(move || {
                        Self::execute_bash_step_parallel(
                            step, &ctx_clone, adapter, default_wd, dry_run,
                        )
                    });
                    handles.push((idx, handle));
                } else {
                    // Non-bash steps fall back to sequential on the main thread
                    let mut ctx_clone = ctx.clone();
                    let result = self.execute_step(step, &mut ctx_clone);
                    results[idx] = Some(result);
                }
            }

            for (idx, handle) in handles {
                match handle.join() {
                    Ok(result) => results[idx] = Some(result),
                    Err(_) => {
                        results[idx] = Some(StepResult {
                            step_id: steps[idx].id.clone(),
                            status: StepStatus::Failed,
                            output: String::new(),
                            error: "Thread panicked during parallel execution".to_string(),
                            duration: None,
                        });
                    }
                }
            }
        });

        results.into_iter().flatten().collect()
    }

    /// Execute a single bash step in a parallel context without `&mut RecipeContext`.
    fn execute_bash_step_parallel(
        step: &Step,
        ctx: &RecipeContext,
        adapter: &A,
        default_working_dir: &str,
        dry_run: bool,
    ) -> StepResult {
        let step_start = Instant::now();

        if dry_run {
            return StepResult {
                step_id: step.id.clone(),
                status: StepStatus::Completed,
                output: "[dry run]".to_string(),
                error: String::new(),
                duration: Some(step_start.elapsed()),
            };
        }

        if let Some(ref condition) = step.condition {
            match ctx.evaluate(condition) {
                Ok(true) => {}
                Ok(false) => {
                    return StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Skipped,
                        output: String::new(),
                        error: String::new(),
                        duration: Some(step_start.elapsed()),
                    };
                }
                Err(e) => {
                    return StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Failed,
                        output: String::new(),
                        error: format!("Condition error: {}", e),
                        duration: Some(step_start.elapsed()),
                    };
                }
            }
        }

        let rendered = ctx.render_shell(step.command.as_deref().unwrap_or(""));
        let working_dir = step.working_dir.as_deref().unwrap_or(default_working_dir);

        match adapter.execute_bash_step(&rendered, working_dir, step.timeout) {
            Ok(output) => {
                // Apply parse_json if requested
                let final_output = if step.parse_json && !output.is_empty() {
                    match parse_json_output(&output, &step.id) {
                        Some(parsed) => serde_json::to_string(&parsed).unwrap_or(output),
                        None => {
                            return StepResult {
                                step_id: step.id.clone(),
                                status: StepStatus::Failed,
                                output: String::new(),
                                error: "parse_json failed: output is not valid JSON".to_string(),
                                duration: Some(step_start.elapsed()),
                            };
                        }
                    }
                } else {
                    output
                };
                StepResult {
                    step_id: step.id.clone(),
                    status: StepStatus::Completed,
                    output: final_output,
                    error: String::new(),
                    duration: Some(step_start.elapsed()),
                }
            }
            Err(e) => StepResult {
                step_id: step.id.clone(),
                status: StepStatus::Failed,
                output: String::new(),
                error: e.to_string(),
                duration: Some(step_start.elapsed()),
            },
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
    if let Some(caps) = JSON_FENCE_RE.captures(text)
        && let Some(m) = caps.get(1)
        && let Ok(v) = serde_json::from_str::<Value>(m.as_str().trim())
    {
        return Some(v);
    }

    // Strategy 3: Find first balanced JSON block
    for (open_ch, close_ch) in [('{', '}'), ('[', ']')] {
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

    warn!(
        "All JSON extraction strategies failed for step '{}'",
        step_id
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::Adapter;

    struct MockAdapter;

    impl Adapter for MockAdapter {
        fn execute_agent_step(
            &self,
            prompt: &str,
            _agent_name: Option<&str>,
            _system_prompt: Option<&str>,
            _mode: Option<&str>,
            _working_dir: &str,
            _timeout: Option<u64>,
        ) -> Result<String, anyhow::Error> {
            Ok(format!(
                "Agent response for: {}",
                &prompt[..prompt.len().min(50)]
            ))
        }

        fn execute_bash_step(
            &self,
            command: &str,
            _working_dir: &str,
            _timeout: Option<u64>,
        ) -> Result<String, anyhow::Error> {
            Ok(format!("Bash output for: {}", command))
        }

        fn is_available(&self) -> bool {
            true
        }
        fn name(&self) -> &str {
            "mock"
        }
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

    /// C2-RD-10: timeout:0 edge case — verify step still executes and completes
    /// normally. A zero timeout is passed to the adapter as `Some(0)` and the
    /// adapter decides what to do with it (mock adapter ignores timeout).
    #[test]
    fn test_timeout_zero_executes() {
        let yaml = r#"
name: "test-timeout-zero"
steps:
  - id: "zero-timeout"
    command: "echo hello"
    timeout: 0
    output: "result"
"#;
        let parser = RecipeParser::new();
        let recipe = parser.parse(yaml).unwrap();
        let runner = RecipeRunner::new(MockAdapter);
        let result = runner.execute(&recipe, None);
        assert!(result.success, "timeout:0 step should still succeed");
        assert_eq!(result.step_results.len(), 1);
        assert_eq!(result.step_results[0].status, StepStatus::Completed);
        // Verify the output was stored in context
        assert!(result.context.contains_key("result"));
    }
}
