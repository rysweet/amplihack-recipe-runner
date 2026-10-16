/// YAML recipe parser.
///
/// Parses YAML recipe definitions into Recipe model objects, with validation
/// and step-type inference.
use crate::discovery;
use crate::models::{Recipe, StepType};
use log::{debug, info};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const MAX_YAML_SIZE_BYTES: usize = 1_000_000;

/// Top-level fields recognized by the parser.
const KNOWN_TOP_FIELDS: &[&str] = &[
    "name",
    "description",
    "version",
    "author",
    "tags",
    "context",
    "steps",
    "recursion",
    "output",
    "hooks",
    "extends",
];

/// Step-level fields recognized by the parser.
const KNOWN_STEP_FIELDS: &[&str] = &[
    "id",
    "type",
    "agent",
    "prompt",
    "command",
    "output",
    "condition",
    "parse_json",
    "parse_json_required",
    "mode",
    "working_dir",
    "timeout",
    "auto_stage",
    "recipe",
    "context",
    "continue_on_error",
    "fatal",
    "nonfatal",
    "parallel_group",
    "when_tags",
    "recovery_on_failure",
    "model",
];

/// Step fields that were honoured by earlier executors and are now ignored,
/// with the reason shown in the parse-time warning.
const RETIRED_STEP_FIELDS: &[(&str, &str)] = &[(
    "max_env_value_bytes",
    "the runner now spills oversized env values to private files automatically; \
     remove it from the recipe",
)];

/// Most step-field warnings reported for one recipe.
const MAX_STEP_FIELD_WARNINGS: usize = 100;

/// Longest piece of recipe text (name, id, field) quoted in a warning.
const MAX_WARNING_TEXT_CHARS: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("Recipe file not found: {0}")]
    FileNotFound(String),

    #[error("Recipe file too large ({size} bytes). Maximum allowed: {max} bytes")]
    FileTooLarge { size: usize, max: usize },

    #[error("Invalid recipe: {0}")]
    Invalid(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("Extends error: {0}")]
    Extends(String),
}

pub struct RecipeParser {
    /// Report unknown and retired step fields when parsing (#130).
    emit_field_warnings: bool,
}

impl RecipeParser {
    pub fn new() -> Self {
        debug!("RecipeParser::new: creating parser instance");
        Self {
            emit_field_warnings: true,
        }
    }

    /// Parse a recipe YAML file from disk.
    pub fn parse_file(&self, path: &Path) -> Result<Recipe, ParseError> {
        info!("RecipeParser::parse_file: parsing {:?}", path);
        if !path.is_file() {
            return Err(ParseError::FileNotFound(path.display().to_string()));
        }

        let metadata = std::fs::metadata(path)?;
        let size = metadata.len() as usize;
        if size > MAX_YAML_SIZE_BYTES {
            return Err(ParseError::FileTooLarge {
                size,
                max: MAX_YAML_SIZE_BYTES,
            });
        }

        let content = std::fs::read_to_string(path)?;
        self.parse(&content)
    }

    /// Parse a YAML string into a Recipe.
    pub fn parse(&self, yaml_content: &str) -> Result<Recipe, ParseError> {
        debug!(
            "RecipeParser::parse: parsing {} bytes of YAML",
            yaml_content.len()
        );
        if yaml_content.len() > MAX_YAML_SIZE_BYTES {
            return Err(ParseError::FileTooLarge {
                size: yaml_content.len(),
                max: MAX_YAML_SIZE_BYTES,
            });
        }

        let recipe: Recipe = serde_yaml::from_str(yaml_content)?;

        if recipe.name.is_empty() {
            return Err(ParseError::Invalid(
                "Recipe must have a 'name' field".to_string(),
            ));
        }

        if recipe.steps.is_empty() {
            return Err(ParseError::Invalid(
                "Recipe must have a 'steps' field with at least one step".to_string(),
            ));
        }

        // Check for duplicate step IDs
        let mut seen = HashSet::new();
        for step in &recipe.steps {
            if step.id.is_empty() {
                return Err(ParseError::Invalid(
                    "Every step must have a non-empty 'id' field".to_string(),
                ));
            }
            if !seen.insert(&step.id) {
                return Err(ParseError::Invalid(format!(
                    "Duplicate step id: '{}'",
                    step.id
                )));
            }
        }

        if self.emit_field_warnings
            && let Ok(raw) = serde_yaml::from_str::<serde_yaml::Value>(yaml_content)
        {
            for warning in step_field_warnings(&raw) {
                // Sub-recipes are re-parsed on every invocation; report each
                // distinct warning once per process.
                if !first_report(&warning) {
                    continue;
                }
                // Without a logger configured for warnings (the CLI's default),
                // `warn!` would drop the message, and silently ignored fields
                // are exactly what this check exists to surface.
                if log::log_enabled!(log::Level::Warn) {
                    log::warn!("{warning}");
                } else {
                    eprintln!("warning: {warning}");
                }
            }
        }

        Ok(recipe)
    }

