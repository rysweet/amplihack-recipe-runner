pub mod adapters;
pub mod agent_resolver;
pub mod context;
pub mod discovery;
pub mod models;
pub mod parser;
pub mod runner;

// Public API convenience functions

use models::{Recipe, RecipeResult};
use parser::RecipeParser;
use runner::RecipeRunner;
use adapters::Adapter;
use serde_json::Value;
use std::collections::HashMap;

/// Shortcut: parse a YAML string into a Recipe.
pub fn parse_recipe(yaml_content: &str) -> Result<Recipe, parser::ParseError> {
    RecipeParser::new().parse(yaml_content)
}

/// Shortcut: parse and execute a recipe in one call.
pub fn run_recipe<A: Adapter>(
    yaml_content: &str,
    adapter: A,
    user_context: Option<HashMap<String, Value>>,
    dry_run: bool,
) -> Result<RecipeResult, parser::ParseError> {
    let recipe = parse_recipe(yaml_content)?;
    let runner = RecipeRunner::new(adapter).with_dry_run(dry_run);
    Ok(runner.execute(&recipe, user_context))
}

/// Find a recipe by name, parse it, and execute it.
pub fn run_recipe_by_name<A: Adapter>(
    name: &str,
    adapter: A,
    user_context: Option<HashMap<String, Value>>,
    dry_run: bool,
) -> Result<RecipeResult, Box<dyn std::error::Error>> {
    let path = discovery::find_recipe(name, None)
        .ok_or_else(|| format!("Recipe '{}' not found in any search directory", name))?;
    let recipe = RecipeParser::new().parse_file(&path)?;
    let runner = RecipeRunner::new(adapter).with_dry_run(dry_run);
    Ok(runner.execute(&recipe, user_context))
}

/// Validate a recipe and return warnings.
pub fn validate_recipe(yaml_content: &str) -> Result<Vec<String>, parser::ParseError> {
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml_content)?;
    Ok(parser.validate_with_yaml(&recipe, Some(yaml_content)))
}
