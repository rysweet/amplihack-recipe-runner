/// amplihack Recipe Runner (Rust)
///
/// CLI interface for parsing and executing YAML-defined recipes.
///
use clap::{Parser, Subcommand};
use log::{debug, info};
use recipe_runner_rs::adapters::cli_subprocess::{CLISubprocessAdapter, TimeoutAction};
use recipe_runner_rs::agent_resolver::AgentResolver;
use recipe_runner_rs::discovery;
use recipe_runner_rs::models::diagnostic_identifier;
use recipe_runner_rs::parser::RecipeParser;
use recipe_runner_rs::runner::{FileLogListener, RecipeRunner, StderrListener};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

mod update;

/// Exit codes for structured error reporting.
mod exit_codes {
    /// Recipe executed successfully.
    pub const SUCCESS: i32 = 0;
    /// One or more recipe steps failed during execution.
    pub const RECIPE_FAILED: i32 = 1;
    /// Recipe YAML could not be parsed or is invalid.
    pub const PARSE_ERROR: i32 = 2;
    /// Recipe file or name could not be found.
    pub const NOT_FOUND: i32 = 3;
    /// Invalid CLI arguments or context overrides.
    pub const BAD_ARGS: i32 = 4;
}

/// What to do when a step's timeout is reached.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
enum TimeoutActionArg {
    /// Kill the child and fail the step.
    #[default]
    Kill,
    /// Report the overrun once and keep waiting; nothing is killed.
    Report,
}

#[derive(Parser)]
#[command(
    name = "recipe-runner",
    version,
    about = "Execute amplihack YAML recipes"
)]
struct Cli {
    /// Report supported execution contracts without update or network activity.
    #[arg(long)]
    capabilities: bool,

    /// Private launch descriptor supplied by an admitting host.
    #[arg(long, hide = true)]
    recovery_descriptor: Option<PathBuf>,
    /// Bypass inherited replay in the current invocation subtree.
    #[arg(long)]
    fresh: bool,

    /// Make no update check at startup: no network request and no update
    /// cache file (the same as RECIPE_RUNNER_NO_UPDATE_CHECK=1 for one run).
    /// Accepted before or after a subcommand.
    #[arg(long, global = true)]
    no_update_check: bool,

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

    /// Directory to search for agent definitions before the default
    /// locations (can be specified multiple times)
    #[arg(long = "agent-dir", value_name = "DIR")]
    agent_dirs: Vec<String>,

    /// Find recipes, sub-recipes and agent definitions only in the
    /// directories given with --recipe-dir and --agent-dir. A sub-recipe or
    /// agent definition that is not found there fails its step.
    #[arg(long, global = true)]
    no_default_search: bool,

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

    /// Fail the run if the audit log cannot be opened (no step runs) or an
    /// entry cannot be written (that step fails and the run stops)
    #[arg(long, requires = "audit_dir")]
    require_audit: bool,

    /// Agent binary to use for agent steps (e.g., claude, copilot, codex).
    /// Falls back to AMPLIHACK_AGENT_BINARY env var, then "claude".
    #[arg(long)]
    agent_binary: Option<String>,

    /// What to do when an agent step's timeout, a shell step's timeout or a
    /// hook's deadline is reached: "kill" (default) kills the child and fails
    /// the step; "report" reports the overrun once on stderr and keeps
    /// waiting, killing nothing.
    #[arg(long, value_enum, default_value_t = TimeoutActionArg::Kill)]
    timeout_action: TimeoutActionArg,

    /// Keep agent output and stderr logs in this directory (created if
    /// missing, readable only by the owner).
    #[arg(long, value_name = "DIR")]
    log_dir: Option<PathBuf>,

    /// Run agent steps through the launcher at this absolute path instead of
    /// `amplihack`. Takes precedence over AMPLIHACK_LAUNCHER_BINARY.
    #[arg(long, value_name = "PATH")]
    launcher: Option<PathBuf>,

    /// Do not pass --allow-all-tools to the Copilot CLI (the same as
    /// AMPLIHACK_NO_ALLOW_ALL_TOOLS for one run).
    #[arg(long)]
    no_allow_all_tools: bool,

    /// Do not grant the working directory to the agent CLI with --add-dir.
    /// The directory that holds an overflow prompt file is still granted to
    /// CLIs that read the file through --add-dir (such as Copilot).
    #[arg(long)]
    no_add_dir: bool,

