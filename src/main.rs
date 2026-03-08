/// amplihack Recipe Runner (Rust)
///
/// CLI interface for parsing and executing YAML-defined recipes.
///
use clap::{Parser, Subcommand};
use recipe_runner_rs::adapters::cli_subprocess::CLISubprocessAdapter;
use recipe_runner_rs::discovery;
use recipe_runner_rs::parser::RecipeParser;
use recipe_runner_rs::runner::{RecipeRunner, StderrListener};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "recipe-runner",
    version,
    about = "Execute amplihack YAML recipes"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to the recipe YAML file
    recipe: Option<PathBuf>,

    /// Working directory for execution
    #[arg(short = 'C', long, default_value = ".")]
    working_dir: String,

    /// Context overrides as key=value pairs.
    /// Values are auto-detected: "true"→bool, "42"→number, else string.
    /// Use --set 'key={"a":1}' for JSON objects.
    #[arg(short, long = "set", value_name = "KEY=VALUE")]
    context: Vec<String>,

    /// Directory to search for sub-recipes (can be specified multiple times)
    #[arg(short = 'R', long = "recipe-dir")]
    recipe_dirs: Vec<String>,

    /// Dry run (log steps without executing)
    #[arg(long)]
    dry_run: bool,

    /// Disable auto-staging of git changes
    #[arg(long)]
    no_auto_stage: bool,

    /// Output format: "text" (default) or "json"
    #[arg(long, default_value = "text")]
    output_format: String,

    /// Validate recipe without executing
    #[arg(long)]
    validate_only: bool,

    /// Show recipe structure (steps, conditions, outputs)
    #[arg(long)]
    explain: bool,

    /// Show step-level progress on stderr
    #[arg(long)]
    progress: bool,

    /// Only run steps matching these tags (comma-separated)
    #[arg(long, value_delimiter = ',')]
    include_tags: Vec<String>,

    /// Skip steps matching these tags (comma-separated)
    #[arg(long, value_delimiter = ',')]
    exclude_tags: Vec<String>,

    /// Directory for JSONL audit logs
    #[arg(long)]
    audit_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// List all discoverable recipes
    List {
        /// Directory to search (can be specified multiple times)
        #[arg(short = 'R', long = "recipe-dir")]
        recipe_dirs: Vec<String>,
    },
}

/// Parse a context value string, auto-detecting type.
fn parse_context_value(raw: &str) -> Value {
    // Try JSON first (handles objects, arrays, booleans, numbers)
    if let Ok(v) = serde_json::from_str::<Value>(raw)
        && !v.is_string()
    {
        return v;
    }
    // Try boolean
    match raw {
        "true" | "True" => return Value::Bool(true),
        "false" | "False" => return Value::Bool(false),
        _ => {}
    }
    // Try integer
    if let Ok(n) = raw.parse::<i64>() {
        return Value::Number(serde_json::Number::from(n));
    }
    // Try float
    if let Ok(n) = raw.parse::<f64>()
        && let Some(num) = serde_json::Number::from_f64(n)
    {
        return Value::Number(num);
    }
    Value::String(raw.to_string())
}

