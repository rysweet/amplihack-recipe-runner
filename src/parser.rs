/// YAML recipe parser.
///
/// Parses YAML recipe definitions into Recipe model objects, with validation
/// and step-type inference. Direct port from Python `amplihack.recipes.parser`.
use crate::discovery;
use crate::models::{Recipe, StepType};
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
    "mode",
    "working_dir",
    "timeout",
    "auto_stage",
    "recipe",
    "context",
    "continue_on_error",
    "parallel_group",
    "when_tags",
];

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

pub struct RecipeParser;

impl RecipeParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse a recipe YAML file from disk.
    pub fn parse_file(&self, path: &Path) -> Result<Recipe, ParseError> {
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

        Ok(recipe)
    }

    /// Validate a parsed recipe and return a list of warning strings.
    pub fn validate(&self, recipe: &Recipe) -> Vec<String> {
        self.validate_with_yaml(recipe, None)
    }

    /// Validate a parsed recipe with optional raw YAML for field checking.
    pub fn validate_with_yaml(&self, recipe: &Recipe, raw_yaml: Option<&str>) -> Vec<String> {
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
            && let Some(map) = data.as_mapping()
        {
            let known: HashSet<&str> = KNOWN_TOP_FIELDS.iter().copied().collect();
            for key in map.keys() {
                if let Some(key_str) = key.as_str()
                    && !known.contains(key_str)
                {
                    warnings.push(format!(
                        "Unrecognized top-level field '{}' (possible typo)",
                        key_str
                    ));
                }
            }

            // Check step-level fields
            let step_known: HashSet<&str> = KNOWN_STEP_FIELDS.iter().copied().collect();
            if let Some(steps) = map.get(serde_yaml::Value::String("steps".to_string()))
                && let Some(steps_seq) = steps.as_sequence()
            {
                for (i, step_raw) in steps_seq.iter().enumerate() {
                    if let Some(step_map) = step_raw.as_mapping() {
                        let default_sid = format!("index {}", i);
                        let sid = step_map
                            .get(serde_yaml::Value::String("id".to_string()))
                            .and_then(|v| v.as_str())
                            .unwrap_or(&default_sid);
                        for key in step_map.keys() {
                            if let Some(key_str) = key.as_str()
                                && !step_known.contains(key_str)
                            {
                                warnings.push(format!(
                                    "Step '{}': unrecognized field '{}' (possible typo)",
                                    sid, key_str
                                ));
                            }
                        }
                    }
                }
            }
        }

        warnings
    }
}

impl Default for RecipeParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve single-level recipe inheritance via the `extends` field.
///
/// If `recipe.extends` is `Some`, finds and parses the parent recipe, then
/// merges the parent into the child:
/// - Child context values override parent context values
/// - Child steps are appended after parent steps
/// - Child name, version, description override parent
/// - Parent tags are merged with child tags (union)
/// - Recursion config: child overrides if set (non-default)
/// - Hooks: child overrides if set
///
/// Only single-level inheritance is supported (parent's `extends` is ignored).
pub fn resolve_extends(recipe: &mut Recipe, search_dirs: &[PathBuf]) -> Result<(), ParseError> {
    let parent_name = match recipe.extends.take() {
        Some(name) => name,
        None => return Ok(()),
    };

    let parent_path = discovery::find_recipe(&parent_name, Some(search_dirs)).ok_or_else(|| {
        ParseError::Extends(format!(
            "Parent recipe '{}' not found in search directories",
            parent_name
        ))
    })?;

    let parser = RecipeParser::new();
    let parent = parser.parse_file(&parent_path)?;

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
}
