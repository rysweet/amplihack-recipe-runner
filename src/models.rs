/// Data models for the Recipe Runner.
///
/// Defines the core data structures: steps, recipes, results, and error types.
/// Direct port from Python `amplihack.recipes.models`.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepType {
    Bash,
    Agent,
    Recipe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Pending,
    Running,
    Completed,
    Skipped,
    Failed,
}

impl fmt::Display for StepStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            StepStatus::Pending => "pending",
            StepStatus::Running => "running",
            StepStatus::Completed => "completed",
            StepStatus::Skipped => "skipped",
            StepStatus::Failed => "failed",
        };
        write!(f, "{}", s)
    }
}

/// A single step in a recipe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    #[serde(rename = "type", default)]
    pub step_type: Option<StepType>,
    pub command: Option<String>,
    pub agent: Option<String>,
    pub prompt: Option<String>,
    pub output: Option<String>,
    pub condition: Option<String>,
    #[serde(default)]
    pub parse_json: bool,
    pub mode: Option<String>,
    pub working_dir: Option<String>,
    pub timeout: Option<u64>,
    pub auto_stage: Option<bool>,
    pub recipe: Option<String>,
    #[serde(rename = "context")]
    pub sub_context: Option<HashMap<String, serde_json::Value>>,
    /// If true, step failure logs a warning but does not abort the recipe.
    #[serde(default)]
    pub continue_on_error: bool,
    /// Steps sharing the same parallel_group execute concurrently.
    pub parallel_group: Option<String>,
    /// Tags for conditional step filtering via --include-tags / --exclude-tags.
    #[serde(default)]
    pub when_tags: Vec<String>,
}

impl Step {
    /// Infer the effective step type from explicit field or presence of other fields.
    pub fn effective_type(&self) -> StepType {
        if let Some(t) = self.step_type {
            return t;
        }
        if self.recipe.is_some() {
            return StepType::Recipe;
        }
        if self.agent.is_some() {
            return StepType::Agent;
        }
        if self.prompt.is_some() && self.command.is_none() {
            return StepType::Agent;
        }
        StepType::Bash
    }
}

/// Per-recipe recursion limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecursionConfig {
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    #[serde(default = "default_max_total_steps")]
    pub max_total_steps: u32,
}

impl Default for RecursionConfig {
    fn default() -> Self {
        Self {
            max_depth: default_max_depth(),
            max_total_steps: default_max_total_steps(),
        }
    }
}

fn default_max_depth() -> u32 {
    6
}
fn default_max_total_steps() -> u32 {
    200
}

/// Pre/post step hook commands.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecipeHooks {
    /// Command to run before each step.
    pub pre_step: Option<String>,
    /// Command to run after each step.
    pub post_step: Option<String>,
    /// Command to run on step error.
    pub on_error: Option<String>,
}

/// A parsed recipe definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub context: HashMap<String, serde_json::Value>,
    /// Per-recipe recursion limits (max_depth, max_total_steps).
    #[serde(default)]
    pub recursion: RecursionConfig,
    /// Pre/post step hook commands.
    #[serde(default)]
    pub hooks: RecipeHooks,
    /// Inherit steps from another recipe, optionally overriding individual steps.
    pub extends: Option<String>,
}

fn default_version() -> String {
    "1.0.0".to_string()
}

/// Result of executing a single step.
#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    pub step_id: String,
    pub status: StepStatus,
    pub output: String,
    pub error: String,
    /// Wall-clock duration of this step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<Duration>,
}

impl fmt::Display for StepResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:>9}] {}", self.status, self.step_id)?;
        if let Some(d) = self.duration {
            write!(f, " ({:.1}s)", d.as_secs_f64())?;
        }
        if !self.error.is_empty() {
            write!(f, " -- error: {}", self.error)?;
        }
        Ok(())
    }
}

/// Result of executing an entire recipe.
#[derive(Debug, Clone, Serialize)]
pub struct RecipeResult {
    pub recipe_name: String,
    pub success: bool,
    pub step_results: Vec<StepResult>,
    #[serde(skip)]
    pub context: HashMap<String, serde_json::Value>,
    /// Total wall-clock duration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<Duration>,
}

impl fmt::Display for RecipeResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = if self.success { "SUCCESS" } else { "FAILED" };
        write!(f, "Recipe '{}': {}", self.recipe_name, status)?;
        if let Some(d) = self.duration {
            write!(f, " ({:.1}s)", d.as_secs_f64())?;
        }
        writeln!(f)?;
        for sr in &self.step_results {
            writeln!(f, "  {}", sr)?;
        }
        Ok(())
    }
}

/// Error raised when a step fails to execute.
#[derive(Debug, thiserror::Error)]
#[error("Step '{step_id}' failed: {message}")]
pub struct StepExecutionError {
    pub step_id: String,
    pub message: String,
}