    /// Validate a parsed recipe and return a list of warning strings.
    pub fn validate(&self, recipe: &Recipe) -> Vec<String> {
        debug!(
            "RecipeParser::validate: validating recipe '{}'",
            recipe.name
        );
        self.validate_with_yaml(recipe, None)
    }

    /// Validate a parsed recipe with optional raw YAML for field checking.
    pub fn validate_with_yaml(&self, recipe: &Recipe, raw_yaml: Option<&str>) -> Vec<String> {
        debug!(
            "RecipeParser::validate_with_yaml: validating recipe '{}' ({} steps)",
            recipe.name,
            recipe.steps.len()
        );
        let mut warnings = Vec::new();

        for step in &recipe.steps {
            let st = step.effective_type();
            match st {
                StepType::Agent => {
                    if step.prompt.is_none() {
                        warnings.push(format!(
                            "Step '{}': agent step is missing a 'prompt' field",
                            step.id
                        ));
                    }
                }
                StepType::Bash => {
                    if step.command.is_none() {
                        warnings.push(format!(
                            "Step '{}': bash step is missing a 'command' field",
                            step.id
                        ));
                    }
                }
                StepType::Recipe => {
                    if step.recipe.is_none() {
                        warnings.push(format!(
                            "Step '{}': recipe step is missing a 'recipe' field",
                            step.id
                        ));
                    }
                }
            }
        }

        // Check for unrecognized fields if raw YAML is provided
        if let Some(yaml_str) = raw_yaml
            && let Ok(data) = serde_yaml::from_str::<serde_yaml::Value>(yaml_str)
        {
            if let Some(map) = data.as_mapping() {
                let known: HashSet<&str> = KNOWN_TOP_FIELDS.iter().copied().collect();
                for key in map.keys() {
                    if let Some(key_str) = key.as_str()
                        && !known.contains(key_str)
                    {
                        warnings.push(format!(
                            "Unrecognized top-level field '{}' (possible typo)",
                            warning_text(key_str)
                        ));
                    }
                }
            }
            warnings.extend(step_field_warnings(&data));
        }

        warnings
    }
}

impl RecipeParser {
    /// Opt out of parse-time step-field warnings. `--validate-only` and
    /// [`crate::validate_recipe`] use this because they report the same
    /// warnings through [`RecipeParser::validate_with_yaml`].
    pub fn without_field_warnings(self) -> Self {
        Self {
            emit_field_warnings: false,
        }
    }
}

/// #130: one warning per unknown or retired step field in the raw recipe YAML.
/// Never fails; malformed shapes yield no warnings.
fn step_field_warnings(raw: &serde_yaml::Value) -> Vec<String> {
    let Some(map) = raw.as_mapping() else {
        return Vec::new();
    };
    let Some(steps) = map.get("steps").and_then(|v| v.as_sequence()) else {
        return Vec::new();
    };
    let recipe = map
        .get("name")
        .and_then(|v| v.as_str())
        .map_or_else(|| "<unnamed>".to_string(), warning_text);

    let mut warnings = Vec::new();
    let mut overflow = 0usize;
    for (i, step) in steps.iter().enumerate() {
        let Some(step_map) = step.as_mapping() else {
            continue;
        };
        let step_id = step_map
            .get("id")
            .and_then(|v| v.as_str())
            .map_or_else(|| format!("<index {i}>"), warning_text);
        for key in step_map.keys() {
            let key_str = key.as_str();
            if key_str.is_some_and(|k| KNOWN_STEP_FIELDS.contains(&k)) {
                continue;
            }
            if warnings.len() == MAX_STEP_FIELD_WARNINGS {
                overflow += 1;
                continue;
            }
            let retired =
                key_str.and_then(|k| RETIRED_STEP_FIELDS.iter().find(|(name, _)| *name == k));
            warnings.push(match (retired, key_str) {
                (Some((field, reason)), _) => format!(
                    "recipe '{recipe}': step '{step_id}': field '{field}' is retired and \
                     ignored — {reason}"
                ),
                (None, Some(k)) => format!(
                    "recipe '{recipe}': step '{step_id}' has unknown field '{}' (ignored)",
                    warning_text(k)
                ),
                (None, None) => format!(
                    "recipe '{recipe}': step '{step_id}' has unknown field '<non-string key>' \
                     (ignored)"
                ),
            });
        }
    }
    if overflow > 0 {
        warnings.push(format!("… and {overflow} more unknown step fields"));
    }
    warnings
}