/// Parse a `key=value` context pair, splitting on the first `=` only.
///
/// Returns `None` if there is no `=` (malformed pair).
fn parse_context_pair(pair: &str) -> Option<(String, Value)> {
    let (key, val) = pair.split_once('=')?;
    Some((key.to_string(), parse_context_value(val)))
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let cli = Cli::parse();

    // Handle subcommands
    if let Some(Commands::List { recipe_dirs }) = &cli.command {
        let dirs: Vec<PathBuf> = recipe_dirs.iter().map(PathBuf::from).collect();
        let search = if dirs.is_empty() {
            None
        } else {
            Some(dirs.as_slice())
        };
        let recipes = discovery::list_recipes(search);
        if recipes.is_empty() {
            println!("No recipes found.");
        } else {
            println!("{:<30} {:<10} DESCRIPTION", "NAME", "VERSION");
            println!("{}", "-".repeat(72));
            for r in &recipes {
                println!(
                    "{:<30} {:<10} {}",
                    r.name,
                    r.version,
                    recipe_runner_rs::safe_truncate(&r.description, 60)
                );
            }
            println!("\n{} recipe(s) found.", recipes.len());
        }
        return Ok(());
    }

    // Require recipe path for all other operations
    let recipe_path = cli.recipe.ok_or_else(|| {
        anyhow::anyhow!(
            "Recipe path is required. Use `recipe-runner <path>` or `recipe-runner list`."
        )
    })?;

    // Parse context overrides with type detection
    let mut user_context: HashMap<String, Value> = HashMap::new();
    for pair in &cli.context {
        if let Some((key, val)) = parse_context_pair(pair) {
            user_context.insert(key, val);
        } else {
            eprintln!("Warning: ignoring malformed context override: {}", pair);
        }
    }

    // Parse recipe
    let parser = RecipeParser::new();
    let recipe = parser.parse_file(&recipe_path)?;

    // --explain: show recipe structure
    if cli.explain {
        println!("Recipe: {} (v{})", recipe.name, recipe.version);
        if !recipe.description.is_empty() {
            println!("Description: {}", recipe.description);
        }
        if !recipe.author.is_empty() {
            println!("Author: {}", recipe.author);
        }
        if !recipe.tags.is_empty() {
            println!("Tags: {}", recipe.tags.join(", "));
        }
        println!(
            "Recursion: max_depth={}, max_total_steps={}",
            recipe.recursion.max_depth, recipe.recursion.max_total_steps
        );
        println!("\nContext defaults:");
        for (k, v) in &recipe.context {
            println!("  {}: {}", k, v);
        }
        println!("\nSteps ({}):", recipe.steps.len());
        for step in &recipe.steps {
            let ty = format!("{:?}", step.effective_type());
            let cond = step
                .condition
                .as_deref()
                .map(|c| format!(" [if {}]", c))
                .unwrap_or_default();
            let out = step
                .output
                .as_deref()
                .map(|o| format!(" → {}", o))
                .unwrap_or_default();
            let pj = if step.parse_json { " (parse_json)" } else { "" };
            let coe = if step.continue_on_error {
                " (continue_on_error)"
            } else {
                ""
            };
            println!(
                "  {:>3}. [{:<6}] {}{}{}{}{}",
                recipe.steps.iter().position(|s| s.id == step.id).unwrap() + 1,
                ty.to_lowercase(),
                step.id,
                cond,
                out,
                pj,
                coe
            );
        }
        return Ok(());
    }

    // --validate-only: parse + validate
    if cli.validate_only {
        let warnings =
            parser.validate_with_yaml(&recipe, Some(&std::fs::read_to_string(&recipe_path)?));
        if warnings.is_empty() {
            println!(
                "✓ Recipe '{}' is valid ({} steps)",
                recipe.name,
                recipe.steps.len()
            );
        } else {
            println!(
                "⚠ Recipe '{}' has {} warning(s):",
                recipe.name,
                warnings.len()
            );
            for w in &warnings {
                println!("  - {}", w);
            }
        }
        return Ok(());
    }

    if cli.output_format != "json" {
        println!("Recipe: {} (v{})", recipe.name, recipe.version);
        println!("Steps: {}", recipe.steps.len());
    }

    // Build runner
    let adapter = CLISubprocessAdapter::new();
    let mut runner = RecipeRunner::new(adapter)
        .with_working_dir(&cli.working_dir)
        .with_dry_run(cli.dry_run)
        .with_auto_stage(!cli.no_auto_stage)
        .with_recipe_search_dirs(cli.recipe_dirs.into_iter().map(PathBuf::from).collect())
        .with_tags(cli.include_tags, cli.exclude_tags);

    if let Some(ref audit_dir) = cli.audit_dir {
        runner = runner.with_audit_dir(audit_dir.clone());
    }

    if cli.progress {
        runner = runner.with_listener(Box::new(StderrListener));
    }

    // Execute
    let ctx = if user_context.is_empty() {
        None
    } else {
        Some(user_context)
    };
    let result = runner.execute(&recipe, ctx);

    // Output
    if cli.output_format == "json" {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("\n{}", result);
    }

    if result.success {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- RR-M7: parse_context_value unit tests --

    #[test]
    fn test_parse_context_value_string() {
        assert_eq!(parse_context_value("hello"), json!("hello"));
    }

    #[test]
    fn test_parse_context_value_empty_string() {
        assert_eq!(parse_context_value(""), json!(""));
    }

    #[test]
    fn test_parse_context_value_bool_true() {
        assert_eq!(parse_context_value("true"), json!(true));
        assert_eq!(parse_context_value("True"), json!(true));
    }

    #[test]
    fn test_parse_context_value_bool_false() {
        assert_eq!(parse_context_value("false"), json!(false));
        assert_eq!(parse_context_value("False"), json!(false));
    }

    #[test]
    fn test_parse_context_value_integer() {
        assert_eq!(parse_context_value("42"), json!(42));
        assert_eq!(parse_context_value("-7"), json!(-7));
        assert_eq!(parse_context_value("0"), json!(0));
    }

    #[test]
    fn test_parse_context_value_float() {
        assert_eq!(parse_context_value("1.23"), json!(1.23));
        assert_eq!(parse_context_value("-0.5"), json!(-0.5));
    }

    #[test]
    fn test_parse_context_value_json_object() {
        let val = parse_context_value(r#"{"a":1,"b":"two"}"#);
        assert_eq!(val, json!({"a": 1, "b": "two"}));
    }

    #[test]
    fn test_parse_context_value_json_array() {
        let val = parse_context_value(r#"[1,2,3]"#);
        assert_eq!(val, json!([1, 2, 3]));
    }

    #[test]
    fn test_parse_context_value_string_not_parsed_as_json_string() {
        // A quoted JSON string like `"hello"` should still become Value::String
        // because of the `!v.is_string()` guard.
        let val = parse_context_value(r#""hello""#);
        assert_eq!(val, json!("\"hello\""));
    }

    // -- parse_context_pair tests --

    #[test]
    fn test_parse_pair_simple() {
        let (k, v) = parse_context_pair("key=value").unwrap();
        assert_eq!(k, "key");
        assert_eq!(v, json!("value"));
    }

    #[test]
    fn test_parse_pair_value_with_equals() {
        let (k, v) = parse_context_pair("key=val=ue").unwrap();
        assert_eq!(k, "key");
        assert_eq!(v, json!("val=ue"));
    }

    #[test]
    fn test_parse_pair_empty_value() {
        let (k, v) = parse_context_pair("key=").unwrap();
        assert_eq!(k, "key");
        assert_eq!(v, json!(""));
    }

    #[test]
    fn test_parse_pair_no_equals_returns_none() {
        assert!(parse_context_pair("key").is_none());
    }

    #[test]
    fn test_parse_pair_empty_string_returns_none() {
        assert!(parse_context_pair("").is_none());
    }

    #[test]
    fn test_parse_pair_typed_value() {
        let (_, v) = parse_context_pair("count=42").unwrap();
        assert_eq!(v, json!(42));

        let (_, v) = parse_context_pair("flag=true").unwrap();
        assert_eq!(v, json!(true));
    }
}
