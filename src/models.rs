use log::trace;
/// Data models for the Recipe Runner.
///
/// Defines the core data structures: steps, recipes, results, and error types.
///
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

/// Escape a raw identifier for printable diagnostics without changing the identifier.
///
/// Uses the same visible control-character escapes as Rust debug strings, without
/// enclosing quotes. Apply only to identifiers at display boundaries, not to
/// assembled messages or structured fields. This function performs no I/O.
pub fn diagnostic_identifier(raw: &str) -> String {
    raw.escape_debug().collect()
}

/// Longest rendered `model:` or `effort:` accepted.
///
/// Both became template sinks with #219, and a template is fed from the recipe
/// context — which carries earlier step output, agent output, parsed JSON and
/// `--context-file` values. The model becomes an argv token, which the
/// environment budget does not measure at all; the effort becomes an
/// `AMPLIHACK_`-prefixed environment value, which that budget protects from
/// eviction rather than drops. So neither is bounded anywhere else: without
/// this cap an oversized value surfaces either as an opaque `E2BIG` from
/// `execve` or as a budget error that stops the run. This is the one place
/// both sinks are bounded.
///
/// The number is chosen to clear the longest identifier any provider
/// publishes — a Bedrock inference-profile ARN runs to about 110 bytes — with
/// room to spare, while staying far below both the kernel's per-string limit
/// and the environment budget's floor. It bounds a sink; it does not express
/// an opinion about what a model may be called.
const MAX_LAUNCH_VALUE_BYTES: usize = 256;

/// A character a display escape would hide: a control character such as NUL or
/// ESC, or a format character such as the bidi override `U+202E`.
///
/// `char::escape_debug` escapes exactly this class, and it is the escape the
/// crate's own diagnostics use. A rendered `model` or `effort`, though, also
/// reaches the child process, the audit JSONL and the JSON step records
/// unescaped, so for these two values the shape is rejected rather than
/// escaped on the way out. The three characters `escape_debug` escapes for
/// quoting reasons are not of this class and are excluded.
fn hides_in_display(c: char) -> bool {
    c.escape_debug().next() == Some('\\') && !matches!(c, '\\' | '\'' | '"')
}

/// Which of the two launch fields' rules [`check_launch_value`] applies.
///
/// The two fields do not carry the same rules — `model` becomes an argv token
/// and so must not look like an option, while `effort` becomes an environment
/// value — and the selector is an enum rather than a field name so that a
/// rename or a typo cannot silently drop `model`'s extra rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchField {
    Model,
    Effort,
}

impl LaunchField {
    /// The field's spelling in a recipe, as it appears in a message.
    fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Effort => "effort",
        }
    }
}

/// Check an already-trimmed launch value against the shape rules for `field`.
///
/// The one place the rules live, so the parse-time pass over a literal and the
/// run-time pass over a rendered value cannot disagree about a value they both
/// check. It holds no model vocabulary: amplihack-rs resolves tier words such
/// as `fast` in its own launcher, and every provider spells its identifiers
/// differently, so what is rejected here is a shape that could not reach the
/// launcher intact, never a character some provider happens not to use.
///
/// The returned reason begins with the quoted field name, so a caller can
/// prefix it — the runner says `rendered 'model' …`, the parser prefixes the
/// step id. It never contains the value, which can carry a secret or a
/// terminal escape sequence into stderr, a step record and a CI log. This
/// function performs no I/O and no logging.
pub(crate) fn check_launch_value(field: LaunchField, value: &str) -> Result<(), String> {
    let name = field.name();

    // First, so the cost of checking a hostile value is bounded by the cap
    // rather than by the length of the input.
    if value.len() > MAX_LAUNCH_VALUE_BYTES {
        return Err(format!(
            "'{name}' is {} bytes; the maximum is {MAX_LAUNCH_VALUE_BYTES}",
            value.len()
        ));
    }

    // The highest-severity rule of the set: `--add-dir` or
    // `--dangerously-skip-permissions` would be read as an option and alter
    // the child's privileges rather than its model.
    if field == LaunchField::Model && value.starts_with('-') {
        return Err(format!(
            "'{name}' starts with '-', which the launcher would read as an option rather than a \
             model name"
        ));
    }

    // Covers NUL, which cannot be placed in an environment at all; ESC, which
    // could otherwise carry a terminal escape sequence; and the format
    // characters, of which a bidi override such as `U+202E` reverses the
    // display of the audit JSONL and the JSON step records — the two surfaces
    // that write these values unescaped.
    if value.chars().any(hides_in_display) {
        return Err(format!("'{name}' contains a control or format character"));
    }

    // Not an argv-splitting hazard: the runner passes the model as a single
    // argv entry and performs no shell quoting, so a space survives inside it
    // exactly as `$` or `;` would. The rule is conservatism about anything
    // downstream that re-parses the value, together with the observation that
    // no published identifier contains a space, so a value that has acquired
    // one is more likely a mis-rendered template than a model name.
    if field == LaunchField::Model && value.chars().any(char::is_whitespace) {
        return Err(format!(
            "'{name}' contains whitespace, so it would not reach the launcher as a single word"
        ));
    }

    Ok(())
}

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
        trace!("StepStatus::fmt: formatting {:?}", self);
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

