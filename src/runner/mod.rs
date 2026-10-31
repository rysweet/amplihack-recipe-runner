/// Recipe execution engine.
///
/// Runs a parsed Recipe step-by-step through an adapter, managing context
/// accumulation, conditional execution, template rendering, and fail-fast behavior.
///
pub mod audit;
pub mod json_parser;
pub mod listeners;
pub mod sub_recipe_paths;
mod terminal;
use terminal::{ParallelOutcome, TerminalFailure, TerminalState};

#[cfg(test)]
pub(crate) mod cleanup_test_observer;

use crate::adapters::Adapter;
use crate::agent_resolver::{AgentResolveError, AgentResolver};
use crate::context::RecipeContext;
use crate::discovery;
use crate::models::{
    Recipe, RecipeResult, Step, StepExecutionError, StepResult, StepStatus, StepType,
};
use crate::parser::{RecipeParser, resolve_extends};
use log::{error, info, warn};
use serde_json::Value;
use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::models::DEFAULT_MAX_DEPTH;

use json_parser::parse_json_output;
pub use listeners::{ExecutionListener, FileLogListener, NullListener, StderrListener};

/// Maximum number of threads to spawn per parallel group.
const MAX_PARALLEL_STEPS: usize = 50;

/// Maximum size of step output stored in memory (10 MB).
pub const MAX_STEP_OUTPUT_BYTES: usize = 10_000_000;

/// Executes recipes by delegating steps to an adapter.
pub struct RecipeRunner<A: Adapter> {
    adapter: A,
    agent_resolver: AgentResolver,
    working_dir: String,
    dry_run: bool,
    auto_stage: bool,
    terminal: TerminalState,
    depth: Cell<u32>,
    total_steps: Cell<u32>,
    max_depth: Cell<u32>,
    max_total_steps: Cell<u32>,
    recipe_search_dirs: Vec<PathBuf>,
    /// Directory containing the top-level recipe file that was loaded at
    /// runner entry. Used as the highest-priority anchor when resolving
    /// sub-recipes referenced by `recipe:`-typed steps. See
    /// [`sub_recipe_paths::anchored_search_dirs`].
    recipe_origin_dir: Option<PathBuf>,
    audit_dir: Option<PathBuf>,
    require_audit: bool,
    active_tags: Vec<String>,
    exclude_tags: Vec<String>,
    listener: Box<dyn ExecutionListener>,
    /// Step IDs to skip (already completed in a prior run).
    resume_completed: Vec<String>,
    /// Whether to save checkpoints after each step.
    save_checkpoints: bool,
}

impl<A: Adapter> RecipeRunner<A> {
    pub fn new(adapter: A) -> Self {
        info!("RecipeRunner::new: creating runner with default settings");
        Self {
            adapter,
            agent_resolver: AgentResolver::default(),
            working_dir: ".".to_string(),
            dry_run: false,
            auto_stage: true,
            terminal: TerminalState::default(),
            depth: Cell::new(0),
            total_steps: Cell::new(0),
            max_depth: Cell::new(DEFAULT_MAX_DEPTH),
            max_total_steps: Cell::new(200),
            recipe_search_dirs: Vec::new(),
            recipe_origin_dir: None,
            audit_dir: None,
            require_audit: false,
            active_tags: Vec::new(),
            exclude_tags: Vec::new(),
            listener: Box::new(NullListener),
            resume_completed: Vec::new(),
            save_checkpoints: false,
        }
    }