/// Whether `warning` has not been reported by [`RecipeParser::parse`] in this
/// process yet. Records it as reported.
fn first_report(warning: &str) -> bool {
    static REPORTED: std::sync::Mutex<Option<HashSet<String>>> = std::sync::Mutex::new(None);
    REPORTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashSet::new)
        .insert(warning.to_string())
}

/// Recipe-supplied text made safe for a terminal or log line: escaped, and
/// capped at [`MAX_WARNING_TEXT_CHARS`] characters.
fn warning_text(raw: &str) -> String {
    let escaped: String = raw.escape_debug().collect();
    if escaped.chars().count() <= MAX_WARNING_TEXT_CHARS {
        escaped
    } else {
        let mut capped: String = escaped.chars().take(MAX_WARNING_TEXT_CHARS).collect();
        capped.push('…');
        capped
    }
}

impl Default for RecipeParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve recipe inheritance via the `extends` field, recursively.
///
/// If `recipe.extends` is `Some`, finds and parses the parent recipe, then
/// recursively resolves the parent's own `extends` chain *before* merging
/// the parent into the child. The merge semantics are:
///
/// - Child context values override parent context values
/// - Child steps are appended after parent steps (ancestor-first ordering)
/// - Child name, version, description override parent
/// - Parent tags are merged with child tags (union, sorted)
/// - Recursion config: child overrides if non-default, otherwise inherited
/// - Hooks: each hook field is inherited individually if unset on the child
///
/// Cycles in the `extends` chain are detected via a path-based visited set
/// and reported as an error of the form
/// `"Circular extends detected: '<name>' was already visited in the extends chain"`.
pub fn resolve_extends(recipe: &mut Recipe, search_dirs: &[PathBuf]) -> Result<(), ParseError> {
    info!(
        "resolve_extends: resolving extends for recipe '{}'",
        recipe.name
    );
    resolve_extends_with_visited(recipe, search_dirs, &mut HashSet::new())
}