fn default_fatal() -> bool {
    true
}

/// A single step in a recipe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    #[serde(default)]
    pub recovery: Option<crate::recovery::model::StepPolicy>,
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
    /// `fatal: false` in YAML is an alias for this field.
    #[serde(default, alias = "nonfatal")]
    pub continue_on_error: bool,
    /// Convenience alias: `fatal: false` → `continue_on_error: true`.
    /// When both are specified, `continue_on_error` takes precedence.
    #[serde(default = "default_fatal")]
    pub fatal: bool,
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
    ///
    /// Rendered against the recipe context before launch, so a recipe can
    /// choose a tier from a context variable or an earlier step's output. The
    /// runner holds no model vocabulary: the rendered value travels to the
    /// launcher byte for byte, as a single `--model` argument, with no shell
    /// quoting anywhere on the way. It is never rewritten — the only thing
    /// the runner does besides render and trim it is apply
    /// [`check_launch_value`], which fails the step rather than altering the
    /// value. A value written out in full is checked by the parser instead,
    /// against the same rules.
    pub model: Option<String>,
    /// How much context the step is started with. The only accepted value is
    /// `minimal`, and only on an agent step; a step asks for the normal start
    /// by omitting the field.
    ///
    /// Carried as written so that the validation pass — which has the step in
    /// hand — can name the step that owns an unrecognised value. Use
    /// [`StepContextMode::parse`] to act on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_mode: Option<String>,
    /// Reasoning-effort hint for the harness, rendered like `model`.
    ///
    /// Free-form: the accepted words differ per harness and the runner never
    /// interprets them. Accepted wherever `model` is, with no step-type
    /// restriction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// How much context an agent step is started with.
///
/// There is exactly one variant. A second spelling meaning "the normal start"
/// would be a second thing to keep consistent with the absent field, so the
/// normal start is expressed by omitting `context_mode` entirely.
///
/// The typed form exists for the code that acts on the declaration; the recipe
/// field itself is a plain string, typed at the parser boundary so that a bad
/// value is reported against the step that wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepContextMode {
    /// Start the step with only its rendered prompt and declared inputs.
    Minimal,
}

impl StepContextMode {
    /// The one accepted spelling, as written in a recipe and as placed on the
    /// child environment.
    pub const MINIMAL: &'static str = "minimal";

    /// Parse a declared value, returning `None` for anything unrecognised.
    ///
    /// Matching is exact: `Minimal` and ` minimal` are not this value.
    pub fn parse(value: &str) -> Option<Self> {
        trace!("StepContextMode::parse: value={:?}", value);
        match value {
            Self::MINIMAL => Some(Self::Minimal),
            _ => None,
        }
    }

    /// The declared spelling of this mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => Self::MINIMAL,
        }
    }
}

impl Step {
    /// Whether this step should continue on failure.
    /// `continue_on_error: true` OR `fatal: false` makes a step non-fatal.
    pub fn is_nonfatal(&self) -> bool {
        self.continue_on_error || !self.fatal
    }