    /// Pass only the named parent environment variables to agent and shell
    /// children (can be specified multiple times). Without this option every
    /// parent variable is passed on. The variables the runner sets itself
    /// (HOME and PATH defaults, the session-tree variables and
    /// AMPLIHACK_AGENT_BINARY), a step's `env:` block and RECIPE_VAR_* are
    /// still passed.
    #[arg(long = "env-allow", value_name = "NAME")]
    env_allow: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// List all discoverable recipes
    List {
        /// Directory to search (can be specified multiple times)
        #[arg(short = 'R', long = "recipe-dir")]
        recipe_dirs: Vec<String>,
    },
    /// Update recipe-runner-rs to the latest version
    Update,
}

/// Parse a context value string, auto-detecting type.
fn parse_context_value(raw: &str) -> Value {
    debug!("parse_context_value: raw={:?}", raw);
    // Try JSON first (handles objects, arrays, booleans, numbers)
    if let Ok(v) = serde_json::from_str::<Value>(raw)
        && !v.is_string()
    {
        return v;
    }
    // Try boolean (capitalized only; lowercase is handled above by serde_json,
    // which parses "true"/"false" as Value::Bool).
    match raw {
        "True" => return Value::Bool(true),
        "False" => return Value::Bool(false),
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
    debug!("parse_context_pair: pair={:?}", pair);
    let (key, val) = pair.split_once('=')?;
    Some((key.to_string(), parse_context_value(val)))
}

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.iter().skip(1).any(|arg| arg == "--capabilities") {
        if args.len() != 2 {
            eprintln!("--capabilities must be used alone");
            std::process::exit(2);
        }
        use std::io::Write;
        let capabilities: &[&str] = if cfg!(unix) {
            &["codex_exec", "durable_recovery_v1"]
        } else {
            &[]
        };
        let report = serde_json::json!({"schema_version":1,"version":env!("CARGO_PKG_VERSION"),"capabilities":capabilities});
        if let Err(error) = writeln!(std::io::stdout().lock(), "{report}") {
            eprintln!("Failed to write capability report: {error}");
            std::process::exit(1);
        }
        return;
    }
    env_logger::init();
    std::process::exit(run());
}