fn resolve_extends_with_visited(
    recipe: &mut Recipe,
    search_dirs: &[PathBuf],
    visited: &mut HashSet<String>,
) -> Result<(), ParseError> {
    debug!(
        "resolve_extends_with_visited: processing recipe '{}', visited={:?}",
        recipe.name, visited
    );
    let parent_name = match recipe.extends.take() {
        Some(name) => name,
        None => return Ok(()),
    };

    if !visited.insert(parent_name.clone()) {
        return Err(ParseError::Extends(format!(
            "Circular extends detected: '{}' was already visited in the extends chain",
            parent_name
        )));
    }

    let parent_path = discovery::find_recipe(&parent_name, Some(search_dirs)).ok_or_else(|| {
        ParseError::Extends(format!(
            "Parent recipe '{}' not found in search directories",
            parent_name
        ))
    })?;

    let parser = RecipeParser::new();
    let mut parent = parser.parse_file(&parent_path)?;

    // Recursively resolve parent's extends before merging
    if parent.extends.is_some() {
        resolve_extends_with_visited(&mut parent, search_dirs, visited)?;
    }

    // Merge context: start with parent, child overrides
    let mut merged_context = parent.context;
    merged_context.extend(recipe.context.drain());
    recipe.context = merged_context;

    // Merge steps: parent steps first, then child steps
    let child_steps = std::mem::take(&mut recipe.steps);
    let mut merged_steps = parent.steps;
    merged_steps.extend(child_steps);
    recipe.steps = merged_steps;

    // Merge tags: union
    let mut tag_set: HashSet<String> = parent.tags.into_iter().collect();
    tag_set.extend(recipe.tags.drain(..));
    recipe.tags = tag_set.into_iter().collect();
    recipe.tags.sort();

    // Recursion: child overrides if non-default
    let default_recursion = crate::models::RecursionConfig::default();
    if recipe.recursion.max_depth == default_recursion.max_depth
        && recipe.recursion.max_total_steps == default_recursion.max_total_steps
    {
        recipe.recursion = parent.recursion;
    }

    // Hooks: child field overrides parent field individually
    if recipe.hooks.pre_step.is_none() {
        recipe.hooks.pre_step = parent.hooks.pre_step;
    }
    if recipe.hooks.post_step.is_none() {
        recipe.hooks.post_step = parent.hooks.post_step;
    }
    if recipe.hooks.on_error.is_none() {
        recipe.hooks.on_error = parent.hooks.on_error;
    }

    debug!(
        "resolve_extends_with_visited: extends for '{}' resolved successfully",
        recipe.name
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_recipe() {
        let yaml = r#"
name: "test-recipe"
steps:
  - id: "step-1"
    command: "echo hello"
"#;
        let parser = RecipeParser::new();
        let recipe = parser.parse(yaml).unwrap();
        assert_eq!(recipe.name, "test-recipe");
        assert_eq!(recipe.steps.len(), 1);
        assert_eq!(recipe.steps[0].id, "step-1");
        assert_eq!(recipe.steps[0].effective_type(), StepType::Bash);
    }

    #[test]
    fn test_parse_agent_step() {
        let yaml = r#"
name: "agent-recipe"
steps:
  - id: "agent-step"
    agent: "amplihack:core:architect"
    prompt: "Do something"
"#;
        let parser = RecipeParser::new();
        let recipe = parser.parse(yaml).unwrap();
        assert_eq!(recipe.steps[0].effective_type(), StepType::Agent);
    }

    #[test]
    fn test_parse_recipe_step() {
        let yaml = r#"
name: "parent-recipe"
steps:
  - id: "sub"
    type: "recipe"
    recipe: "child-recipe"
"#;
        let parser = RecipeParser::new();
        let recipe = parser.parse(yaml).unwrap();
        assert_eq!(recipe.steps[0].effective_type(), StepType::Recipe);
    }

    #[test]
    fn test_reject_duplicate_step_ids() {
        let yaml = r#"
name: "dup-recipe"
steps:
  - id: "same-id"
    command: "echo 1"
  - id: "same-id"
    command: "echo 2"
"#;
        let parser = RecipeParser::new();
        let err = parser.parse(yaml).unwrap_err();
        assert!(err.to_string().contains("Duplicate step id"));
    }

    #[test]
    fn test_reject_empty_name() {
        let yaml = r#"
name: ""
steps:
  - id: "step-1"
    command: "echo hello"
"#;
        let parser = RecipeParser::new();
        let err = parser.parse(yaml).unwrap_err();
        assert!(err.to_string().contains("name"));
    }

    #[test]
    fn test_validate_missing_prompt() {
        let yaml = r#"
name: "bad-recipe"
steps:
  - id: "agent-no-prompt"
    agent: "amplihack:core:architect"
"#;
        let parser = RecipeParser::new();
        let recipe = parser.parse(yaml).unwrap();
        let warnings = parser.validate(&recipe);
        assert!(warnings.iter().any(|w| w.contains("missing a 'prompt'")));
    }

    #[test]
    fn test_parse_with_context() {
        let yaml = r#"
name: "ctx-recipe"
context:
  task_description: "hello"
  repo_path: "."
steps:
  - id: "step-1"
    command: "echo {{task_description}}"
"#;
        let parser = RecipeParser::new();
        let recipe = parser.parse(yaml).unwrap();
        assert!(recipe.context.contains_key("task_description"));
    }

    #[test]
    fn test_validate_unrecognized_fields() {
        let yaml = r#"
name: "typo-recipe"
descrption: "typo!"
steps:
  - id: "step-1"
    comand: "echo oops"
"#;
        let parser = RecipeParser::new();
        let recipe = parser.parse(yaml).unwrap();
        let warnings = parser.validate_with_yaml(&recipe, Some(yaml));
        assert!(warnings.iter().any(|w| w.contains("descrption")));
        assert!(warnings.iter().any(|w| w.contains("comand")));
    }

    #[test]
    fn test_resolve_extends_inherits_parent_steps() {
        let tmp = tempfile::tempdir().unwrap();
        let parent_yaml = r#"
name: "parent"
description: "Parent recipe"
tags: ["base", "shared"]
context:
  parent_var: "from-parent"
  shared_var: "parent-value"
steps:
  - id: "parent-step-1"
    command: "echo parent step 1"
  - id: "parent-step-2"
    command: "echo parent step 2"
"#;
        std::fs::write(tmp.path().join("parent.yaml"), parent_yaml).unwrap();

        let child_yaml = r#"
name: "child"
description: "Child recipe"
extends: "parent"
tags: ["child-tag", "shared"]
context:
  child_var: "from-child"
  shared_var: "child-value"
steps:
  - id: "child-step-1"
    command: "echo child step 1"
"#;
        let parser = RecipeParser::new();
        let mut recipe = parser.parse(child_yaml).unwrap();
        let search_dirs = vec![tmp.path().to_path_buf()];
        resolve_extends(&mut recipe, &search_dirs).unwrap();

        // Child name/description override parent
        assert_eq!(recipe.name, "child");
        assert_eq!(recipe.description, "Child recipe");

        // Parent steps come first, then child steps
        assert_eq!(recipe.steps.len(), 3);
        assert_eq!(recipe.steps[0].id, "parent-step-1");
        assert_eq!(recipe.steps[1].id, "parent-step-2");
        assert_eq!(recipe.steps[2].id, "child-step-1");

        // Child context overrides parent context
        assert_eq!(
            recipe.context.get("shared_var").and_then(|v| v.as_str()),
            Some("child-value")
        );
        assert_eq!(
            recipe.context.get("parent_var").and_then(|v| v.as_str()),
            Some("from-parent")
        );
        assert_eq!(
            recipe.context.get("child_var").and_then(|v| v.as_str()),
            Some("from-child")
        );

        // Tags are merged (union)
        assert!(recipe.tags.contains(&"base".to_string()));
        assert!(recipe.tags.contains(&"shared".to_string()));
        assert!(recipe.tags.contains(&"child-tag".to_string()));

        // extends is consumed (set to None)
        assert!(recipe.extends.is_none());
    }

    #[test]
    fn test_resolve_extends_no_extends() {
        let yaml = r#"
name: "standalone"
steps:
  - id: "step-1"
    command: "echo hello"
"#;
        let parser = RecipeParser::new();
        let mut recipe = parser.parse(yaml).unwrap();
        // Should be a no-op
        resolve_extends(&mut recipe, &[]).unwrap();
        assert_eq!(recipe.steps.len(), 1);
    }

    #[test]
    fn test_resolve_extends_parent_not_found() {
        let yaml = r#"
name: "orphan"
extends: "nonexistent-parent"
steps:
  - id: "step-1"
    command: "echo hello"
"#;
        let parser = RecipeParser::new();
        let mut recipe = parser.parse(yaml).unwrap();
        let err = resolve_extends(&mut recipe, &[]).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_resolve_extends_recursive_three_level_chain() {
        // grandparent <- parent <- child : steps must be ordered
        // grandparent's steps, then parent's, then child's.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("grand.yaml"),
            r#"
name: "grand"
context:
  layer: "grand"
  grand_only: "g"
steps:
  - id: "grand-step"
    command: "echo grand"
"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("mid.yaml"),
            r#"
name: "mid"
extends: "grand"
context:
  layer: "mid"
  mid_only: "m"
steps:
  - id: "mid-step"
    command: "echo mid"
"#,
        )
        .unwrap();

        let child_yaml = r#"
name: "leaf"
extends: "mid"
context:
  layer: "leaf"
steps:
  - id: "leaf-step"
    command: "echo leaf"
"#;
        let parser = RecipeParser::new();
        let mut recipe = parser.parse(child_yaml).unwrap();
        let dirs = vec![tmp.path().to_path_buf()];
        resolve_extends(&mut recipe, &dirs).unwrap();

        // All three levels' steps appear in dependency order
        let ids: Vec<&str> = recipe.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["grand-step", "mid-step", "leaf-step"]);
        // Deepest child wins on overlapping context key
        assert_eq!(
            recipe.context.get("layer").and_then(|v| v.as_str()),
            Some("leaf")
        );
        // Inherited keys from both ancestors are present
        assert_eq!(
            recipe.context.get("grand_only").and_then(|v| v.as_str()),
            Some("g")
        );
        assert_eq!(
            recipe.context.get("mid_only").and_then(|v| v.as_str()),
            Some("m")
        );
        assert!(recipe.extends.is_none(), "extends must be consumed");
    }

    #[test]
    fn test_resolve_extends_detects_cycle() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("a.yaml"),
            "name: a\nextends: b\nsteps:\n  - id: a1\n    command: echo",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("b.yaml"),
            "name: b\nextends: a\nsteps:\n  - id: b1\n    command: echo",
        )
        .unwrap();
        let parser = RecipeParser::new();
        let mut recipe = parser.parse_file(&tmp.path().join("a.yaml")).unwrap();
        let err = resolve_extends(&mut recipe, &[tmp.path().to_path_buf()]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Circular extends") || msg.to_lowercase().contains("circular"),
            "expected cycle error, got: {}",
            msg
        );
    }

    // ── Edge cases (test-5) ──────────────────────────────

    #[test]
    fn test_parse_empty_yaml() {
        let parser = RecipeParser::new();
        let result = parser.parse("");
        assert!(result.is_err(), "empty YAML should fail");
    }

    #[test]
    fn test_parse_whitespace_only_yaml() {
        let parser = RecipeParser::new();
        let result = parser.parse("   \n\n  ");
        assert!(result.is_err(), "whitespace-only YAML should fail");
    }

    #[test]
    fn test_parse_no_steps() {
        let parser = RecipeParser::new();
        let result = parser.parse("name: test\n");
        assert!(result.is_err(), "recipe with no steps should fail");
    }

    #[test]
    fn test_parse_empty_steps_list() {
        let parser = RecipeParser::new();
        let result = parser.parse("name: test\nsteps: []\n");
        // Empty steps list is technically valid YAML but useless — should either
        // parse to Recipe with 0 steps or error
        if let Ok(r) = result {
            assert!(r.steps.is_empty());
        }
    }

    #[test]
    fn test_parse_step_with_empty_id() {
        let yaml = r#"
name: test
steps:
  - id: ""
    command: "echo hello"
"#;
        let parser = RecipeParser::new();
        let result = parser.parse(yaml);
        // Parser rejects empty step IDs — this is correct validation
        assert!(result.is_err(), "empty step ID should be rejected");
    }

    #[test]
    fn test_parse_step_with_empty_command() {
        let yaml = r#"
name: test
steps:
  - id: "step1"
    command: ""
"#;
        let parser = RecipeParser::new();
        let result = parser.parse(yaml);
        assert!(result.is_ok());
        let recipe = result.unwrap();
        assert_eq!(recipe.steps[0].command.as_deref(), Some(""));
    }

    // ── Boundary values (test-6) ──────────────────────────

    #[test]
    fn test_parse_yaml_exactly_at_size_limit() {
        let parser = RecipeParser::new();
        // Create YAML that's exactly at MAX_YAML_SIZE_BYTES
        let base = "name: test\nsteps:\n  - id: step1\n    command: echo ";
        let padding_needed = MAX_YAML_SIZE_BYTES - base.len();
        let yaml = format!("{}{}", base, "x".repeat(padding_needed));
        assert_eq!(yaml.len(), MAX_YAML_SIZE_BYTES);
        // Should parse successfully (at limit, not over)
        let result = parser.parse(&yaml);
        assert!(
            result.is_ok(),
            "YAML at exactly the size limit should be accepted"
        );
    }

    #[test]
    fn test_parse_yaml_one_byte_over_limit() {
        let parser = RecipeParser::new();
        let base = "name: test\nsteps:\n  - id: step1\n    command: echo ";
        let padding_needed = MAX_YAML_SIZE_BYTES - base.len() + 1;
        let yaml = format!("{}{}", base, "x".repeat(padding_needed));
        assert_eq!(yaml.len(), MAX_YAML_SIZE_BYTES + 1);
        let result = parser.parse(&yaml);
        assert!(
            result.is_err(),
            "YAML over the size limit should be rejected"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("too large"),
            "error should mention size: {}",
            err_msg
        );
    }

    #[test]
    fn test_parse_name_with_special_characters() {
        let yaml = r#"
name: "test-recipe_v2.1 (beta)"
steps:
  - id: "step-1"
    command: "echo hello"
"#;
        let parser = RecipeParser::new();
        let result = parser.parse(yaml);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().name, "test-recipe_v2.1 (beta)");
    }

    // ── Property-based tests (PR4: audit/proptest-parser-template) ──────

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        /// Strategy: generate syntactically valid minimal YAML recipes.
        fn valid_recipe_yaml() -> impl Strategy<Value = String> {
            (
                "[a-zA-Z][a-zA-Z0-9_-]{0,30}", // recipe name
                proptest::collection::vec(
                    "[a-zA-Z][a-zA-Z0-9_-]{0,20}", // step IDs
                    1..=5,
                ),
            )
                .prop_map(|(name, step_ids)| {
                    let mut yaml = format!("name: \"{}\"\nsteps:\n", name);
                    // Deduplicate step IDs
                    let mut seen = std::collections::HashSet::new();
                    for (i, id) in step_ids.iter().enumerate() {
                        let unique_id = if seen.insert(id.clone()) {
                            id.clone()
                        } else {
                            format!("{}-dup{}", id, i)
                        };
                        yaml.push_str(&format!(
                            "  - id: \"{}\"\n    command: \"echo step {}\"\n",
                            unique_id, i
                        ));
                    }
                    yaml
                })
        }

        // P4-1: Parser never panics on arbitrary byte strings
        proptest! {
            #[test]
            fn parser_no_panic_on_arbitrary_input(s in "\\PC{0,500}") {
                let parser = RecipeParser::new();
                let _ = parser.parse(&s);
            }
        }

        // P4-2: Valid YAML recipes always parse successfully
        proptest! {
            #[test]
            fn valid_recipes_always_parse(yaml in valid_recipe_yaml()) {
                let parser = RecipeParser::new();
                let result = parser.parse(&yaml);
                prop_assert!(result.is_ok(), "Failed to parse valid YAML:\n{}\nError: {:?}", yaml, result.err());
            }
        }

        // P4-3: Size limit is enforced — any input > MAX_YAML_SIZE_BYTES is rejected
        proptest! {
            #[test]
            fn oversized_yaml_rejected(extra in 1..1000usize) {
                let parser = RecipeParser::new();
                let base = "name: test\nsteps:\n  - id: s1\n    command: echo ";
                let padding = "x".repeat(MAX_YAML_SIZE_BYTES - base.len() + extra);
                let yaml = format!("{}{}", base, padding);
                prop_assert!(yaml.len() > MAX_YAML_SIZE_BYTES);
                let result = parser.parse(&yaml);
                prop_assert!(result.is_err(), "Oversized YAML should be rejected");
                let err_msg = result.unwrap_err().to_string();
                prop_assert!(err_msg.contains("too large"), "Error should mention size limit: {}", err_msg);
            }
        }

        // P4-4: Parse then validate roundtrip — parsed recipes always validate without panic
        proptest! {
            #[test]
            fn parse_then_validate_no_panic(yaml in valid_recipe_yaml()) {
                let parser = RecipeParser::new();
                if let Ok(recipe) = parser.parse(&yaml) {
                    let _warnings = parser.validate(&recipe);
                    // Also test validate_with_yaml path
                    let _warnings_with_yaml = parser.validate_with_yaml(&recipe, Some(&yaml));
                }
            }
        }

        // P4-5: Duplicate step IDs are always detected
        proptest! {
            #[test]
            fn duplicate_ids_detected(
                name in "[a-zA-Z][a-zA-Z0-9]{0,10}",
                dup_id in "[a-zA-Z][a-zA-Z0-9]{0,10}",
            ) {
                let yaml = format!(
                    "name: \"{}\"\nsteps:\n  - id: \"{}\"\n    command: echo a\n  - id: \"{}\"\n    command: echo b\n",
                    name, dup_id, dup_id
                );
                let parser = RecipeParser::new();
                let result = parser.parse(&yaml);
                prop_assert!(result.is_err(), "Duplicate IDs should be rejected");
                let msg = result.unwrap_err().to_string();
                prop_assert!(msg.contains("Duplicate"), "Error should mention duplicate: {}", msg);
            }
        }

        // P4-6: Empty names are always rejected
        proptest! {
            #[test]
            fn empty_name_rejected(step_cmd in "[a-zA-Z0-9 ]{1,30}") {
                let yaml = format!(
                    "name: \"\"\nsteps:\n  - id: \"s1\"\n    command: \"{}\"\n",
                    step_cmd
                );
                let parser = RecipeParser::new();
                let result = parser.parse(&yaml);
                prop_assert!(result.is_err(), "Empty name should be rejected");
            }
        }
    }
}