    /// Infer the effective step type from explicit field or presence of other fields.
    pub fn effective_type(&self) -> StepType {
        trace!(
            "Step::effective_type: inferring type for step '{}'",
            diagnostic_identifier(&self.id)
        );
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
        trace!(
            "RecursionConfig::default: using defaults max_depth={}, max_total_steps=200",
            DEFAULT_MAX_DEPTH
        );
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
    #[serde(default)]
    pub recovery: Option<crate::recovery::model::RecipePolicy>,
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
    /// Optional validation rules for context variables.
    /// Keys match context var names. Values specify the validation type.
    /// Supported types: "nonempty", "git_repo", "path", "optional" (default).
    /// Example:
    /// ```yaml
    /// context_validation:
    ///   task_description: "nonempty"
    ///   repo_path: "git_repo"
    /// ```
    #[serde(default)]
    pub context_validation: HashMap<String, String>,
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
    trace!("serialize_duration_secs: duration={:?}", duration);
    match duration {
        Some(d) => serializer.serialize_f64(d.as_secs_f64()),
        None => serializer.serialize_none(),
    }
}

fn deserialize_duration_secs<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Duration>, D::Error> {
    let value = Option::<f64>::deserialize(d)?;
    value
        .map(|v| Duration::try_from_secs_f64(v).map_err(serde::de::Error::custom))
        .transpose()
}

/// Result of executing a single step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step_id: String,
    pub status: StepStatus,
    pub output: String,
    pub error: String,
    /// Wall-clock duration of this step.
    #[serde(
        serialize_with = "serialize_duration_secs",
        deserialize_with = "deserialize_duration_secs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub duration: Option<Duration>,
    /// The context mode this step was launched with, when it declared one.
    ///
    /// Absent — and omitted from the record — for a step that asked for the
    /// normal start and for a step that never launched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_mode: Option<String>,
    /// The model this step was launched with, after rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The effort this step was launched with, after rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl Default for StepResult {
    /// A record for a step that has not run.
    ///
    /// `StepStatus` has no default of its own on purpose — a status is always
    /// a statement about a step — so this names `Pending` explicitly.
    fn default() -> Self {
        Self {
            step_id: String::new(),
            status: StepStatus::Pending,
            output: String::new(),
            error: String::new(),
            duration: None,
            context_mode: None,
            model: None,
            effort: None,
        }
    }
}

impl fmt::Display for StepResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        trace!(
            "StepResult::fmt: formatting step_id={:?}, status={:?}",
            self.step_id, self.status
        );
        write!(
            f,
            "[{:>9}] {}",
            self.status,
            diagnostic_identifier(&self.step_id)
        )?;
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
        trace!(
            "RecipeResult::fmt: formatting recipe='{}', success={}",
            diagnostic_identifier(&self.recipe_name),
            self.success
        );
        let status = if self.success { "SUCCESS" } else { "FAILED" };
        write!(
            f,
            "Recipe '{}': {}",
            diagnostic_identifier(&self.recipe_name),
            status
        )?;
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
#[error("Step '{}' failed: {message}", diagnostic_identifier(.step_id))]
pub struct StepExecutionError {
    pub step_id: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_displays_escape_identifiers_and_preserve_structured_values() {
        let raw = "identifier\n\r\u{1b}[31m\"\\tail";
        let escaped = "identifier\\n\\r\\u{1b}[31m\\\"\\\\tail";
        assert_eq!(diagnostic_identifier(raw), escaped);
        assert_eq!(diagnostic_identifier("ordinary-step_1"), "ordinary-step_1");
        assert_eq!(diagnostic_identifier(""), "");
        let error = StepExecutionError {
            step_id: raw.into(),
            message: "already escaped \\n with a real\nmessage line".into(),
        };
        assert_eq!(
            error.to_string(),
            format!("Step '{escaped}' failed: {}", error.message)
        );
        assert_eq!(error.step_id, raw);
        let result = RecipeResult {
            recipe_name: raw.into(),
            success: false,
            step_results: vec![StepResult {
                step_id: raw.into(),
                status: StepStatus::Failed,
                output: String::new(),
                error: error.to_string(),
                duration: None,
                ..Default::default()
            }],
            context: HashMap::new(),
            duration: None,
        };
        let text = result.to_string();
        assert!(text.contains(&format!("Recipe '{escaped}': FAILED")));
        assert!(text.contains(&format!("[failed] {escaped}")));
        assert!(
            text.contains(&error.to_string()),
            "message escaped twice: {text}"
        );
        assert!(!text.contains(raw));
        let structured = serde_json::to_value(&result).unwrap();
        assert_eq!(structured["recipe_name"], raw);
        assert_eq!(structured["step_results"][0]["step_id"], raw);
        assert_eq!(structured["step_results"][0]["error"], error.to_string());
    }

