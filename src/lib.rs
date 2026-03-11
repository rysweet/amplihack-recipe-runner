//! # amplihack Recipe Runner
//!
//! A Rust library for parsing and executing YAML-defined recipes.
//! Recipes are declarative workflows composed of bash commands and agent steps,
//! with support for conditions, output chaining, parallel execution, and inheritance.
//!
//! ## Quick Start
//!
//! ```no_run
//! use recipe_runner_rs::adapters::cli_subprocess::CLISubprocessAdapter;
//!
//! let yaml = r#"
//! name: example
//! steps:
//!   - id: greet
//!     command: "echo hello"
//! "#;
//!
//! let adapter = CLISubprocessAdapter::new();
//! let result = recipe_runner_rs::run_recipe(yaml, adapter, None, false).unwrap();
//! assert!(result.success);
//! ```

pub mod adapters;
pub mod agent_resolver;
pub mod condition;
pub mod context;
pub mod discovery;
pub mod models;
pub mod parser;
pub mod runner;

/// Safely truncate a string to at most `max_bytes` bytes at a UTF-8 boundary.
///
/// Returns a slice of the original string that is at most `max_bytes` long,
/// ending at a valid character boundary. If the string is shorter than
/// `max_bytes`, the full string is returned unchanged.
pub fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    log::trace!("safe_truncate: len={}, max_bytes={}", s.len(), max_bytes);
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Safely get the tail of a string starting at most `max_bytes` from the end.
///
/// Returns a slice of the string comprising at most the last `max_bytes` bytes,
/// starting at a valid character boundary.
pub fn safe_tail(s: &str, max_bytes: usize) -> &str {
    log::trace!("safe_tail: len={}, max_bytes={}", s.len(), max_bytes);
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

// Public API convenience functions

use adapters::Adapter;
use models::{Recipe, RecipeResult};
use parser::{RecipeParser, resolve_extends};
use runner::RecipeRunner;
use serde_json::Value;
use std::collections::HashMap;
use log::{debug, info};

/// Parse a YAML string into a [`Recipe`].
///
/// # Errors
///
/// Returns [`parser::ParseError`] if the YAML is malformed, exceeds the 1 MB
/// size limit, or contains invalid step definitions.
pub fn parse_recipe(yaml_content: &str) -> Result<Recipe, parser::ParseError> {
    debug!("parse_recipe: yaml_content length={}", yaml_content.len());
    RecipeParser::new().parse(yaml_content)
}

/// Parse and execute a recipe from a YAML string in one call.
///
/// Parses the recipe, resolves any `extends` inheritance, and executes it
/// using the provided adapter. Optional `user_context` values override recipe defaults.
///
/// # Errors
///
/// Returns [`parser::ParseError`] if parsing or extends resolution fails.
pub fn run_recipe<A: Adapter>(
    yaml_content: &str,
    adapter: A,
    user_context: Option<HashMap<String, Value>>,
    dry_run: bool,
) -> Result<RecipeResult, parser::ParseError> {
    info!("run_recipe: dry_run={}", dry_run);
    let mut recipe = parse_recipe(yaml_content)?;
    resolve_extends(&mut recipe, &[])?;
    let runner = RecipeRunner::new(adapter).with_dry_run(dry_run);
    Ok(runner.execute(&recipe, user_context))
}

/// Find a recipe by name across standard search directories, parse it, and execute it.
///
/// Searches `~/.amplihack/recipes/`, `./recipes/`, and `./` for a recipe file
/// matching `name` (with `.yaml` or `.yml` extension).
///
/// # Errors
///
/// Returns an error if the recipe is not found, cannot be parsed, or extends
/// resolution fails.
pub fn run_recipe_by_name<A: Adapter>(
    name: &str,
    adapter: A,
    user_context: Option<HashMap<String, Value>>,
    dry_run: bool,
) -> Result<RecipeResult, Box<dyn std::error::Error>> {
    info!("run_recipe_by_name: name={:?}, dry_run={}", name, dry_run);
    let path = discovery::find_recipe(name, None)
        .ok_or_else(|| format!("Recipe '{}' not found in any search directory", name))?;
    let mut recipe = RecipeParser::new().parse_file(&path)?;
    resolve_extends(&mut recipe, &[])?;
    let runner = RecipeRunner::new(adapter).with_dry_run(dry_run);
    Ok(runner.execute(&recipe, user_context))
}

/// Validate a recipe and return any warnings.
///
/// Parses the YAML and checks for structural issues such as unrecognized fields,
/// duplicate step IDs, or unreferenced output variables.
///
/// # Errors
///
/// Returns [`parser::ParseError`] if the YAML cannot be parsed at all.
pub fn validate_recipe(yaml_content: &str) -> Result<Vec<String>, parser::ParseError> {
    debug!("validate_recipe: yaml_content length={}", yaml_content.len());
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml_content)?;
    Ok(parser.validate_with_yaml(&recipe, Some(yaml_content)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── safe_truncate ───────────────────────────────────────────

    #[test]
    fn truncate_empty_string() {
        assert_eq!(safe_truncate("", 10), "");
    }

    #[test]
    fn truncate_ascii_shorter_than_max() {
        assert_eq!(safe_truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_ascii_longer_than_max() {
        assert_eq!(safe_truncate("hello world", 5), "hello");
    }

    #[test]
    fn truncate_multibyte_at_boundary() {
        // '🦀' is 4 bytes; cutting at byte 5 would split the second emoji
        let s = "🦀🦀🦀";
        assert_eq!(safe_truncate(s, 5), "🦀"); // can't fit partial second emoji
        assert_eq!(safe_truncate(s, 8), "🦀🦀");
    }

    #[test]
    fn truncate_cjk_at_boundary() {
        // '漢' is 3 bytes
        let s = "漢字テスト";
        assert_eq!(safe_truncate(s, 4), "漢"); // 4 bytes can't fit 2nd char (need 6)
        assert_eq!(safe_truncate(s, 6), "漢字");
    }

    #[test]
    fn truncate_max_zero() {
        assert_eq!(safe_truncate("hello", 0), "");
        assert_eq!(safe_truncate("🦀", 0), "");
    }

    #[test]
    fn truncate_single_multibyte_char() {
        assert_eq!(safe_truncate("🦀", 4), "🦀");
        assert_eq!(safe_truncate("🦀", 3), ""); // can't fit partial emoji
    }

    // ── safe_tail ───────────────────────────────────────────────

    #[test]
    fn tail_empty_string() {
        assert_eq!(safe_tail("", 10), "");
    }

    #[test]
    fn tail_ascii_shorter_than_max() {
        assert_eq!(safe_tail("hello", 10), "hello");
    }

    #[test]
    fn tail_ascii_longer_than_max() {
        assert_eq!(safe_tail("hello world", 5), "world");
    }

    #[test]
    fn tail_multibyte_at_boundary() {
        let s = "🦀🦀🦀"; // 12 bytes total
        assert_eq!(safe_tail(s, 5), "🦀"); // start at 12-5=7, adjust to char boundary -> 8
        assert_eq!(safe_tail(s, 8), "🦀🦀");
    }

    #[test]
    fn tail_cjk_at_boundary() {
        let s = "漢字テスト"; // 15 bytes
        assert_eq!(safe_tail(s, 4), "ト"); // 15-4=11, next boundary -> 12
        assert_eq!(safe_tail(s, 6), "スト");
    }

    #[test]
    fn tail_max_zero() {
        assert_eq!(safe_tail("hello", 0), "");
        assert_eq!(safe_tail("🦀", 0), "");
    }

    #[test]
    fn tail_single_multibyte_char() {
        assert_eq!(safe_tail("🦀", 4), "🦀");
        assert_eq!(safe_tail("🦀", 3), ""); // can't fit partial emoji from the tail side
    }
}