#[cfg(test)]
mod step_field_warning_tests {
    //! #130 PART 2: unknown step fields used to be dropped silently by serde,
    //! which is how `max_env_value_bytes` stopped protecting amplihack recipes
    //! without anyone noticing.
    use super::*;
    use crate::models::Step;

    fn warnings_for(yaml: &str) -> Vec<String> {
        step_field_warnings(&serde_yaml::from_str(yaml).unwrap())
    }

    #[test]
    fn each_distinct_warning_is_reported_once_per_process() {
        let warning = format!(
            "recipe 'r': step '{}' has unknown field 'x'",
            uuid::Uuid::new_v4()
        );
        assert!(first_report(&warning), "first sighting must be reported");
        assert!(
            !first_report(&warning),
            "a repeat parse must not report it again"
        );
        assert!(first_report(&format!("{warning} (other)")));
    }

    const RETIRED: &str = "recipe 'r': step 'checkpoint': field 'max_env_value_bytes' is retired \
                           and ignored — the runner now spills oversized env values to private \
                           files automatically; remove it from the recipe";

    #[test]
    fn retired_max_env_value_bytes_parses_and_names_the_step() {
        let yaml = "name: r\nsteps:\n  - id: checkpoint\n    command: echo hi\n    max_env_value_bytes: 65536\n";
        let recipe = RecipeParser::new()
            .parse(yaml)
            .expect("an unknown field must never fail the parse");
        assert_eq!(recipe.steps[0].id, "checkpoint");
        assert_eq!(warnings_for(yaml), vec![RETIRED.to_string()]);
    }

