/// Data models for the Recipe Runner.
///
/// Defines the core data structures: steps, recipes, results, and error types.
/// Direct port from Python `amplihack.recipes.models`.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

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
}

fn default_version() -> String {
    "1.0.0".to_string()
}

/// Result of executing a single step.
#[derive(Debug, Clone)]
pub struct StepResult {
    pub step_id: String,
    pub status: StepStatus,
    pub output: String,
    pub error: String,
}

impl fmt::Display for StepResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{:>9}] {}", self.status, self.step_id)?;
        if !self.error.is_empty() {
            write!(f, " -- error: {}", self.error)?;
        }
        Ok(())
    }
}

/// Result of executing an entire recipe.
#[derive(Debug, Clone)]
pub struct RecipeResult {
    pub recipe_name: String,
    pub success: bool,
    pub step_results: Vec<StepResult>,
    pub context: HashMap<String, serde_json::Value>,
}

impl fmt::Display for RecipeResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = if self.success { "SUCCESS" } else { "FAILED" };
        writeln!(f, "Recipe '{}': {}", self.recipe_name, status)?;
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
