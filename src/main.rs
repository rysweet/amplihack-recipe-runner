/// amplihack Recipe Runner (Rust)
///
/// CLI interface for parsing and executing YAML-defined recipes.
/// Port from Python `amplihack.recipes`.
use clap::Parser;
use recipe_runner_rs::adapters::cli_subprocess::CLISubprocessAdapter;
use recipe_runner_rs::parser::RecipeParser;
use recipe_runner_rs::runner::RecipeRunner;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "recipe-runner", version, about = "Execute amplihack YAML recipes")]
struct Cli {
    /// Path to the recipe YAML file
    recipe: PathBuf,

    /// Working directory for execution
    #[arg(short = 'C', long, default_value = ".")]
    working_dir: String,

    /// Context overrides as key=value pairs
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
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let cli = Cli::parse();

    // Parse context overrides
    let mut user_context: HashMap<String, Value> = HashMap::new();
    for pair in &cli.context {
        if let Some((key, val)) = pair.split_once('=') {
            user_context.insert(key.to_string(), Value::String(val.to_string()));
        } else {
            eprintln!("Warning: ignoring malformed context override: {}", pair);
        }
    }

    // Parse recipe
    let parser = RecipeParser::new();
    let recipe = parser.parse_file(&cli.recipe)?;
    println!("Recipe: {} (v{})", recipe.name, recipe.version);
    println!("Steps: {}", recipe.steps.len());

    // Build runner
    let adapter = CLISubprocessAdapter::new();
    let runner = RecipeRunner::new(adapter)
        .with_working_dir(&cli.working_dir)
        .with_dry_run(cli.dry_run)
        .with_auto_stage(!cli.no_auto_stage)
        .with_recipe_search_dirs(cli.recipe_dirs);

    // Execute
    let ctx = if user_context.is_empty() {
        None
    } else {
        Some(user_context)
    };
    let result = runner.execute(&recipe, ctx);

    // Print result
    println!("\n{}", result);

    if result.success {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}