    #[test]
    fn unknown_field_gets_the_generic_warning() {
        let yaml = "name: r\nsteps:\n  - id: s1\n    command: echo\n    foo: 1\n";
        assert_eq!(
            warnings_for(yaml),
            vec!["recipe 'r': step 's1' has unknown field 'foo' (ignored)".to_string()]
        );
    }

    #[test]
    fn missing_ids_and_names_use_placeholders() {
        let yaml = "steps:\n  - id: a\n    command: x\n  - command: y\n    foo: 1\n";
        assert_eq!(
            warnings_for(yaml),
            vec![
                "recipe '<unnamed>': step '<index 1>' has unknown field 'foo' (ignored)"
                    .to_string()
            ]
        );
    }

    #[test]
    fn known_fields_including_fatal_and_nonfatal_do_not_warn() {
        let yaml = r#"
name: r
steps:
  - id: a
    type: bash
    command: echo
    output: out
    condition: "true"
    parse_json: true
    parse_json_required: false
    working_dir: .
    timeout: 5
    auto_stage: false
    continue_on_error: false
    fatal: false
    parallel_group: g
    when_tags: [x]
    recovery_on_failure: false
  - id: b
    type: agent
    agent: builder
    prompt: hi
    mode: fast
    model: m
    nonfatal: true
  - id: c
    type: recipe
    recipe: child
    context: {k: v}
"#;
        assert!(RecipeParser::new().parse(yaml).is_ok());
        assert_eq!(warnings_for(yaml), Vec::<String>::new());
    }