fn run() -> i32 {
    // Non-blocking startup update check (respects 24h cooldown)
    update::maybe_print_update_notice_from_args(&std::env::args_os().collect::<Vec<_>>());

    let cli = Cli::parse();
    debug!(
        "run: starting recipe runner (no_update_check={})",
        cli.no_update_check
    );

    // Handle subcommands
    if let Some(Commands::Update) = &cli.command {
        if let Err(e) = update::run_update() {
            eprintln!("Update failed: {e}");
            return exit_codes::RECIPE_FAILED;
        }
        return exit_codes::SUCCESS;
    }

    if let Some(Commands::List { recipe_dirs }) = &cli.command {
        let dirs: Vec<PathBuf> = recipe_dirs.iter().map(PathBuf::from).collect();
        let search = if dirs.is_empty() && !cli.no_default_search {
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
        return exit_codes::SUCCESS;
    }

    // Require recipe path for all other operations
    let recipe_path = match cli.recipe {
        Some(p) => p,
        None => {
            eprintln!(
                "Error: Recipe path is required. Use `recipe-runner <path>` or `recipe-runner list`."
            );
            return exit_codes::BAD_ARGS;
        }
    };

    // A relative --launcher is a bad argument whether or not the recipe runs,
    // so --validate-only reports it too.
    if let Some(ref launcher) = cli.launcher
        && !launcher.is_absolute()
    {
        eprintln!(
            "Error: --launcher must be an absolute path, got '{}'.",
            launcher.display()
        );
        return exit_codes::BAD_ARGS;
    }

    // Parse context overrides with type detection
    let mut user_context: HashMap<String, Value> = HashMap::new();
    for pair in &cli.context {
        if let Some((key, val)) = parse_context_pair(pair) {
            user_context.insert(key, val);
        } else {
            eprintln!("Warning: ignoring malformed context override: {}", pair);
        }
    }

    // Parse recipe — try as file path first, then as recipe name
    info!("run: parsing recipe from {:?}", recipe_path);
    // `--validate-only` reports step-field warnings itself via
    // `validate_with_yaml`; parsing must not print them a second time.
    let parser = if cli.validate_only {
        RecipeParser::new().without_field_warnings()
    } else {
        RecipeParser::new()
    };
    let resolved_path;
    let recipe = if recipe_path.is_file() {
        resolved_path = recipe_path;
        match parser.parse_file(&resolved_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error: Failed to parse recipe: {}", e);
                return exit_codes::PARSE_ERROR;
            }
        }
    } else {
        let name = recipe_path.to_string_lossy();
        let extra_dirs: Vec<PathBuf> = cli.recipe_dirs.iter().map(PathBuf::from).collect();
        let search = if extra_dirs.is_empty() && !cli.no_default_search {
            None
        } else {
            Some(extra_dirs.as_slice())
        };
        resolved_path = match discovery::find_recipe(&name, search) {
            Some(p) => p,
            None => {
                eprintln!(
                    "Error: Recipe '{}' not found. Use `recipe-runner list` to see available recipes.",
                    name
                );
                return exit_codes::NOT_FOUND;
            }
        };
        match parser.parse_file(&resolved_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error: Failed to parse recipe: {}", e);
                return exit_codes::PARSE_ERROR;
            }
        }
    };

    // --explain: show recipe structure
    if cli.explain {
        println!(
            "Recipe: {} (v{})",
            diagnostic_identifier(&recipe.name),
            recipe.version
        );
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
                recipe
                    .steps
                    .iter()
                    .position(|s| s.id == step.id)
                    .map(|p| p + 1)
                    .unwrap_or(0),
                ty.to_lowercase(),
                diagnostic_identifier(&step.id),
                cond,
                out,
                pj,
                coe
            );
        }
        return exit_codes::SUCCESS;
    }

    // --validate-only: parse + validate
    if cli.validate_only {
        let yaml_content = match std::fs::read_to_string(&resolved_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: Failed to read recipe file: {}", e);
                return exit_codes::PARSE_ERROR;
            }
        };
        let warnings = parser.validate_with_yaml(&recipe, Some(&yaml_content));
        if warnings.is_empty() {
            println!(
                "✓ Recipe '{}' is valid ({} steps)",
                diagnostic_identifier(&recipe.name),
                recipe.steps.len()
            );
        } else {
            println!(
                "⚠ Recipe '{}' has {} warning(s):",
                diagnostic_identifier(&recipe.name),
                warnings.len()
            );
            for w in &warnings {
                println!("  - {}", w);
            }
        }
        return exit_codes::SUCCESS;
    }

    if cli.output_format != "json" {
        println!(
            "Recipe: {} (v{})",
            diagnostic_identifier(&recipe.name),
            recipe.version
        );
        println!("Steps: {}", recipe.steps.len());
    }

    // Build runner
    let mut adapter = CLISubprocessAdapter::new();
    if let Some(ref binary) = cli.agent_binary {
        adapter = adapter.with_binary(binary);
    }
    if matches!(cli.timeout_action, TimeoutActionArg::Report) {
        adapter = adapter.with_timeout_action(TimeoutAction::Report);
    }
    if let Some(ref dir) = cli.log_dir {
        adapter = adapter.with_log_dir(dir.clone());
    }
    if let Some(ref launcher) = cli.launcher {
        adapter = adapter.with_launcher(launcher.clone());
    }
    if cli.no_allow_all_tools {
        adapter = adapter.without_allow_all_tools();
    }
    if cli.no_add_dir {
        adapter = adapter.without_working_dir_grant();
    }
    if !cli.env_allow.is_empty() {
        adapter = adapter.with_env_allow(cli.env_allow.iter().cloned());
    }
    #[cfg(unix)]
    let recovery_settings = recipe_runner_rs::recovery::model::fingerprint(&serde_json::json!({
        "agent_binary":cli.agent_binary,"launcher":cli.launcher,"allow_all_tools":!cli.no_allow_all_tools,
        "working_dir_grant":!cli.no_add_dir,"env_allow":cli.env_allow,"timeout_action":format!("{:?}",cli.timeout_action as u8),
        "step_timeout":std::env::var("AMPLIHACK_STEP_TIMEOUT").ok(),"require_audit":cli.require_audit,
        "engine_version":env!("CARGO_PKG_VERSION"),
        "execution_environment":(["AMPLIHACK_LAUNCHER_BINARY","AMPLIHACK_AGENT_BINARY","AMPLIHACK_BASH","AMPLIHACK_NO_ALLOW_ALL_TOOLS","AMPLIHACK_HOME","AMPLIHACK_CLAUDE_PROFILE","AMPLIHACK_CLAUDE_PROFILE_ROOT","AMPLIHACK_MAX_DEPTH","AMPLIHACK_MAX_SESSIONS"].map(|k|(k,std::env::var(k).ok()))),
        "agent_dirs":cli.agent_dirs,"recipe_dirs":cli.recipe_dirs,"no_default_search":cli.no_default_search,
    })).expect("serializable execution settings");
    #[cfg(unix)]
    let recovery = if !cli.dry_run {
        (|| -> anyhow::Result<_> {
            use recipe_runner_rs::recovery::{
                ipc::{Client, Server},
                model::Descriptor,
                private_fs::{Directory, parse},
                store::Store,
            };
            let inherited = Client::from_environment()?;
            if let Some(path) = &cli.recovery_descriptor {
                anyhow::ensure!(
                    inherited.is_none(),
                    "root recovery descriptor conflicts with delegated attachment"
                );
                let parent = Directory::open(
                    path.parent()
                        .ok_or_else(|| anyhow::anyhow!("invalid recovery descriptor path"))?,
                    false,
                )?;
                let d: Descriptor = parse(
                    &parent.read(
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .ok_or_else(|| anyhow::anyhow!("invalid descriptor name"))?,
                    )?,
                )?;
                let settings = recipe_runner_rs::recovery::model::fingerprint(&(
                    &d.settings,
                    &recovery_settings,
                ))?;
                let server = Server::start(Store::open(d)?)?;
                let client = server.client();
                Ok((Some(server), Some(client), settings))
            } else {
                Ok((None, inherited, recovery_settings))
            }
        })()
    } else {
        Ok((None, None, recovery_settings))
    };
    #[cfg(unix)]
    let (recovery_server, recovery_client, recovery_settings) = match recovery {
        Ok(v) => v,
        Err(error) => {
            eprintln!("durable recovery admission failed: {error}");
            return exit_codes::RECIPE_FAILED;
        }
    };

    let mut runner = RecipeRunner::new(adapter)
        .with_working_dir(&cli.working_dir)
        .with_dry_run(cli.dry_run)
        .with_auto_stage(!cli.no_auto_stage)
        .with_recipe_search_dirs(cli.recipe_dirs.into_iter().map(PathBuf::from).collect())
        .with_default_search(!cli.no_default_search)
        .with_tags(cli.include_tags, cli.exclude_tags);

    #[cfg(unix)]
    let mut _delegated_lease = None;
    #[cfg(unix)]
    if let Some(client) = recovery_client {
        let client = if client.prefix.is_empty() {
            client
        } else {
            let identity: std::collections::BTreeMap<_, _> =
                ["issue_number", "workstream", "workstream_id", "iteration"]
                    .into_iter()
                    .filter_map(|k| {
                        user_context
                            .get(k)
                            .or_else(|| recipe.context.get(k))
                            .map(|v| (k, v))
                    })
                    .collect();
            let discriminator = recipe_runner_rs::recovery::model::fingerprint(&identity)
                .expect("serializable invocation identity");
            match client.enter_invocation(recipe.name.clone(), discriminator) {
                Ok(c) => c,
                Err(error) => {
                    eprintln!("delegated recovery attachment failed: {error}");
                    return exit_codes::RECIPE_FAILED;
                }
            }
        };
        if !client.prefix.is_empty() {
            _delegated_lease = Some(client.invocation_guard());
        }
        if cli.fresh
            && let Err(error) = client.fresh_subtree()
        {
            eprintln!("cannot start fresh recovery subtree: {error}");
            return exit_codes::RECIPE_FAILED;
        }
        runner = runner
            .with_recovery(client, recovery_settings)
            .with_recipe_file(resolved_path.clone());
    }

    if !cli.agent_dirs.is_empty() || cli.no_default_search {
        runner = runner.with_agent_resolver(AgentResolver::from_dirs(
            cli.agent_dirs.iter().map(PathBuf::from).collect(),
            !cli.no_default_search,
        ));
    }

    // Anchor sub-recipe resolution to the directory holding the top-level
    // recipe file. Without this, sub-recipes co-located with the parent
    // recipe are unfindable when the runner subprocess's cwd differs from
    // the invocation directory (issue rysweet/amplihack-rs#480).
    if let Some(parent) = resolved_path.parent()
        && !parent.as_os_str().is_empty()
    {
        runner = runner.with_recipe_origin_dir(parent.to_path_buf());
    }

    if let Some(ref audit_dir) = cli.audit_dir {
        runner = runner.with_audit_dir(audit_dir.clone());
    }
    runner = runner.with_require_audit(cli.require_audit);

    if cli.progress {
        // Use FileLogListener (writes structured JSON log + stderr) when available,
        // fall back to StderrListener if log file creation fails.
        match FileLogListener::new(&recipe.name) {
            Some((listener, _path)) => {
                runner = runner.with_listener(Box::new(listener));
            }
            None => {
                runner = runner.with_listener(Box::new(StderrListener));
            }
        }
    }

    // Execute
    let ctx = if user_context.is_empty() {
        None
    } else {
        Some(user_context)
    };
    info!(
        "run: executing recipe '{}' with {} steps",
        diagnostic_identifier(&recipe.name),
        recipe.steps.len()
    );
    let result = runner.execute(&recipe, ctx);

    #[cfg(unix)]
    let (recovery_report, failure_cause) = if let Some(server) = recovery_server {
        match server.finish_result(&result) {
            Ok((report, cause)) => (
                Some(report),
                cause.map(|origin| serde_json::json!({"class":"usage_limit","origin":origin})),
            ),
            Err(error) => {
                eprintln!("durable recovery final publication failed: {error}");
                return exit_codes::RECIPE_FAILED;
            }
        }
    } else {
        (None, None)
    };
    let diagnostics = runner.diagnostic_artifacts();
    // Keep CLI-only diagnostics outside RecipeResult: its Display is also used
    // for nested recipe output, and context is exported to subsequent steps.
    #[derive(serde::Serialize)]
    struct CliResult<'a> {
        #[serde(flatten)]
        result: &'a recipe_runner_rs::models::RecipeResult,
        #[serde(
            skip_serializing_if = "<[recipe_runner_rs::runner::DiagnosticArtifact]>::is_empty"
        )]
        diagnostic_artifacts: &'a [recipe_runner_rs::runner::DiagnosticArtifact],
        #[cfg(unix)]
        #[serde(skip_serializing_if = "Option::is_none")]
        recovery: &'a Option<recipe_runner_rs::recovery::model::Report>,
        #[cfg(unix)]
        #[serde(skip_serializing_if = "Option::is_none")]
        failure_cause: &'a Option<serde_json::Value>,
    }
    // Output
    if cli.output_format == "json" {
        match serde_json::to_string_pretty(&CliResult {
            result: &result,
            diagnostic_artifacts: &diagnostics,
            #[cfg(unix)]
            recovery: &recovery_report,
            #[cfg(unix)]
            failure_cause: &failure_cause,
        }) {
            Ok(json) => println!("{}", json),
            Err(e) => eprintln!("Error: Failed to serialize result: {}", e),
        }
    } else {
        println!("\n{}", result);
        for record in &diagnostics {
            println!("{}", record.diagnostic.text_reference());
        }
    }

    if result.success {
        exit_codes::SUCCESS
    } else {
        exit_codes::RECIPE_FAILED
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- RR-M7: parse_context_value unit tests --

    #[test]
    fn test_parse_context_value_bool_both_paths_after_dead_arm_removal() {
        // After removing the unreachable `"true"`/`"false"` arms, both
        // lowercase (handled by serde_json) and capitalized (handled by
        // the explicit match arm) must still produce Value::Bool.
        // Lowercase: takes the serde_json path (returns Value::Bool, !is_string()).
        assert_eq!(parse_context_value("true"), json!(true));
        assert_eq!(parse_context_value("false"), json!(false));
        // Capitalized: not valid JSON, falls through to the match arm.
        assert_eq!(parse_context_value("True"), json!(true));
        assert_eq!(parse_context_value("False"), json!(false));
        // Other capitalizations are NOT booleans (intentional, preserved).
        assert_eq!(parse_context_value("TRUE"), json!("TRUE"));
        assert_eq!(parse_context_value("tRuE"), json!("tRuE"));
    }

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
