/// Data models for the Recipe Runner.
///
/// Defines the core data structures: steps, recipes, results, and error types.
///
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
    Degraded,
}

impl fmt::Display for StepStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            StepStatus::Pending => "pending",
            StepStatus::Running => "running",
            StepStatus::Completed => "completed",
            StepStatus::Skipped => "skipped",
            StepStatus::Failed => "failed",
            StepStatus::Degraded => "degraded",
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
    /// If true, parse_json failure stops the recipe. If false (default),
    /// parse_json failure stores raw output and marks step as DEGRADED.
    #[serde(default)]
    pub parse_json_required: bool,
    pub mode: Option<String>,
    pub working_dir: Option<String>,
    /// Timeout in seconds. Only applies to bash steps; agent steps run without
    /// a timeout and complete when the underlying CLI process exits.
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
    /// If true, attempt agentic recovery when a sub-recipe fails before
    /// reporting the step as failed.
    #[serde(default)]
    pub recovery_on_failure: bool,
    /// Model override for agent steps (e.g., "haiku", "sonnet").
    pub model: Option<String>,
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

/// Default maximum recursion depth for sub-recipe execution.
pub const DEFAULT_MAX_DEPTH: u32 = 6;

fn default_max_depth() -> u32 {
    DEFAULT_MAX_DEPTH
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

/// Serialize an `Option<Duration>` as an f64 number of seconds.
fn serialize_duration_secs<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match duration {
        Some(d) => serializer.serialize_f64(d.as_secs_f64()),
        None => serializer.serialize_none(),
    }
}