    #[test]
    fn known_step_fields_cover_every_step_field_and_alias() {
        // Exhaustive literal: adding a Step field without updating this test
        // (and KNOWN_STEP_FIELDS) is a compile error, not a false warning.
        let step = Step {
            id: "x".into(),
            step_type: Some(StepType::Bash),
            command: Some("c".into()),
            agent: Some("a".into()),
            prompt: Some("p".into()),
            output: Some("o".into()),
            condition: Some("true".into()),
            parse_json: true,
            parse_json_required: true,
            mode: Some("m".into()),
            working_dir: Some(".".into()),
            timeout: Some(1),
            auto_stage: Some(true),
            recipe: Some("r".into()),
            sub_context: Some(Default::default()),
            continue_on_error: true,
            fatal: true,
            parallel_group: Some("g".into()),
            when_tags: vec!["t".into()],
            recovery_on_failure: true,
            model: Some("m".into()),
        };
        let json = serde_json::to_value(&step).unwrap();
        for key in json.as_object().unwrap().keys() {
            assert!(
                KNOWN_STEP_FIELDS.contains(&key.as_str()),
                "Step field '{key}' missing from KNOWN_STEP_FIELDS"
            );
        }
        assert!(KNOWN_STEP_FIELDS.contains(&"nonfatal"), "serde alias");
        assert!(!KNOWN_STEP_FIELDS.contains(&"max_env_value_bytes"));
    }