    #[test]
    fn test_effective_type_explicit_bash() {
        let step = Step {
            recovery: None,
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
            fatal: true,
            parallel_group: None,
            when_tags: vec![],
            recovery_on_failure: false,
            model: None,
            context_mode: None,
            effort: None,
        };
        // Explicit type overrides all field-based inference
        assert_eq!(step.effective_type(), StepType::Bash);
    }

    #[test]
    fn test_effective_type_infers_recipe() {
        let step = Step {
            recovery: None,
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
            fatal: true,
            parallel_group: None,
            when_tags: vec![],
            recovery_on_failure: false,
            model: None,
            context_mode: None,
            effort: None,
        };
        assert_eq!(step.effective_type(), StepType::Recipe);
    }

    #[test]
    fn test_effective_type_infers_agent_from_agent_field() {
        let step = Step {
            recovery: None,
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
            fatal: true,
            parallel_group: None,
            when_tags: vec![],
            recovery_on_failure: false,
            model: None,
            context_mode: None,
            effort: None,
        };
        assert_eq!(step.effective_type(), StepType::Agent);
    }

    #[test]
    fn test_effective_type_infers_agent_from_prompt_only() {
        let step = Step {
            recovery: None,
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
            fatal: true,
            parallel_group: None,
            when_tags: vec![],
            recovery_on_failure: false,
            model: None,
            context_mode: None,
            effort: None,
        };
        assert_eq!(step.effective_type(), StepType::Agent);
    }

    #[test]
    fn test_effective_type_infers_bash_with_command_and_prompt() {
        let step = Step {
            recovery: None,
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
            fatal: true,
            parallel_group: None,
            when_tags: vec![],
            recovery_on_failure: false,
            model: None,
            context_mode: None,
            effort: None,
        };
        // command + prompt → Bash (prompt alone would be Agent, but command presence wins)
        assert_eq!(step.effective_type(), StepType::Bash);
    }

    #[test]
    fn test_effective_type_defaults_to_bash() {
        let step = Step {
            recovery: None,
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
            fatal: true,
            parallel_group: None,
            when_tags: vec![],
            recovery_on_failure: false,
            model: None,
            context_mode: None,
            effort: None,
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
            ..Default::default()
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
            ..Default::default()
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
                ..Default::default()
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

/// Checkpoint saved after each step for resume-on-failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeCheckpoint {
    pub recipe_name: String,
    pub completed_steps: Vec<String>,
    pub context: HashMap<String, serde_json::Value>,
    pub timestamp: String,
}

impl RecipeCheckpoint {
    /// Write checkpoint to a file. Returns the path.
    pub fn save(&self, recipe_name: &str) -> std::io::Result<std::path::PathBuf> {
        let safe_name: String = recipe_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(64)
            .collect();
        let path = std::env::temp_dir().join(format!(
            "amplihack-checkpoint-{}-{}.json",
            safe_name,
            std::process::id()
        ));
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&path, json)?;
        log::info!(
            "Checkpoint saved: {} ({} steps completed)",
            path.display(),
            self.completed_steps.len()
        );
        Ok(path)
    }

    /// Load a checkpoint from file.
    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        let json = std::fs::read_to_string(path)?;
        serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}