    pub fn with_working_dir(mut self, dir: &str) -> Self {
        log::debug!("RecipeRunner::with_working_dir: dir={:?}", dir);
        self.working_dir = dir.to_string();
        self
    }

    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        log::debug!("RecipeRunner::with_dry_run: dry_run={}", dry_run);
        self.dry_run = dry_run;
        self
    }

    pub fn with_auto_stage(mut self, auto_stage: bool) -> Self {
        log::debug!("RecipeRunner::with_auto_stage: auto_stage={}", auto_stage);
        self.auto_stage = auto_stage;
        self
    }

    pub fn with_agent_resolver(mut self, resolver: AgentResolver) -> Self {
        log::debug!("RecipeRunner::with_agent_resolver: setting custom agent resolver");
        self.agent_resolver = resolver;
        self
    }

    pub fn with_recipe_search_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        log::debug!("RecipeRunner::with_recipe_search_dirs: {} dirs", dirs.len());
        self.recipe_search_dirs = dirs;
        self
    }

    /// Set the directory containing the top-level recipe file (typically the
    /// parent of the file path passed to `recipe-runner-rs`). This anchors
    /// sub-recipe resolution to the same location the user invoked the
    /// recipe from, so a sub-recipe co-located with its parent is found
    /// even when `recipe-runner-rs`'s subprocess `cwd` differs from the
    /// invocation directory. See issue rysweet/amplihack-rs#480.
    pub fn with_recipe_origin_dir(mut self, dir: PathBuf) -> Self {
        log::debug!("RecipeRunner::with_recipe_origin_dir: dir={:?}", dir);
        self.recipe_origin_dir = Some(dir);
        self
    }

    #[cfg(test)]
    pub fn with_depth(self, depth: u32) -> Self {
        log::debug!("RecipeRunner::with_depth: depth={}", depth);
        self.depth.set(depth);
        self
    }

    /// When set, a failure to open the audit log fails the run before any
    /// step starts, and a failure to write an entry fails that step and stops
    /// the run.
    pub fn with_require_audit(mut self, require_audit: bool) -> Self {
        log::debug!(
            "RecipeRunner::with_require_audit: require_audit={}",
            require_audit
        );
        self.require_audit = require_audit;
        self
    }

    pub fn with_audit_dir(mut self, dir: PathBuf) -> Self {
        log::debug!("RecipeRunner::with_audit_dir: dir={:?}", dir);
        self.audit_dir = Some(dir);
        self
    }

    pub fn with_tags(mut self, include: Vec<String>, exclude: Vec<String>) -> Self {
        log::debug!(
            "RecipeRunner::with_tags: include={:?}, exclude={:?}",
            include,
            exclude
        );
        self.active_tags = include;
        self.exclude_tags = exclude;
        self
    }

    pub fn with_listener(mut self, listener: Box<dyn ExecutionListener>) -> Self {
        log::debug!("RecipeRunner::with_listener: setting custom execution listener");
        self.listener = listener;
        self
    }

    /// Resume from a checkpoint — skip steps that were already completed.
    pub fn with_resume_from(mut self, checkpoint: &crate::models::RecipeCheckpoint) -> Self {
        log::info!(
            "Resuming from checkpoint: {} completed steps",
            checkpoint.completed_steps.len()
        );
        self.resume_completed = checkpoint.completed_steps.clone();
        self
    }

    /// Enable checkpoint saving after each step.
    pub fn with_checkpoints(mut self, enabled: bool) -> Self {
        self.save_checkpoints = enabled;
        self
    }

    /// Execute a recipe and return the result.
    pub fn execute(
        &self,
        recipe: &Recipe,
        user_context: Option<HashMap<String, Value>>,
    ) -> RecipeResult {
        self.terminal.reset();
        info!(
            "RecipeRunner::execute: recipe='{}', dry_run={}",
            recipe.name, self.dry_run
        );
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

        // Pre-flight context validation (#3741)
        if !recipe.context_validation.is_empty() {
            let merged_ctx = {
                let mut ctx = recipe.context.clone();
                if let Some(ref uc) = user_context {
                    for (k, v) in uc {
                        ctx.insert(k.clone(), v.clone());
                    }
                }
                ctx
            };
            let errors = validate_context(&recipe.context_validation, &merged_ctx);
            if !errors.is_empty() {
                let msg = format!(
                    "=== PRE-FLIGHT VALIDATION FAILED for '{}' ===\n{}\n\nFix the above, then retry.",
                    recipe.name,
                    errors.join("\n")
                );
                error!("{}", msg);
                return RecipeResult {
                    recipe_name: recipe.name.clone(),
                    success: false,
                    step_results: vec![StepResult {
                        step_id: "preflight-validation".to_string(),
                        status: StepStatus::Failed,
                        output: String::new(),
                        error: msg,
                        duration: None,
                    }],
                    context: HashMap::new(),
                    duration: None,
                };
            }
        }

        self.run_steps(&recipe, user_context)
    }

    /// Core step execution loop shared by `execute()` and `execute_with_depth()`.
    ///
    /// Handles serial and parallel step execution, hooks, tag filtering,
    /// audit logging, and listener notifications.
    fn run_steps(
        &self,
        recipe: &Recipe,
        user_context: Option<HashMap<String, Value>>,
    ) -> RecipeResult {
        info!(
            "run_steps: recipe='{}', steps={}",
            recipe.name,
            recipe.steps.len()
        );
        let start = Instant::now();

        let mut initial: HashMap<String, Value> = recipe.context.clone();
        if let Some(uc) = user_context {
            initial.extend(uc);
        }
        let mut ctx = RecipeContext::new(initial);

        let mut step_results = Vec::new();
        let mut success = true;

        let audit_file = self.open_audit_log(&recipe.name);
        if self.require_audit && audit_file.is_none() {
            error!(
                "Audit log for recipe '{}' could not be opened and an audit log is required; no step runs",
                recipe.name
            );
            return RecipeResult {
                recipe_name: recipe.name.clone(),
                success: false,
                step_results: vec![StepResult {
                    step_id: "audit-log".to_string(),
                    status: StepStatus::Failed,
                    output: String::new(),
                    error: "audit log could not be opened and an audit log is required".to_string(),
                    duration: None,
                }],
                context: ctx.to_map(),
                duration: Some(start.elapsed()),
            };
        }

        let mut step_idx = 0;
        while step_idx < recipe.steps.len() {
            if let Some(group_name) = &recipe.steps[step_idx].parallel_group {
                let group_name = group_name.clone();
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
                    let failed_id = group_steps
                        .first()
                        .map(|s| s.id.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    step_results.push(StepResult {
                        step_id: failed_id,
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

                let mut pre_hook_failure = None;
                for (idx, gs) in group_steps.iter().enumerate() {
                    self.listener.on_step_start(&gs.id, gs.effective_type());
                    self.run_hook(&recipe.hooks.pre_step, "pre_step", &gs.id, &ctx);
                    if self.terminal.is_terminal() {
                        pre_hook_failure = Some(idx);
                        break;
                    }
                }

                let group_results = if let Some(idx) = pre_hook_failure {
                    vec![(idx, self.execute_step(group_steps[idx], &mut ctx))]
                } else {
                    self.execute_parallel_group(&group_steps, recipe, &ctx, &*self.listener)
                };

                let mut group_failed = false;
                for (idx, mut result) in group_results {
                    let gs = group_steps[idx];
                    self.total_steps.set(self.total_steps.get() + 1);
                    let failed = result.status == StepStatus::Failed;

                    let terminal_before_hook = self.terminal.is_terminal();
                    if failed {
                        self.run_hook(&recipe.hooks.on_error, "on_error", &gs.id, &ctx);
                    } else {
                        self.run_hook(&recipe.hooks.post_step, "post_step", &gs.id, &ctx);
                    }

                    // Retain outcomes of siblings that completed before terminal
                    // observation. A newly terminal hook belongs to this step.
                    if !terminal_before_hook {
                        self.terminal.apply(&mut result);
                    }
                    let audit_failed = self.audit_entry_failed(&audit_file, &mut result);
                    let failed = result.status == StepStatus::Failed;
                    self.listener.on_step_complete(&result);

                    if !failed && let Some(ref output_key) = gs.output {
                        let value = match serde_json::from_str(&result.output) {
                            Ok(v) => v,
                            Err(_) => Value::String(result.output.clone()),
                        };
                        ctx.set(output_key, value);
                    }

                    if self.terminal.is_terminal() || audit_failed || (failed && !gs.is_nonfatal())
                    {
                        group_failed = true;
                    }

                    if failed && gs.is_nonfatal() && !self.terminal.is_terminal() {
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

                // Resume support: skip steps already completed in a prior run
                if self.resume_completed.contains(&step.id) {
                    info!("Skipping step '{}': already completed (resume)", step.id);
                    step_results.push(StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Skipped,
                        output: String::new(),
                        error: "skipped (resume from checkpoint)".to_string(),
                        duration: None,
                    });
                    step_idx += 1;
                    continue;
                }

                self.listener.on_step_start(&step.id, step.effective_type());
                self.run_hook(&recipe.hooks.pre_step, "pre_step", &step.id, &ctx);

                let mut result = self.execute_step(step, &mut ctx);
                self.total_steps.set(self.total_steps.get() + 1);

                let failed = result.status == StepStatus::Failed;

                if failed {
                    self.run_hook(&recipe.hooks.on_error, "on_error", &step.id, &ctx);
                } else {
                    self.run_hook(&recipe.hooks.post_step, "post_step", &step.id, &ctx);
                }

                self.terminal.apply(&mut result);
                let audit_failed = self.audit_entry_failed(&audit_file, &mut result);
                let failed = result.status == StepStatus::Failed;
                self.listener.on_step_complete(&result);

                // Save checkpoint after each step for resume-on-failure
                if self.save_checkpoints && !failed {
                    let completed: Vec<String> = step_results
                        .iter()
                        .filter(|r| r.status == StepStatus::Completed)
                        .map(|r| r.step_id.clone())
                        .chain(std::iter::once(step.id.clone()))
                        .collect();
                    let checkpoint = crate::models::RecipeCheckpoint {
                        recipe_name: recipe.name.clone(),
                        completed_steps: completed,
                        context: ctx.data().clone(),
                        timestamp: chrono_now(),
                    };
                    if let Err(e) = checkpoint.save(&recipe.name) {
                        warn!("Failed to save checkpoint: {}", e);
                    }
                }

                if self.terminal.is_terminal() || audit_failed || (failed && !step.is_nonfatal()) {
                    step_results.push(result);
                    success = false;
                    break;
                }

                if failed && step.is_nonfatal() && !self.terminal.is_terminal() {
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
            success: success && !self.terminal.is_terminal(),
            step_results,
            context: ctx.to_map(),
            duration: Some(start.elapsed()),
        }
    }

    fn should_skip_by_tags(&self, step: &Step) -> bool {
        log::debug!(
            "should_skip_by_tags: step='{}', when_tags={:?}",
            step.id,
            step.when_tags
        );
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
        if self.terminal.is_terminal() {
            return;
        }
        if let Some(cmd) = hook {
            // A dry run executes nothing, hooks included: they are arbitrary
            // shell commands with effects of their own.
            if self.dry_run {
                info!(
                    "DRY RUN: would run {} hook for step '{}'",
                    hook_name, step_id
                );
                return;
            }
            let rendered = ctx.render_shell(cmd);
            let env_vars = ctx.shell_env_for_step();
            info!("Running {} hook for step '{}'", hook_name, step_id);
            if let Err(e) =
                self.adapter
                    .execute_bash_step(&rendered, &self.working_dir, Some(30), &env_vars)
            {
                self.terminal.observe(&e);
                warn!("{} hook failed for step '{}': {}", hook_name, step_id, e);
            }
        }
    }

    fn open_audit_log(&self, recipe_name: &str) -> Option<std::fs::File> {
        log::debug!("open_audit_log: recipe_name={:?}", recipe_name);
        let dir = self.audit_dir.as_ref()?;
        audit::open_audit_log(dir, recipe_name)
    }

    /// Writes the step's audit entry. When an audit log is required and the
    /// entry cannot be written, marks the step failed and returns `true`.
    fn audit_entry_failed(&self, file: &Option<std::fs::File>, result: &mut StepResult) -> bool {
        if self.write_audit_entry(file, result) || !self.require_audit {
            return false;
        }
        result.status = StepStatus::Failed;
        result.error = if result.error.is_empty() {
            "audit log entry could not be written and an audit log is required".to_string()
        } else {
            format!(
                "{}; audit log entry could not be written and an audit log is required",
                result.error
            )
        };
        true
    }

    fn write_audit_entry(&self, file: &Option<std::fs::File>, result: &StepResult) -> bool {
        log::debug!(
            "write_audit_entry: step_id={:?}, status={:?}",
            result.step_id,
            result.status
        );
        audit::write_audit_entry(file, result)
    }

    fn execute_step(&self, step: &Step, ctx: &mut RecipeContext) -> StepResult {
        let step_start = Instant::now();
        if let Some(error) = self.terminal.diagnostic() {
            return StepResult {
                step_id: step.id.clone(),
                status: StepStatus::Failed,
                output: String::new(),
                error,
                duration: Some(step_start.elapsed()),
            };
        }

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
            Ok(o) => {
                if o.len() > MAX_STEP_OUTPUT_BYTES
                    && self.adapter.name() == "codex"
                    && step.effective_type() == StepType::Agent
                {
                    return StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Failed,
                        output: String::new(),
                        error: format!(
                            "Codex final output exceeds {MAX_STEP_OUTPUT_BYTES}-byte limit"
                        ),
                        duration: Some(step_start.elapsed()),
                    };
                }
                if o.len() > MAX_STEP_OUTPUT_BYTES {
                    warn!(
                        "Step '{}' output truncated from {} to {} bytes",
                        step.id,
                        o.len(),
                        MAX_STEP_OUTPUT_BYTES
                    );
                    crate::safe_truncate(&o, MAX_STEP_OUTPUT_BYTES).to_string()
                } else {
                    o
                }
            }
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

        // A worker may publish terminality during this admitted primary call.
        if let Some(error) = self.terminal.diagnostic() {
            return StepResult {
                step_id: step.id.clone(),
                status: StepStatus::Failed,
                output: String::new(),
                error,
                duration: Some(step_start.elapsed()),
            };
        }

        // Parse JSON if requested — retry once on failure.
        // When parse_json fails, respect continue_on_error: if set, complete
        // with raw output instead of failing the recipe (#2954).
        let (final_output, step_status, step_error) = if step.parse_json && !output.is_empty() {
            match parse_json_output(&output, &step.id) {
                Some(parsed) => (
                    serde_json::to_string(&parsed).unwrap_or_else(|_| output.clone()),
                    StepStatus::Completed,
                    String::new(),
                ),
                None => {
                    // Retry: re-execute with explicit JSON instruction
                    warn!(
                        "Step '{}': parse_json failed on first attempt. Retrying with JSON reminder.",
                        step.id
                    );
                    let retry_result = self.retry_for_json(step, ctx).and_then(|retry_output| {
                        parse_json_output(&retry_output, &step.id).map(|parsed| {
                            info!("Step '{}': parse_json succeeded on retry.", step.id);
                            serde_json::to_string(&parsed).unwrap_or(retry_output)
                        })
                    });

                    if let Some(diagnostic) = self.terminal.diagnostic().as_ref() {
                        return StepResult {
                            step_id: step.id.clone(),
                            status: StepStatus::Failed,
                            output: String::new(),
                            error: diagnostic.clone(),
                            duration: Some(step_start.elapsed()),
                        };
                    }

                    match retry_result {
                        Some(parsed_output) => {
                            (parsed_output, StepStatus::Completed, String::new())
                        }
                        None => {
                            if step.parse_json_required {
                                error!(
                                    "Step '{}': parse_json failed (parse_json_required=true). Raw: {}...",
                                    step.id,
                                    crate::safe_truncate(&output, 200)
                                );
                                (
                                    String::new(),
                                    StepStatus::Failed,
                                    "parse_json failed: output is not valid JSON".to_string(),
                                )
                            } else {
                                warn!(
                                    "Step '{}': parse_json failed, using raw output (degraded)",
                                    step.id
                                );
                                (output, StepStatus::Degraded, String::new())
                            }
                        }
                    }
                }
            }
        } else {
            (output, StepStatus::Completed, String::new())
        };

        // Store output in context (only if step didn't fail; degraded steps store raw output)
        if step_status != StepStatus::Failed
            && let Some(ref output_key) = step.output
        {
            let value = match serde_json::from_str(&final_output) {
                Ok(v) => v,
                Err(_) => Value::String(final_output.clone()),
            };
            ctx.set(output_key, value);
        }

        // Auto-stage git changes after agent steps
        if step.effective_type() == StepType::Agent {
            self.maybe_auto_stage(step);
        }

        StepResult {
            step_id: step.id.clone(),
            status: step_status,
            output: final_output,
            error: step_error,
            duration: Some(step_start.elapsed()),
        }
    }

    fn dispatch_step(
        &self,
        step: &Step,
        ctx: &mut RecipeContext,
    ) -> Result<String, StepExecutionError> {
        log::debug!(
            "dispatch_step: step='{}', type={:?}",
            step.id,
            step.effective_type()
        );
        if let Some(message) = self.terminal.diagnostic() {
            return Err(StepExecutionError {
                step_id: step.id.clone(),
                message,
            });
        }
        // Render working_dir through template engine so {{worktree_setup.worktree_path}} resolves
        let raw_working_dir = step.working_dir.as_deref().unwrap_or(&self.working_dir);
        let working_dir_rendered = ctx.render(raw_working_dir);
        let working_dir = working_dir_rendered.as_str();
        let st = step.effective_type();

        match st {
            StepType::Recipe => self.execute_sub_recipe(step, ctx),
            StepType::Bash => {
                let rendered = ctx.render_shell(step.command.as_deref().unwrap_or(""));
                let env_vars = ctx.shell_env_for_step();
                self.adapter
                    .execute_bash_step(&rendered, working_dir, step.timeout, &env_vars)
                    .map(|output| output.trim_end().to_string())
                    .map_err(|e| {
                        self.terminal.observe(&e);
                        StepExecutionError {
                            step_id: step.id.clone(),
                            message: format!("bash step failed: {:#}", e),
                        }
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
                        step.model.as_deref(),
                        step.timeout,
                    )
                    .map_err(|e| {
                        self.terminal.observe(&e);
                        StepExecutionError {
                            step_id: step.id.clone(),
                            message: format!("agent step failed: {:#}", e),
                        }
                    })
            }
        }
    }

    fn execute_sub_recipe(
        &self,
        step: &Step,
        ctx: &mut RecipeContext,
    ) -> Result<String, StepExecutionError> {
        log::debug!(
            "execute_sub_recipe: step='{}', recipe={:?}, depth={}",
            step.id,
            step.recipe,
            self.depth.get()
        );
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
                message: format!(
                    "Sub-recipe '{}' not found. Searched the following directories (in order):\n{}",
                    recipe_name,
                    self.sub_recipe_search_diagnostic()
                ),
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
            let failure_summary = self.describe_sub_recipe_failure(&sub_result);
            if step.recovery_on_failure && !self.terminal.is_terminal() {
                let working_dir = step.working_dir.as_deref().unwrap_or(&self.working_dir);
                let recovery_prompt = format!(
                    "Sub-recipe '{}' failed.\n{}\n\n\
                     Attempt to complete the remaining work. If you succeed, end with 'STATUS: COMPLETE'. \
                     If recovery is impossible, explain why.",
                    recipe_name, failure_summary
                );

                // The recovery call works on the step's behalf, so it keeps the
                // step's model rather than falling back to the CLI default.
                let recovery = self.adapter.execute_agent_step(
                    &recovery_prompt,
                    None,
                    None,
                    None,
                    working_dir,
                    step.model.as_deref(),
                    None, // timeout
                );
                if let Err(ref error) = recovery {
                    self.terminal.observe(error);
                }
                match recovery {
                    Ok(output)
                        if !self.terminal.is_terminal()
                            && (output.to_lowercase().contains("status: complete")
                                || output.to_lowercase().contains("recovered")) =>
                    {
                        info!("Sub-recipe '{}' recovered via agent", recipe_name);
                        return Ok(output);
                    }
                    _ => {
                        let mut message = format!(
                            "Sub-recipe '{}' failed and agentic recovery was unsuccessful.\n{}",
                            recipe_name, failure_summary
                        );
                        if let Some(diagnostic) = self.terminal.diagnostic().as_ref() {
                            message.push('\n');
                            message.push_str(diagnostic);
                        }
                        return Err(StepExecutionError {
                            step_id: step.id.clone(),
                            message,
                        });
                    }
                }
            }

            return Err(StepExecutionError {
                step_id: step.id.clone(),
                message: format!("Sub-recipe '{}' failed.\n{}", recipe_name, failure_summary),
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

    fn describe_sub_recipe_failure(&self, sub_result: &RecipeResult) -> String {
        log::debug!(
            "describe_sub_recipe_failure: recipe='{}'",
            sub_result.recipe_name
        );
        let failed: Vec<String> = sub_result
            .step_results
            .iter()
            .filter(|r| r.status == StepStatus::Failed)
            .map(|r| {
                let (detail_source, detail) = if !r.error.trim().is_empty() {
                    ("error", r.error.trim().to_string())
                } else if !r.output.trim().is_empty() {
                    (
                        "output tail",
                        crate::safe_tail(r.output.trim(), 200).to_string(),
                    )
                } else {
                    ("no detail", "no additional detail".to_string())
                };
                format!("- {} [{}]: {}", r.step_id, detail_source, detail)
            })
            .collect();
        let completed: Vec<String> = sub_result
            .step_results
            .iter()
            .filter(|r| r.status == StepStatus::Completed)
            .map(|r| r.step_id.clone())
            .collect();

        let mut sections: Vec<String> = Vec::new();
        if !failed.is_empty() {
            sections.push(format!("Failed steps:\n{}", failed.join("\n")));
        }
        if !completed.is_empty() {
            sections.push(format!("Completed steps: {}", completed.join(", ")));
        }
        if sections.is_empty() {
            "No child step details captured.".to_string()
        } else {
            sections.join("\n")
        }
    }

    /// Execute a recipe at the current recursion depth.
    ///
    /// Delegates to the shared `run_steps()` implementation. Unlike `execute()`,
    /// this skips extends resolution and recursion limit setup (already done by caller).
    fn execute_with_depth(
        &self,
        recipe: &Recipe,
        user_context: Option<HashMap<String, Value>>,
    ) -> RecipeResult {
        log::debug!(
            "execute_with_depth: recipe='{}', depth={}",
            recipe.name,
            self.depth.get()
        );
        self.run_steps(recipe, user_context)
    }

    fn find_recipe_path(&self, name: &str) -> Option<String> {
        log::debug!("find_recipe_path: name={:?}", name);
        // Security: reject names with path separators / parent-dir markers
        // before any filesystem resolution. Failures bubble up as part of
        // the Zero-BS diagnostic emitted by the caller.
        if let Err(reason) = sub_recipe_paths::validate_sub_recipe_name(name) {
            log::warn!(
                "find_recipe_path: rejecting unsafe sub-recipe name {:?}: {}",
                name,
                reason
            );
            return None;
        }

        // 1. Explicit user-provided search dirs win first (matches the prior
        //    behavior; preserves -R / --recipe-dir override semantics).
        if !self.recipe_search_dirs.is_empty()
            && let Some(path) = discovery::find_recipe(name, Some(&self.recipe_search_dirs))
        {
            return Some(path.display().to_string());
        }

        // 2. Anchored search: recipe-local → working_dir → walk-up to .git.
        //    These dirs are computed from the runner's `working_dir` (-C
        //    arg) rather than the subprocess cwd, so resolution is stable
        //    regardless of how `recipe-runner-rs` was invoked.
        let anchored = sub_recipe_paths::anchored_search_dirs(
            self.recipe_origin_dir.as_deref(),
            Path::new(&self.working_dir),
        );
        if !anchored.is_empty()
            && let Some(path) = discovery::find_recipe(name, Some(&anchored))
        {
            // Defense in depth: ensure the resolved file physically lives
            // within one of the anchored roots, so a symlink in those roots
            // cannot be used to read an arbitrary file off the filesystem.
            if sub_recipe_paths::is_within_any(&path, &anchored) {
                return Some(path.display().to_string());
            }
            log::warn!(
                "find_recipe_path: rejecting candidate {} — resolves outside anchored search roots",
                path.display()
            );
        }

        // 3. Fall back to discovery module's default search dirs
        //    (~/.amplihack, $AMPLIHACK_HOME, etc.).
        if let Some(path) = discovery::find_recipe(name, None) {
            return Some(path.display().to_string());
        }

        None
    }

    /// Build the human-readable list of directories that were consulted while
    /// trying to resolve a sub-recipe. Used to produce a Zero-BS diagnostic
    /// when resolution fails — the user must be told exactly where we looked
    /// (paths only; no raw env-var values, to avoid leaking secrets).
    fn sub_recipe_search_diagnostic(&self) -> String {
        let mut all: Vec<PathBuf> = Vec::new();
        all.extend(self.recipe_search_dirs.iter().cloned());
        all.extend(sub_recipe_paths::anchored_search_dirs(
            self.recipe_origin_dir.as_deref(),
            Path::new(&self.working_dir),
        ));
        // The discovery module's defaults are private, but their contents
        // are stable and documented; enumerate them here so the diagnostic
        // is complete without exposing internal helpers.
        if let Some(home) = dirs::home_dir() {
            all.push(
                home.join(".amplihack")
                    .join("amplifier-bundle")
                    .join("recipes"),
            );
            all.push(home.join(".amplihack").join(".claude").join("recipes"));
        }
        if let Ok(amplihack_home) = std::env::var("AMPLIHACK_HOME")
            && !amplihack_home.is_empty()
        {
            all.push(
                PathBuf::from(amplihack_home)
                    .join("amplifier-bundle")
                    .join("recipes"),
            );
        }
        all.push(PathBuf::from("amplifier-bundle").join("recipes"));
        all.push(
            PathBuf::from("src")
                .join("amplihack")
                .join("amplifier-bundle")
                .join("recipes"),
        );
        all.push(PathBuf::from(".claude").join("recipes"));

        // De-dupe while preserving order.
        let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let mut lines = String::new();
        for d in all {
            if seen.insert(d.clone()) {
                lines.push_str("  - ");
                lines.push_str(&d.display().to_string());
                if !d.is_dir() {
                    lines.push_str(" (does not exist)");
                }
                lines.push('\n');
            }
        }
        lines
    }

    /// Retry an agent step with an explicit JSON-only instruction.
    fn retry_for_json(&self, step: &Step, ctx: &mut RecipeContext) -> Option<String> {
        log::debug!("retry_for_json: step='{}'", step.id);
        if self.terminal.is_terminal() || step.effective_type() != StepType::Agent {
            return None; // No pending repair after terminal observation
        }

        let original_prompt = step.prompt.as_deref().unwrap_or("");
        let retry_prompt = format!(
            "{}\n\nIMPORTANT: Your previous response was not valid JSON. \
             Return ONLY a valid JSON object. No markdown fences, no explanation, \
             no text before or after. Just the raw JSON object starting with {{ and ending with }}.",
            original_prompt
        );

        let result = if self.adapter.name() == "codex" {
            let mut repair_step = step.clone();
            repair_step.prompt = Some(retry_prompt);
            self.dispatch_step(&repair_step, ctx)
                .map_err(anyhow::Error::new)
        } else {
            // The repair call keeps the step's model, as the Codex path above
            // does by re-dispatching a copy of the step.
            self.adapter.execute_agent_step(
                &ctx.render(&retry_prompt),
                None,
                None,
                None,
                step.working_dir.as_deref().unwrap_or(&self.working_dir),
                step.model.as_deref(),
                None,
            )
        };
        match result {
            Ok(output) => Some(output),
            Err(e) => {
                self.terminal.observe(&e);
                warn!("Retry for step '{}' failed: {}", step.id, e);
                None
            }
        }
    }

    fn maybe_auto_stage(&self, step: &Step) {
        log::debug!(
            "maybe_auto_stage: step='{}', auto_stage={:?}",
            step.id,
            step.auto_stage
        );
        // A step can opt out of staging, but cannot opt in when the runner has
        // staging turned off (for example with --no-auto-stage).
        let should_stage = self.auto_stage && step.auto_stage.unwrap_or(true);
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
    ) -> Vec<(usize, StepResult)> {
        log::debug!("execute_parallel_group: {} steps", steps.len());
        if steps.len() > MAX_PARALLEL_STEPS {
            warn!(
                "Parallel group has {} steps (limit {}); excess steps will run sequentially",
                steps.len(),
                MAX_PARALLEL_STEPS
            );
        }
        let adapter = &self.adapter;
        let default_wd = self.working_dir.as_str();
        let dry_run = self.dry_run;
        let mut results: Vec<Option<StepResult>> = vec![None; steps.len()];
        if self.terminal.is_terminal() {
            let mut ctx_clone = ctx.clone();
            return vec![(0, self.execute_step(steps[0], &mut ctx_clone))];
        }
        let terminal = &self.terminal;

        std::thread::scope(|s| {
            let mut handles = Vec::new();

            for (idx, step) in steps.iter().enumerate() {
                if self.terminal.is_terminal() {
                    break;
                }
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

                if step.effective_type() == StepType::Bash && handles.len() < MAX_PARALLEL_STEPS {
                    let ctx_clone = ctx.clone();
                    let handle = s.spawn(move || {
                        Self::execute_bash_step_parallel(
                            step, &ctx_clone, adapter, default_wd, dry_run, terminal,
                        )
                    });
                    handles.push((idx, handle));
                } else {
                    // Non-bash steps or excess parallel steps fall back to sequential
                    let mut ctx_clone = ctx.clone();
                    let result = self.execute_step(step, &mut ctx_clone);
                    results[idx] = Some(result);
                }
            }

            for (idx, handle) in handles {
                match handle.join() {
                    Ok(outcome) => {
                        self.terminal.merge(outcome.terminal);
                        results[idx] = Some(outcome.result);
                    }
                    Err(panic_info) => {
                        let panic_msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                            s.clone()
                        } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                            s.to_string()
                        } else {
                            "unknown panic".to_string()
                        };
                        log::error!(
                            "Thread panicked during parallel step '{}': {}",
                            steps[idx].id,
                            panic_msg
                        );
                        results[idx] = Some(StepResult {
                            step_id: steps[idx].id.clone(),
                            status: StepStatus::Failed,
                            output: String::new(),
                            error: format!(
                                "Thread panicked during parallel execution: {}",
                                panic_msg
                            ),
                            duration: None,
                        });
                    }
                }
            }
        });

        results
            .into_iter()
            .enumerate()
            .filter_map(|(idx, result)| result.map(|result| (idx, result)))
            .collect()
    }

    /// Execute a single bash step in a parallel context without `&mut RecipeContext`.
    fn execute_bash_step_parallel(
        step: &Step,
        ctx: &RecipeContext,
        adapter: &A,
        default_working_dir: &str,
        dry_run: bool,
        terminal: &TerminalState,
    ) -> ParallelOutcome {
        log::debug!(
            "execute_bash_step_parallel: step='{}', dry_run={}",
            step.id,
            dry_run
        );
        let step_start = Instant::now();

        if dry_run {
            return StepResult {
                step_id: step.id.clone(),
                status: StepStatus::Skipped,
                output: "(dry-run)".to_string(),
                error: String::new(),
                duration: Some(step_start.elapsed()),
            }
            .into();
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
                    }
                    .into();
                }
                Err(e) => {
                    return StepResult {
                        step_id: step.id.clone(),
                        status: StepStatus::Failed,
                        output: String::new(),
                        error: format!("Condition error: {}", e),
                        duration: Some(step_start.elapsed()),
                    }
                    .into();
                }
            }
        }

        let rendered = ctx.render_shell(step.command.as_deref().unwrap_or(""));
        let env_vars = ctx.shell_env_for_step();
        let working_dir = step.working_dir.as_deref().unwrap_or(default_working_dir);

        let mut failure = None;
        let result = match adapter.execute_bash_step(
            &rendered,
            working_dir,
            step.timeout,
            &env_vars,
        ) {
            Ok(raw_output) => {
                // Strip trailing whitespace/newlines from bash output so that
                // condition comparisons like `count != '1'` work correctly.
                let output = raw_output.trim_end().to_string();
                // Apply parse_json if requested
                let (final_output, status, error) = if step.parse_json && !output.is_empty() {
                    match parse_json_output(&output, &step.id) {
                        Some(parsed) => (
                            serde_json::to_string(&parsed).unwrap_or(output),
                            StepStatus::Completed,
                            String::new(),
                        ),
                        None => {
                            if step.parse_json_required {
                                (
                                    String::new(),
                                    StepStatus::Failed,
                                    "parse_json failed: output is not valid JSON".to_string(),
                                )
                            } else {
                                warn!(
                                    "Step '{}': parse_json failed on bash step, using raw output (degraded)",
                                    step.id
                                );
                                (output, StepStatus::Degraded, String::new())
                            }
                        }
                    }
                } else {
                    (output, StepStatus::Completed, String::new())
                };
                StepResult {
                    step_id: step.id.clone(),
                    status,
                    output: final_output,
                    error,
                    duration: Some(step_start.elapsed()),
                }
            }
            Err(e) => {
                failure = TerminalFailure::classify(&e);
                terminal.merge(failure.clone());
                // Test-only acknowledgment at the actual worker error conversion.
                // Publication precedes this acknowledgment and owner joins.
                #[cfg(test)]
                cleanup_test_observer::converted();
                StepResult {
                    step_id: step.id.clone(),
                    status: StepStatus::Failed,
                    output: String::new(),
                    error: format!("{e:#}"),
                    duration: Some(step_start.elapsed()),
                }
            }
        };
        ParallelOutcome {
            result,
            terminal: failure,
        }
    }
}

/// Validate context variables against the recipe's context_validation rules.
/// Returns a list of error messages (empty = all valid).
fn validate_context(rules: &HashMap<String, String>, ctx: &HashMap<String, Value>) -> Vec<String> {
    let mut errors = Vec::new();
    for (var_name, rule) in rules {
        let value = ctx.get(var_name);
        match rule.as_str() {
            "nonempty" => {
                let is_empty = match value {
                    None => true,
                    Some(Value::Null) => true,
                    Some(Value::String(s)) => s.trim().is_empty(),
                    _ => false,
                };
                if is_empty {
                    errors.push(format!(
                        "  ✗ '{}' is required but empty or missing",
                        var_name
                    ));
                }
            }
            "git_repo" => {
                let path = match value {
                    Some(Value::String(s)) if !s.is_empty() => s.clone(),
                    _ => {
                        errors.push(format!(
                            "  ✗ '{}' is required (must be a git repo path)",
                            var_name
                        ));
                        continue;
                    }
                };
                let resolved = if path == "." {
                    std::env::current_dir()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| ".".to_string())
                } else {
                    path.clone()
                };
                if !std::path::Path::new(&resolved).is_dir() {
                    errors.push(format!("  ✗ '{}' path '{}' does not exist", var_name, path));
                } else if !std::path::Path::new(&resolved).join(".git").exists()
                    && git_for_given_dir()
                        .args(["-C", &resolved, "rev-parse", "--git-dir"])
                        .output()
                        .map(|o| !o.status.success())
                        .unwrap_or(true)
                {
                    errors.push(format!(
                        "  ✗ '{}' path '{}' is not a git repository",
                        var_name, path
                    ));
                }
            }
            "path" => match value {
                Some(Value::String(s)) if !s.is_empty() => {
                    if !std::path::Path::new(s.as_str()).exists() {
                        errors.push(format!("  ✗ '{}' path '{}' does not exist", var_name, s));
                    }
                }
                _ => {
                    errors.push(format!(
                        "  ✗ '{}' is required (must be a valid path)",
                        var_name
                    ));
                }
            },
            "optional" | "" => {} // no validation
            other => {
                log::warn!(
                    "Unknown context_validation type '{}' for '{}'",
                    other,
                    var_name
                );
            }
        }
    }
    errors
}

/// Environment variables that make git use a repository other than the one
/// containing its working directory. Git sets GIT_DIR for hooks that run in a
/// worktree, as an absolute path, so a runner started from such a hook would
/// otherwise stage in, and validate against, the hook's repository.
const GIT_LOCATION_VARS: [&str; 3] = ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"];

/// A `git` command that finds its repository from its working directory (or
/// `-C`), not from GIT_DIR, GIT_WORK_TREE or GIT_INDEX_FILE in the runner's
/// environment.
fn git_for_given_dir() -> Command {
    let mut command = Command::new("git");
    for name in GIT_LOCATION_VARS {
        command.env_remove(name);
    }
    command
}

fn git_stage_all(working_dir: &str) -> Option<String> {
    log::debug!("git_stage_all: working_dir={:?}", working_dir);
    let result = git_for_given_dir()
        .args(["add", "-A"])
        .current_dir(working_dir)
        .output()
        .ok()?;

    if !result.status.success() {
        return None;
    }

    let diff = git_for_given_dir()
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

fn chrono_now() -> String {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let hours = (secs / 3600) % 24;
    let mins = (secs / 60) % 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}Z", hours, mins, s)
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
            _model: Option<&str>,
            _timeout: Option<u64>,
        ) -> Result<String, anyhow::Error> {
            if prompt.starts_with("Sub-recipe '") {
                return Ok("STATUS: COMPLETE".to_string());
            }
            if prompt == "ordinary failure" {
                anyhow::bail!("Codex exec cancelled by signal 15 (ordinary diagnostic text)");
            }
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
            _extra_env: &std::collections::HashMap<String, String>,
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

    struct EnvCaptureAdapter(std::sync::Mutex<Option<HashMap<String, String>>>);

    impl Adapter for EnvCaptureAdapter {
        fn execute_agent_step(
            &self,
            _prompt: &str,
            _agent_name: Option<&str>,
            _system_prompt: Option<&str>,
            _mode: Option<&str>,
            _working_dir: &str,
            _model: Option<&str>,
            _timeout: Option<u64>,
        ) -> Result<String, anyhow::Error> {
            anyhow::bail!("agent execution is not used by this test")
        }

        fn execute_bash_step(
            &self,
            _command: &str,
            _working_dir: &str,
            _timeout: Option<u64>,
            extra_env: &std::collections::HashMap<String, String>,
        ) -> Result<String, anyhow::Error> {
            *self.0.lock().unwrap() = Some(extra_env.clone());
            Ok("not-json".to_string())
        }

        fn is_available(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "env-capture"
        }
    }

    struct ResetEnvBudget;

    impl Drop for ResetEnvBudget {
        fn drop(&mut self) {
            crate::context::clear_env_byte_budget_for_test();
        }
    }

    #[test]
    fn required_json_failure_with_tiny_budget_passes_full_env() {
        // #130: a tiny budget no longer swaps the env for AMPLIHACK_CONTEXT_FILE;
        // the adapter receives every var and handles oversize itself.
        let _budget_lock = crate::context::ENV_BUDGET_TEST_LOCK.lock().unwrap();
        crate::context::set_env_byte_budget_for_test(1);
        let _reset = ResetEnvBudget;
        let recipe = RecipeParser::new()
            .parse(
                "name: invalid-json\nsteps:\n  - id: parse\n    type: bash\n    command: echo invalid\n    parse_json: true\n    parse_json_required: true\n    parallel_group: cleanup\n",
            )
            .unwrap();
        let context = RecipeContext::new(HashMap::from([(
            "large".to_string(),
            Value::String("x".repeat(8192)),
        )]));
        let adapter = EnvCaptureAdapter(std::sync::Mutex::new(None));

        let outcome = RecipeRunner::execute_bash_step_parallel(
            &recipe.steps[0],
            &context,
            &adapter,
            ".",
            false,
            &TerminalState::default(),
        );

        assert_eq!(outcome.result.status, StepStatus::Failed);
        assert!(outcome.result.error.contains("parse_json failed"));
        let env = adapter.0.lock().unwrap().clone().unwrap();
        assert!(!env.contains_key("AMPLIHACK_CONTEXT_FILE"));
        assert_eq!(env.get("RECIPE_VAR_large").map(String::len), Some(8192));
        assert_eq!(env.get("LARGE").map(String::len), Some(8192));
    }

    fn sha256_hex(s: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(s.as_bytes()))
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn large_step_output_reaches_the_next_bash_step_byte_for_byte() {
        // T2 (#130): a 200 KiB step output followed by a bash step that reads
        // it through RECIPE_VAR_<name> and its legacy alias. Before the fix the
        // second step died with `Argument list too long (os error 7)`.
        let _budget_lock = crate::context::ENV_BUDGET_TEST_LOCK.lock().unwrap();
        crate::context::clear_env_byte_budget_for_test();
        let unit = "abcdefghijklmnopqrstuvwxyz0123456789\n";
        let produced = unit.repeat(204_800 / unit.len() + 1)[..204_800].to_string();
        // The runner trims captured step output.
        let expected = sha256_hex(produced.trim());

        let recipe = RecipeParser::new()
            .parse(
                "name: big-output\nsteps:\n  - id: gen\n    type: bash\n    command: \"yes abcdefghijklmnopqrstuvwxyz0123456789 | head -c 204800\"\n    output: gen\n  - id: read\n    type: bash\n    command: |\n      printf %s \"$RECIPE_VAR_gen\" | sha256sum | cut -c1-64\n      printf %s \"$GEN\" | sha256sum | cut -c1-64\n",
            )
            .unwrap();
        let runner =
            RecipeRunner::new(crate::adapters::cli_subprocess::CLISubprocessAdapter::new())
                .with_auto_stage(false);
        let result = runner.execute(&recipe, None);

        assert!(result.success, "{:?}", result.step_results);
        assert_eq!(result.step_results[0].output.len(), produced.trim().len());
        assert_eq!(
            result.step_results[1].output.lines().collect::<Vec<_>>(),
            vec![expected.as_str(), expected.as_str()]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn oversized_context_value_keeps_trailing_newlines_end_to_end() {
        // #130: user-supplied context is not trimmed, so a spilled value must
        // come back with its trailing newlines intact.
        let _budget_lock = crate::context::ENV_BUDGET_TEST_LOCK.lock().unwrap();
        crate::context::clear_env_byte_budget_for_test();
        let report = format!("{}\n  tail with spaces  \n\n", "r".repeat(130 * 1024));

        let recipe = RecipeParser::new()
            .parse(
                "name: trailing\nsteps:\n  - id: read\n    type: bash\n    command: |\n      printf %s \"$RECIPE_VAR_report\" | sha256sum | cut -c1-64\n      printf %s \"$REPORT\" | sha256sum | cut -c1-64\n",
            )
            .unwrap();
        let runner =
            RecipeRunner::new(crate::adapters::cli_subprocess::CLISubprocessAdapter::new())
                .with_auto_stage(false);
        let result = runner.execute(
            &recipe,
            Some(HashMap::from([(
                "report".to_string(),
                Value::String(report.clone()),
            )])),
        );

        assert!(result.success, "{:?}", result.step_results);
        let expected = sha256_hex(&report);
        assert_eq!(
            result.step_results[0].output.lines().collect::<Vec<_>>(),
            vec![expected.as_str(), expected.as_str()]
        );
    }

    /// Records the model of every agent call; the first call of each prompt
    /// gets output that is not JSON.
    struct ModelRecordingAdapter(std::sync::Mutex<Vec<Option<String>>>);

    impl Adapter for ModelRecordingAdapter {
        fn execute_agent_step(
            &self,
            prompt: &str,
            _agent_name: Option<&str>,
            _system_prompt: Option<&str>,
            _mode: Option<&str>,
            _working_dir: &str,
            model: Option<&str>,
            _timeout: Option<u64>,
        ) -> Result<String, anyhow::Error> {
            self.0.lock().unwrap().push(model.map(str::to_string));
            if prompt == "ordinary failure" {
                anyhow::bail!("ordinary failure");
            }
            if prompt.contains("previous response was not valid JSON") {
                return Ok(r#"{"ok":true}"#.to_string());
            }
            Ok("not json".to_string())
        }

        fn execute_bash_step(
            &self,
            command: &str,
            _working_dir: &str,
            _timeout: Option<u64>,
            _extra_env: &std::collections::HashMap<String, String>,
        ) -> Result<String, anyhow::Error> {
            Ok(format!("Bash output for: {}", command))
        }

        fn is_available(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "model-recording"
        }
    }

    #[test]
    fn json_repair_call_keeps_the_step_model() {
        let recipe = RecipeParser::new()
            .parse("name: repair\nsteps:\n  - id: ask\n    type: agent\n    prompt: give json\n    model: step-model\n    parse_json: true\n")
            .unwrap();
        let runner =
            RecipeRunner::new(ModelRecordingAdapter(Default::default())).with_auto_stage(false);
        let result = runner.execute(&recipe, None);
        assert!(result.success, "{result:?}");
        let models = runner.adapter.0.lock().unwrap().clone();
        assert_eq!(
            models,
            vec![Some("step-model".to_string()); 2],
            "primary and repair calls"
        );
    }

    #[test]
    fn sub_recipe_recovery_call_keeps_the_step_model() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("child.yaml"),
            "name: child\nsteps:\n  - id: fail\n    type: agent\n    prompt: ordinary failure\n    model: child-model\n",
        )
        .unwrap();
        let recipe = RecipeParser::new()
            .parse("name: parent\nsteps:\n  - id: child\n    type: recipe\n    recipe: child\n    model: step-model\n    recovery_on_failure: true\n")
            .unwrap();
        let runner = RecipeRunner::new(ModelRecordingAdapter(Default::default()))
            .with_working_dir(root.path().to_str().unwrap())
            .with_auto_stage(false);
        runner.execute(&recipe, None);
        let models = runner.adapter.0.lock().unwrap().clone();
        assert_eq!(
            models,
            vec![
                Some("child-model".to_string()),
                Some("step-model".to_string())
            ],
            "the child's own call, then the recovery call"
        );
    }

    #[test]
    fn ordinary_nested_failure_still_runs_recovery() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("child.yaml"),
            "name: child\nsteps:\n  - id: fail\n    type: agent\n    prompt: ordinary failure\n",
        )
        .unwrap();
        let recipe = RecipeParser::new()
            .parse("name: parent\nsteps:\n  - id: child\n    type: recipe\n    recipe: child\n    recovery_on_failure: true\n  - id: after\n    type: bash\n    command: echo continued\n")
            .unwrap();
        let runner = RecipeRunner::new(MockAdapter)
            .with_working_dir(root.path().to_str().unwrap())
            .with_auto_stage(false);
        let result = runner.execute(&recipe, None);
        assert!(result.success, "{result:?}");
        assert_eq!(result.step_results.len(), 2);
        assert_eq!(result.step_results[0].output, "STATUS: COMPLETE");
    }

    #[test]
    fn ordinary_nonfatal_errors_still_continue_without_matching_diagnostic_text() {
        for policy in ["continue_on_error: true", "fatal: false"] {
            let recipe = RecipeParser::new().parse(&format!(
                "name: ordinary\nsteps:\n  - id: fail\n    type: agent\n    prompt: ordinary failure\n    {policy}\n  - id: after\n    type: bash\n    command: echo continued\n"
            )).unwrap();
            let runner = RecipeRunner::new(MockAdapter).with_auto_stage(false);
            let result = runner.execute(&recipe, None);
            assert!(result.success);
            assert_eq!(result.step_results.len(), 2);
            assert_eq!(result.step_results[1].status, StepStatus::Completed);
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

    /// Counts every call that would start a process.
    struct CallCountingAdapter(std::sync::atomic::AtomicUsize);

    impl Adapter for CallCountingAdapter {
        fn execute_agent_step(
            &self,
            _prompt: &str,
            _agent_name: Option<&str>,
            _system_prompt: Option<&str>,
            _mode: Option<&str>,
            _working_dir: &str,
            _model: Option<&str>,
            _timeout: Option<u64>,
        ) -> Result<String, anyhow::Error> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("agent".to_string())
        }

        fn execute_bash_step(
            &self,
            _command: &str,
            _working_dir: &str,
            _timeout: Option<u64>,
            _extra_env: &std::collections::HashMap<String, String>,
        ) -> Result<String, anyhow::Error> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("bash".to_string())
        }

        fn is_available(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "call-counting"
        }
    }

    fn staged_files(repo: &std::path::Path) -> String {
        let out = git_for_given_dir()
            .args(["diff", "--cached", "--name-only"])
            .current_dir(repo)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn repo_with_untracked_file() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let init = git_for_given_dir()
            .args(["init", "-q"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        assert!(init.success(), "git init");
        std::fs::write(repo.path().join("work.txt"), "agent output\n").unwrap();
        repo
    }

    #[test]
    fn no_auto_stage_wins_over_a_step_that_asks_for_staging() {
        let yaml = "name: stage\nsteps:\n  - id: work\n    type: agent\n    prompt: do work\n    auto_stage: true\n";
        let recipe = RecipeParser::new().parse(yaml).unwrap();
        for (runner_stages, expected) in [(false, ""), (true, "work.txt")] {
            let repo = repo_with_untracked_file();
            let result = RecipeRunner::new(MockAdapter)
                .with_working_dir(repo.path().to_str().unwrap())
                .with_auto_stage(runner_stages)
                .execute(&recipe, None);
            assert!(result.success, "{result:?}");
            assert_eq!(
                staged_files(repo.path()),
                expected,
                "runner auto_stage {runner_stages}"
            );
        }
    }

    /// Set only in the child process that
    /// `runner_git_ignores_git_location_variables` starts.
    const GIT_LOCATION_CHILD: &str = "RECIPE_RUNNER_GIT_LOCATION_CHILD";

    /// Runs only in that child, where GIT_DIR, GIT_WORK_TREE and GIT_INDEX_FILE
    /// name another repository: staging and the git_repo check use the
    /// directory the runner was given.
    #[test]
    fn git_location_child_checks() {
        if std::env::var_os(GIT_LOCATION_CHILD).is_none() {
            return;
        }
        let yaml = "name: stage\nsteps:\n  - id: work\n    type: agent\n    prompt: do work\n";
        let recipe = RecipeParser::new().parse(yaml).unwrap();
        // In a repository, the step's file is staged in that repository.
        let repo = repo_with_untracked_file();
        let result = RecipeRunner::new(MockAdapter)
            .with_working_dir(repo.path().to_str().unwrap())
            .execute(&recipe, None);
        assert!(result.success, "{result:?}");
        assert_eq!(staged_files(repo.path()), "work.txt");
        // In a directory outside any repository, nothing is staged.
        let plain = tempfile::tempdir().unwrap();
        std::fs::write(plain.path().join("work.txt"), "agent output\n").unwrap();
        let result = RecipeRunner::new(MockAdapter)
            .with_working_dir(plain.path().to_str().unwrap())
            .execute(&recipe, None);
        assert!(result.success, "{result:?}");
        // The git_repo check finds no repository for that directory.
        let rules = HashMap::from([("repo".to_string(), "git_repo".to_string())]);
        let ctx = HashMap::from([(
            "repo".to_string(),
            Value::String(plain.path().to_str().unwrap().to_string()),
        )]);
        let errors = validate_context(&rules, &ctx);
        assert!(
            errors.len() == 1 && errors[0].contains("is not a git repository"),
            "{errors:?}"
        );
    }

    /// Git sets GIT_DIR for hooks that run in a worktree. Runs the staging
    /// test and the child checks with GIT_DIR, GIT_WORK_TREE and
    /// GIT_INDEX_FILE naming a worktree of another repository, and requires
    /// that repository to be unchanged.
    #[test]
    fn runner_git_ignores_git_location_variables() {
        let root = tempfile::tempdir().unwrap();
        let enclosing = root.path().join("enclosing");
        let worktree = root.path().join("enclosing-wt");
        let tmp = root.path().join("tmp");
        std::fs::create_dir(&tmp).unwrap();
        let git = |args: &[&str]| {
            let out = git_for_given_dir()
                .args([
                    "-c",
                    "user.name=test",
                    "-c",
                    "user.email=test@example.invalid",
                ])
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            out.stdout
        };
        let e = enclosing.to_str().unwrap();
        git(&["init", "-q", e]);
        std::fs::write(enclosing.join("keep.txt"), "keep\n").unwrap();
        git(&["-C", e, "add", "keep.txt"]);
        git(&["-C", e, "commit", "-q", "-m", "base"]);
        git(&[
            "-C",
            e,
            "worktree",
            "add",
            "-q",
            "-b",
            "wt",
            worktree.to_str().unwrap(),
        ]);
        let gitdir = enclosing.join(".git");
        let wt_gitdir = gitdir.join("worktrees/enclosing-wt");
        let snapshot = || {
            let files: Vec<Vec<u8>> = [
                gitdir.join("config"),
                gitdir.join("HEAD"),
                gitdir.join("index"),
                wt_gitdir.join("HEAD"),
                wt_gitdir.join("index"),
            ]
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect();
            (files, git(&["-C", e, "for-each-ref"]))
        };
        let before = snapshot();
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runner::tests::no_auto_stage_wins_over_a_step_that_asks_for_staging",
                "runner::tests::git_location_child_checks",
                "--test-threads=1",
            ])
            .env("GIT_DIR", &wt_gitdir)
            .env("GIT_WORK_TREE", &worktree)
            .env("GIT_INDEX_FILE", wt_gitdir.join("index"))
            .env("GIT_CEILING_DIRECTORIES", root.path())
            .env("TMPDIR", &tmp)
            .env(GIT_LOCATION_CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success()
                && stdout.contains("running 2 tests")
                && stdout.contains("2 passed"),
            "child: {stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            snapshot() == before,
            "the repository named by GIT_DIR changed"
        );
    }

    #[test]
    fn required_audit_log_that_cannot_be_opened_runs_no_step() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the audit directory should be: it cannot be created.
        let not_a_dir = dir.path().join("audit");
        std::fs::write(&not_a_dir, "").unwrap();
        let recipe = RecipeParser::new()
            .parse("name: audited\nsteps:\n  - id: one\n    command: echo one\n")
            .unwrap();
        for (required, expect_success, expect_calls) in [(true, false, 0), (false, true, 1)] {
            let runner = RecipeRunner::new(CallCountingAdapter(Default::default()))
                .with_auto_stage(false)
                .with_audit_dir(not_a_dir.clone())
                .with_require_audit(required);
            let result = runner.execute(&recipe, None);
            assert_eq!(
                result.success, expect_success,
                "required {required}: {result:?}"
            );
            assert_eq!(
                runner.adapter.0.load(std::sync::atomic::Ordering::SeqCst),
                expect_calls,
                "required {required}"
            );
            if required {
                assert_eq!(result.step_results[0].step_id, "audit-log");
            }
        }
    }

    #[test]
    fn test_dry_run_starts_no_hook_or_step() {
        let yaml = r#"
name: "dry-run-hooks"
hooks:
  pre_step: "echo pre"
  post_step: "echo post"
  on_error: "echo err"
steps:
  - id: "serial"
    command: "echo serial"
  - id: "left"
    command: "echo left"
    parallel_group: "pair"
  - id: "right"
    prompt: "say right"
    parallel_group: "pair"
"#;
        let recipe = RecipeParser::new().parse(yaml).unwrap();
        let runner = RecipeRunner::new(CallCountingAdapter(Default::default())).with_dry_run(true);
        let result = runner.execute(&recipe, None);
        assert!(result.success, "dry run should succeed: {:?}", result);
        assert_eq!(result.step_results.len(), 3);
        assert_eq!(
            runner.adapter.0.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a dry run must not reach the adapter for any hook or step"
        );
    }

    /// A mock adapter that returns output with trailing newlines, simulating
    /// real shell behavior where commands like `echo 1` produce "1\n".
    struct TrailingNewlineAdapter;

    impl Adapter for TrailingNewlineAdapter {
        fn execute_agent_step(
            &self,
            _prompt: &str,
            _agent_name: Option<&str>,
            _system_prompt: Option<&str>,
            _mode: Option<&str>,
            _working_dir: &str,
            _model: Option<&str>,
            _timeout: Option<u64>,
        ) -> Result<String, anyhow::Error> {
            Ok("agent output\n".to_string())
        }

        fn execute_bash_step(
            &self,
            _command: &str,
            _working_dir: &str,
            _timeout: Option<u64>,
            _extra_env: &std::collections::HashMap<String, String>,
        ) -> Result<String, anyhow::Error> {
            // Simulate `echo 1` which produces "1\n"
            Ok("1\n".to_string())
        }

        fn is_available(&self) -> bool {
            true
        }
        fn name(&self) -> &str {
            "trailing-newline-mock"
        }
    }

    /// Bash step output should have trailing whitespace stripped so that
    /// condition comparisons like `count != '1'` work correctly.
    /// Regression test for amplihack#3058.
    #[test]
    fn test_bash_output_trailing_whitespace_stripped() {
        let yaml = r#"
name: "test-trim"
steps:
  - id: "count"
    command: "echo 1"
    output: "workstream_count"
  - id: "check"
    command: "echo matched"
    condition: "workstream_count == '1'"
    output: "check_result"
"#;
        let parser = RecipeParser::new();
        let recipe = parser.parse(yaml).unwrap();
        let runner = RecipeRunner::new(TrailingNewlineAdapter);
        let result = runner.execute(&recipe, None);
        assert!(result.success);
        // The count step should store trimmed "1" — serde_json parses it as Number(1)
        let count_val = result.context.get("workstream_count").unwrap();
        assert!(
            count_val.is_number() || count_val == &Value::String("1".to_string()),
            "bash output should be trimmed (no trailing newline); got {:?}",
            count_val
        );
        // The condition step should NOT be skipped — values_equal coerces
        // Number(1) == String("1") via cross-type comparison.
        assert_eq!(
            result.step_results[1].status,
            StepStatus::Completed,
            "condition `workstream_count == '1'` should match after trimming"
        );
    }

    /// Adapter that fails after the timeout value it receives on agent steps,
    /// simulating a timeout error from the real adapter.
    struct TimeoutFailAdapter;
    impl Adapter for TimeoutFailAdapter {
        fn execute_agent_step(
            &self,
            _prompt: &str,
            _agent_name: Option<&str>,
            _system_prompt: Option<&str>,
            _mode: Option<&str>,
            _working_dir: &str,
            _model: Option<&str>,
            timeout: Option<u64>,
        ) -> Result<String, anyhow::Error> {
            if let Some(secs) = timeout {
                anyhow::bail!("Agent step timed out after {}s", secs);
            }
            Ok("ok".to_string())
        }
        fn execute_bash_step(
            &self,
            command: &str,
            _working_dir: &str,
            _timeout: Option<u64>,
            _extra_env: &std::collections::HashMap<String, String>,
        ) -> Result<String, anyhow::Error> {
            Ok(format!("Bash output for: {}", command))
        }
        fn is_available(&self) -> bool {
            true
        }
        fn name(&self) -> &str {
            "timeout-fail-mock"
        }
    }

    /// Verify that the timeout field from a recipe agent step propagates to
    /// the adapter and that a timeout failure is reported correctly.
    #[test]
    fn test_agent_step_timeout_propagated_and_reported() {
        let yaml = r#"
name: "test-agent-timeout"
steps:
  - id: "timed-agent"
    prompt: "do something"
    timeout: 300
"#;
        let parser = RecipeParser::new();
        let recipe = parser.parse(yaml).unwrap();
        let runner = RecipeRunner::new(TimeoutFailAdapter);
        let result = runner.execute(&recipe, None);
        assert!(
            !result.success,
            "adapter timeout error should cause step failure"
        );
        assert_eq!(result.step_results[0].status, StepStatus::Failed);
        assert!(
            result.step_results[0].error.contains("timed out"),
            "error should mention timeout: {}",
            result.step_results[0].error
        );
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