    #[test]
    fn warning_text_from_yaml_is_escaped_and_capped() {
        // S8: keys are attacker-controlled text going to a terminal/log.
        let long = "z".repeat(300);
        let yaml = format!(
            "name: r\nsteps:\n  - id: s\n    command: x\n    \"bad\\nkey\\u001b[31m'\": 1\n    {long}: 1\n"
        );
        let w = warnings_for(&yaml);
        assert_eq!(w.len(), 2, "{w:?}");
        for line in &w {
            assert!(!line.contains('\n') && !line.contains('\u{1b}'), "{line:?}");
        }
        assert!(w.iter().any(|l| l.contains("bad\\nkey")), "{w:?}");
        let capped = w.iter().find(|l| l.contains("zzz")).unwrap();
        assert!(capped.contains('…'), "{capped}");
        assert!(
            capped.chars().filter(|c| *c == 'z').count() <= 128,
            "{capped}"
        );
    }

    #[test]
    fn non_string_keys_do_not_panic() {
        let yaml = "name: r\nsteps:\n  - id: s\n    command: x\n    1: one\n";
        assert_eq!(
            warnings_for(yaml),
            vec!["recipe 'r': step 's' has unknown field '<non-string key>' (ignored)".to_string()]
        );
    }

    #[test]
    fn malformed_shapes_yield_no_warnings() {
        for yaml in [
            "just a string",
            "name: r\nsteps: 3\n",
            "name: r\nsteps:\n  - 7\n",
        ] {
            assert!(warnings_for(yaml).is_empty(), "{yaml}");
        }
    }

    #[test]
    fn warnings_are_capped_at_one_hundred() {
        // S8/SR-13
        let mut yaml = String::from("name: r\nsteps:\n  - id: s\n    command: x\n");
        for i in 0..150 {
            yaml.push_str(&format!("    f{i:03}: 1\n"));
        }
        let w = warnings_for(&yaml);
        assert_eq!(w.len(), 101);
        assert_eq!(w[100], "… and 50 more unknown step fields");
    }

    #[test]
    fn validate_with_yaml_reports_step_fields_through_the_shared_helper() {
        let yaml = "name: r\ndescrption: oops\nsteps:\n  - id: checkpoint\n    command: echo hi\n    max_env_value_bytes: 65536\n";
        let parser = RecipeParser::new().without_field_warnings();
        let recipe = parser.parse(yaml).unwrap();
        let w = parser.validate_with_yaml(&recipe, Some(yaml));
        assert!(w.contains(&RETIRED.to_string()), "{w:?}");
        assert!(
            w.contains(&"Unrecognized top-level field 'descrption' (possible typo)".to_string()),
            "top-level warning is unchanged: {w:?}"
        );
        assert_eq!(
            w.iter()
                .filter(|l| l.contains("max_env_value_bytes"))
                .count(),
            1
        );
    }
}