/// Result of executing a single step.
#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    pub step_id: String,
    pub status: StepStatus,
    pub output: String,
    pub error: String,
    /// Wall-clock duration of this step.
    #[serde(
        serialize_with = "serialize_duration_secs",
        skip_serializing_if = "Option::is_none"
    )]
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
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub context: HashMap<String, serde_json::Value>,
    /// Total wall-clock duration.
    #[serde(
        serialize_with = "serialize_duration_secs",
        skip_serializing_if = "Option::is_none"
    )]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effective_type_explicit_bash() {
        let step = Step {
            id: "s1".into(),
            step_type: Some(StepType::Bash),
            command: None,
            agent: Some("my-agent".into()),
            prompt: None,
            output: None,
            condition: None,
            parse_json: false,
            parse_json_required: false,
            mode: None,
            working_dir: None,
            timeout: None,
            auto_stage: None,
            recipe: None,
            sub_context: None,
            continue_on_error: false,
            parallel_group: None,
            when_tags: vec![],
            recovery_on_failure: false,
            model: None,
        };
        // Explicit type overrides all field-based inference
        assert_eq!(step.effective_type(), StepType::Bash);
    }

    #[test]
    fn test_effective_type_infers_recipe() {
        let step = Step {
            id: "s1".into(),
            step_type: None,
            command: None,
            agent: None,
            prompt: None,
            output: None,
            condition: None,
            parse_json: false,
            parse_json_required: false,
            mode: None,
            working_dir: None,
            timeout: None,
            auto_stage: None,
            recipe: Some("sub-recipe".into()),
            sub_context: None,
            continue_on_error: false,
            parallel_group: None,
            when_tags: vec![],
            recovery_on_failure: false,
            model: None,
        };
        assert_eq!(step.effective_type(), StepType::Recipe);
    }

    #[test]
    fn test_effective_type_infers_agent_from_agent_field() {
        let step = Step {
            id: "s1".into(),
            step_type: None,
            command: None,
            agent: Some("my-agent".into()),
            prompt: None,
            output: None,
            condition: None,
            parse_json: false,
            parse_json_required: false,
            mode: None,
            working_dir: None,
            timeout: None,
            auto_stage: None,
            recipe: None,
            sub_context: None,
            continue_on_error: false,
            parallel_group: None,
            when_tags: vec![],
            recovery_on_failure: false,
            model: None,
        };
        assert_eq!(step.effective_type(), StepType::Agent);
    }

    #[test]
    fn test_effective_type_infers_agent_from_prompt_only() {
        let step = Step {
            id: "s1".into(),
            step_type: None,
            command: None,
            agent: None,
            prompt: Some("do something".into()),
            output: None,
            condition: None,
            parse_json: false,
            parse_json_required: false,
            mode: None,
            working_dir: None,
            timeout: None,
            auto_stage: None,
            recipe: None,
            sub_context: None,
            continue_on_error: false,
            parallel_group: None,
            when_tags: vec![],
            recovery_on_failure: false,
            model: None,
        };
        assert_eq!(step.effective_type(), StepType::Agent);
    }

    #[test]
    fn test_effective_type_infers_bash_with_command_and_prompt() {
        let step = Step {
            id: "s1".into(),
            step_type: None,
            command: Some("echo hello".into()),
            agent: None,
            prompt: Some("prompt too".into()),
            output: None,
            condition: None,
            parse_json: false,
            parse_json_required: false,
            mode: None,
            working_dir: None,
            timeout: None,
            auto_stage: None,
            recipe: None,
            sub_context: None,
            continue_on_error: false,
            parallel_group: None,
            when_tags: vec![],
            recovery_on_failure: false,
            model: None,
        };
        // command + prompt → Bash (prompt alone would be Agent, but command presence wins)
        assert_eq!(step.effective_type(), StepType::Bash);
    }

    #[test]
    fn test_effective_type_defaults_to_bash() {
        let step = Step {
            id: "s1".into(),
            step_type: None,
            command: None,
            agent: None,
            prompt: None,
            output: None,
            condition: None,
            parse_json: false,
            parse_json_required: false,
            mode: None,
            working_dir: None,
            timeout: None,
            auto_stage: None,
            recipe: None,
            sub_context: None,
            continue_on_error: false,
            parallel_group: None,
            when_tags: vec![],
            recovery_on_failure: false,
            model: None,
        };
        assert_eq!(step.effective_type(), StepType::Bash);
    }

    #[test]
    fn test_recursion_config_defaults() {
        let config = RecursionConfig::default();
        assert_eq!(config.max_depth, 6);
        assert_eq!(config.max_total_steps, 200);
    }

    #[test]
    fn test_step_status_display() {
        assert_eq!(format!("{}", StepStatus::Pending), "pending");
        assert_eq!(format!("{}", StepStatus::Running), "running");
        assert_eq!(format!("{}", StepStatus::Completed), "completed");
        assert_eq!(format!("{}", StepStatus::Skipped), "skipped");
        assert_eq!(format!("{}", StepStatus::Failed), "failed");
        assert_eq!(format!("{}", StepStatus::Degraded), "degraded");
    }

    #[test]
    fn test_step_result_display() {
        let result = StepResult {
            step_id: "test-step".into(),
            status: StepStatus::Completed,
            output: "some output".into(),
            error: String::new(),
            duration: Some(Duration::from_secs_f64(1.5)),
        };
        let display = format!("{}", result);
        assert!(display.contains("completed"));
        assert!(display.contains("test-step"));
        assert!(display.contains("1.5s"));
        assert!(!display.contains("error"));
    }

    #[test]
    fn test_step_result_display_with_error() {
        let result = StepResult {
            step_id: "fail-step".into(),
            status: StepStatus::Failed,
            output: String::new(),
            error: "something broke".into(),
            duration: None,
        };
        let display = format!("{}", result);
        assert!(display.contains("failed"));
        assert!(display.contains("something broke"));
    }

    #[test]
    fn test_recipe_result_display() {
        let result = RecipeResult {
            recipe_name: "test-recipe".into(),
            success: true,
            step_results: vec![],
            context: HashMap::new(),
            duration: Some(Duration::from_secs(2)),
        };
        let display = format!("{}", result);
        assert!(display.contains("test-recipe"));
        assert!(display.contains("SUCCESS"));
    }

    #[test]
    fn test_recipe_result_display_failure() {
        let result = RecipeResult {
            recipe_name: "bad-recipe".into(),
            success: false,
            step_results: vec![StepResult {
                step_id: "s1".into(),
                status: StepStatus::Failed,
                output: String::new(),
                error: "boom".into(),
                duration: None,
            }],
            context: HashMap::new(),
            duration: None,
        };
        let display = format!("{}", result);
        assert!(display.contains("bad-recipe"));
        assert!(display.contains("FAILED"));
        assert!(display.contains("boom"));
    }

    #[test]
    fn test_step_execution_error_display() {
        let err = StepExecutionError {
            step_id: "broken".into(),
            message: "timed out".into(),
        };
        let display = format!("{}", err);
        assert!(display.contains("broken"));
        assert!(display.contains("timed out"));
    }
}
