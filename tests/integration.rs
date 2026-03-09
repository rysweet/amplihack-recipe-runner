/// Integration tests for recipe-runner-rs.
///
/// Tests cross-module interactions: parsing → context → runner → adapters.
/// Validates parity with the Python recipe runner behavior.
use recipe_runner_rs::adapters::Adapter;
use recipe_runner_rs::agent_resolver::AgentResolver;
use recipe_runner_rs::context::RecipeContext;
use recipe_runner_rs::discovery;
use recipe_runner_rs::models::{StepStatus, StepType};
use recipe_runner_rs::parser::RecipeParser;
use recipe_runner_rs::runner::RecipeRunner;
use serde_json::json;
use std::collections::HashMap;

// -- Mock adapter for integration tests --

struct MockAdapter {
    responses: HashMap<String, String>,
}

impl MockAdapter {
    fn new() -> Self {
        Self {
            responses: HashMap::new(),
        }
    }

    fn with_response(mut self, pattern: &str, response: &str) -> Self {
        self.responses
            .insert(pattern.to_string(), response.to_string());
        self
    }
}

impl Adapter for MockAdapter {
    fn execute_agent_step(
        &self,
        prompt: &str,
        _agent_name: Option<&str>,
        _system_prompt: Option<&str>,
        _mode: Option<&str>,
        _working_dir: &str,
        _model: Option<&str>,
    ) -> Result<String, anyhow::Error> {
        // Find matching response
        for (pattern, response) in &self.responses {
            if prompt.contains(pattern) {
                return Ok(response.clone());
            }
        }
        Ok(format!(
            "Mock response for: {}",
            &prompt[..prompt.len().min(50)]
        ))
    }

    fn execute_bash_step(
        &self,
        command: &str,
        _working_dir: &str,
        _timeout: Option<u64>,
        _extra_env: &std::collections::HashMap<String, String>,
    ) -> Result<String, anyhow::Error> {
        for (pattern, response) in &self.responses {
            if command.contains(pattern) {
                return Ok(response.clone());
            }
        }
        Ok(format!("Bash: {}", command))
    }

    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "mock"
    }
}

// -- Integration tests --

#[test]
fn test_full_recipe_lifecycle() {
    let yaml = r#"
name: "integration-test"
description: "Full lifecycle integration test"
version: "1.0.0"
context:
  project_name: "my-project"
  status: "NOT_STARTED"
steps:
  - id: "setup"
    command: "echo Setting up {{project_name}}"
    output: "setup_result"

  - id: "analyze"
    agent: "amplihack:core:architect"
    prompt: "Analyze the project {{project_name}}"
    output: "analysis"

  - id: "check-status"
    command: "echo done"
    condition: "status == 'NOT_STARTED'"
    output: "check_result"

  - id: "skip-if-done"
    command: "echo should not run"
    condition: "status == 'DONE'"
"#;
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();

    // Validate
    let warnings = parser.validate(&recipe);
    assert!(warnings.is_empty(), "unexpected warnings: {:?}", warnings);

    // Execute
    let adapter = MockAdapter::new();
    let runner = RecipeRunner::new(adapter);
    let result = runner.execute(&recipe, None);

    assert!(result.success);
    assert_eq!(result.step_results.len(), 4);
    assert_eq!(result.step_results[0].status, StepStatus::Completed);
    assert_eq!(result.step_results[1].status, StepStatus::Completed);
    assert_eq!(result.step_results[2].status, StepStatus::Completed);
    assert_eq!(result.step_results[3].status, StepStatus::Skipped);
}

#[test]
fn test_parse_json_with_retry_via_mock() {
    let yaml = r#"
name: "json-test"
steps:
  - id: "get-json"
    agent: "amplihack:core:analyzer"
    prompt: "Return JSON analysis"
    parse_json: true
    output: "analysis"
"#;
    let adapter =
        MockAdapter::new().with_response("Return JSON", r#"{"status": "ok", "items": [1, 2, 3]}"#);

    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    let runner = RecipeRunner::new(adapter);
    let result = runner.execute(&recipe, None);

    assert!(result.success);
    // Output should be stored as parsed JSON
    let analysis = result.context.get("analysis").unwrap();
    assert_eq!(analysis, &json!({"status": "ok", "items": [1, 2, 3]}));
}

#[test]
fn test_json_extraction_from_markdown_fence() {
    let yaml = r#"
name: "fence-test"
steps:
  - id: "fenced"
    agent: "amplihack:core:analyzer"
    prompt: "fenced response"
    parse_json: true
    output: "data"
"#;
    let fenced_response = "Here's the analysis:\n```json\n{\"result\": \"success\"}\n```\nDone.";
    let adapter = MockAdapter::new().with_response("fenced", fenced_response);

    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    let runner = RecipeRunner::new(adapter);
    let result = runner.execute(&recipe, None);

    assert!(result.success);
    let data = result.context.get("data").unwrap();
    assert_eq!(data, &json!({"result": "success"}));
}

#[test]
fn test_context_flows_between_steps() {
    let yaml = r#"
name: "context-flow"
context:
  task: "build"
steps:
  - id: "step1"
    command: "echo {{task}}"
    output: "result1"
  - id: "step2"
    command: "echo previous was {{result1}}"
    output: "result2"
"#;
    let adapter = MockAdapter::new()
        .with_response("build", "build-complete")
        .with_response("previous", "chained-output");

    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    let runner = RecipeRunner::new(adapter);
    let result = runner.execute(&recipe, None);

    assert!(result.success);
    assert!(result.context.contains_key("result1"));
    assert!(result.context.contains_key("result2"));
}

#[test]
fn test_user_context_overrides_recipe_defaults() {
    let yaml = r#"
name: "override-test"
context:
  greeting: "hello"
steps:
  - id: "greet"
    command: "echo {{greeting}}"
    output: "out"
"#;
    let mut user_ctx = HashMap::new();
    user_ctx.insert("greeting".to_string(), json!("howdy"));

    let adapter = MockAdapter::new().with_response("howdy", "howdy-response");
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    let runner = RecipeRunner::new(adapter);
    let result = runner.execute(&recipe, Some(user_ctx));

    assert!(result.success);
}

#[test]
fn test_sub_recipe_execution() {
    let tmp = tempfile::tempdir().unwrap();

    // Create child recipe
    let child_yaml = r#"
name: "child-recipe"
steps:
  - id: "child-step"
    command: "echo child running with {{parent_val}}"
    output: "child_out"
"#;
    std::fs::write(tmp.path().join("child-recipe.yaml"), child_yaml).unwrap();

    // Parent recipe
    let parent_yaml = r#"
name: "parent-recipe"
context:
  parent_val: "from-parent"
steps:
  - id: "run-child"
    type: "recipe"
    recipe: "child-recipe"
    context:
      parent_val: "{{parent_val}}"
"#;
    let adapter = MockAdapter::new();
    let parser = RecipeParser::new();
    let recipe = parser.parse(parent_yaml).unwrap();
    let runner = RecipeRunner::new(adapter).with_recipe_search_dirs(vec![tmp.path().to_path_buf()]);
    let result = runner.execute(&recipe, None);

    assert!(result.success, "Sub-recipe execution failed: {:?}", result);
}

#[test]
fn test_condition_with_function_calls() {
    let yaml = r#"
name: "function-condition"
context:
  items: "one,two,three"
steps:
  - id: "len-check"
    command: "echo has items"
    condition: "len(items) > 0"
    output: "out1"
  - id: "strip-check"
    command: "echo stripped"
    condition: "items.strip() == 'one,two,three'"
    output: "out2"
"#;
    let adapter = MockAdapter::new();
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    let runner = RecipeRunner::new(adapter);
    let result = runner.execute(&recipe, None);

    assert!(result.success);
    assert_eq!(result.step_results[0].status, StepStatus::Completed);
    assert_eq!(result.step_results[1].status, StepStatus::Completed);
}

#[test]
fn test_dry_run_with_parse_json() {
    let yaml = r#"
name: "dry-json"
steps:
  - id: "json-step"
    agent: "amplihack:core:analyzer"
    prompt: "analyze"
    parse_json: true
    output: "data"
"#;
    let adapter = MockAdapter::new();
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    let runner = RecipeRunner::new(adapter).with_dry_run(true);
    let result = runner.execute(&recipe, None);

    assert!(result.success);
    assert_eq!(result.step_results[0].status, StepStatus::Skipped);
}

#[test]
fn test_discovery_and_manifest() {
    let tmp = tempfile::tempdir().unwrap();

    // Create recipe files
    std::fs::write(
        tmp.path().join("recipe-alpha.yaml"),
        "name: recipe-alpha\ndescription: First\nversion: '2.0.0'\nsteps:\n  - id: s1\n    command: echo a\n  - id: s2\n    command: echo b\n",
    ).unwrap();
    std::fs::write(
        tmp.path().join("recipe-beta.yaml"),
        "name: recipe-beta\ntags:\n  - test\n  - demo\nsteps:\n  - id: s1\n    command: echo b\n",
    )
    .unwrap();

    // Discover
    let recipes = discovery::discover_recipes(Some(&[tmp.path().to_path_buf()]));
    assert_eq!(recipes.len(), 2);
    assert!(recipes.contains_key("recipe-alpha"));
    assert!(recipes.contains_key("recipe-beta"));
    assert_eq!(recipes["recipe-alpha"].step_count, 2);
    assert_eq!(recipes["recipe-alpha"].version, "2.0.0");
    assert_eq!(recipes["recipe-beta"].tags, vec!["test", "demo"]);

    // List (sorted)
    let list = discovery::list_recipes(Some(&[tmp.path().to_path_buf()]));
    assert_eq!(list[0].name, "recipe-alpha");
    assert_eq!(list[1].name, "recipe-beta");

    // Find
    assert!(discovery::find_recipe("recipe-alpha", Some(&[tmp.path().to_path_buf()])).is_some());
    assert!(discovery::find_recipe("nonexistent", Some(&[tmp.path().to_path_buf()])).is_none());

    // Manifest
    let manifest_path = discovery::update_manifest(Some(tmp.path())).unwrap();
    assert!(manifest_path.is_file());

    // No changes after creating manifest
    let changes = discovery::check_upstream_changes(Some(tmp.path()));
    assert!(
        changes.is_empty(),
        "Expected no changes, got: {:?}",
        changes
    );
}

#[test]
fn test_agent_resolver_integration() {
    let tmp = tempfile::tempdir().unwrap();
    let agent_dir = tmp.path().join("amplihack").join("specialized");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("optimizer.md"),
        "# Optimizer Agent\n\nYou optimize code for performance.",
    )
    .unwrap();

    let resolver = AgentResolver::new(Some(vec![tmp.path().to_path_buf()]));

    // 2-part ref
    let content = resolver.resolve("amplihack:optimizer").unwrap();
    assert!(content.contains("Optimizer Agent"));

    // 3-part ref
    let content2 = resolver.resolve("amplihack:specialized:optimizer").unwrap();
    assert!(content2.contains("optimize code"));
}

#[test]
fn test_validate_with_yaml_catches_typos() {
    let yaml = r#"
name: "typo-recipe"
descrption: "oops"
versoin: "1.0"
steps:
  - id: "step1"
    comand: "echo hello"
    prarse_json: true
"#;
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    let warnings = parser.validate_with_yaml(&recipe, Some(yaml));

    // Should detect all 4 typos
    assert!(warnings.iter().any(|w| w.contains("descrption")));
    assert!(warnings.iter().any(|w| w.contains("versoin")));
    assert!(warnings.iter().any(|w| w.contains("comand")));
    assert!(warnings.iter().any(|w| w.contains("prarse_json")));
}

#[test]
fn test_step_type_inference() {
    let yaml = r#"
name: "inference-test"
steps:
  - id: "bash-explicit"
    type: "bash"
    command: "echo hello"
  - id: "agent-by-field"
    agent: "amplihack:builder"
    prompt: "do thing"
  - id: "agent-by-prompt"
    prompt: "analyze this"
  - id: "bash-by-command"
    command: "ls"
  - id: "recipe-by-type"
    type: "recipe"
    recipe: "sub-recipe"
  - id: "recipe-by-field"
    recipe: "other-recipe"
  - id: "default-bash"
    command: "echo default"
"#;
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    assert_eq!(recipe.steps[0].effective_type(), StepType::Bash);
    assert_eq!(recipe.steps[1].effective_type(), StepType::Agent);
    assert_eq!(recipe.steps[2].effective_type(), StepType::Agent);
    assert_eq!(recipe.steps[3].effective_type(), StepType::Bash);
    assert_eq!(recipe.steps[4].effective_type(), StepType::Recipe);
    assert_eq!(recipe.steps[5].effective_type(), StepType::Recipe);
    assert_eq!(recipe.steps[6].effective_type(), StepType::Bash);
}

#[test]
fn test_condition_evaluator_comprehensive() {
    let mut data = HashMap::new();
    data.insert("status".to_string(), json!("CONVERGED"));
    data.insert("count".to_string(), json!(42));
    data.insert("text".to_string(), json!("  Hello World  "));
    data.insert("empty".to_string(), json!(""));
    data.insert("items".to_string(), json!(["a", "b", "c"]));
    let ctx = RecipeContext::new(data);

    // Comparisons
    assert!(ctx.evaluate("status == 'CONVERGED'").unwrap());
    assert!(ctx.evaluate("status != 'OTHER'").unwrap());
    assert!(ctx.evaluate("count == 42").unwrap());
    assert!(ctx.evaluate("count > 10").unwrap());
    assert!(ctx.evaluate("count < 100").unwrap());
    assert!(ctx.evaluate("count >= 42").unwrap());
    assert!(ctx.evaluate("count <= 42").unwrap());

    // Boolean ops
    assert!(
        ctx.evaluate("status == 'CONVERGED' and count == 42")
            .unwrap()
    );
    assert!(ctx.evaluate("status == 'WRONG' or count == 42").unwrap());
    assert!(ctx.evaluate("not empty").unwrap());

    // Containment
    assert!(ctx.evaluate("'World' in text").unwrap());
    assert!(ctx.evaluate("'xyz' not in text").unwrap());

    // Functions
    assert!(ctx.evaluate("len(text) > 0").unwrap());
    assert!(ctx.evaluate("int(count) == 42").unwrap());
    assert!(ctx.evaluate("str(count) == '42'").unwrap());

    // Methods
    assert!(ctx.evaluate("text.strip() == 'Hello World'").unwrap());
    assert!(ctx.evaluate("text.lower() == '  hello world  '").unwrap());
    assert!(ctx.evaluate("text.upper() == '  HELLO WORLD  '").unwrap());
    assert!(ctx.evaluate("text.startswith('  Hello')").unwrap());
    assert!(ctx.evaluate("text.endswith('ld  ')").unwrap());

    // Safety
    assert!(ctx.evaluate("__import__('os')").is_err());
    assert!(ctx.evaluate("text.system()").is_err());
}

#[test]
fn test_fail_fast_on_step_failure() {
    let yaml = r#"
name: "fail-test"
steps:
  - id: "good"
    command: "echo ok"
  - id: "bad"
    command: "fail-command"
  - id: "never"
    command: "echo unreachable"
"#;
    struct FailAdapter;
    impl Adapter for FailAdapter {
        fn execute_agent_step(
            &self,
            _p: &str,
            _a: Option<&str>,
            _s: Option<&str>,
            _m: Option<&str>,
            _w: &str,
            _model: Option<&str>,
        ) -> Result<String, anyhow::Error> {
            Ok("ok".to_string())
        }
        fn execute_bash_step(
            &self,
            cmd: &str,
            _w: &str,
            _t: Option<u64>,
            _extra_env: &std::collections::HashMap<String, String>,
        ) -> Result<String, anyhow::Error> {
            if cmd.contains("fail") {
                anyhow::bail!("command failed")
            }
            Ok("ok".to_string())
        }
        fn is_available(&self) -> bool {
            true
        }
        fn name(&self) -> &str {
            "fail-mock"
        }
    }

    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    let runner = RecipeRunner::new(FailAdapter);
    let result = runner.execute(&recipe, None);

    assert!(!result.success);
    assert_eq!(result.step_results.len(), 2); // stopped at failure
    assert_eq!(result.step_results[0].status, StepStatus::Completed);
    assert_eq!(result.step_results[1].status, StepStatus::Failed);
}

// -- RR-H10: run_recipe() and run_recipe_by_name() integration tests --

#[test]
fn test_run_recipe_valid_bash_only() {
    let yaml = r#"
name: "bash-only"
steps:
  - id: "echo-hello"
    command: "echo hello"
  - id: "echo-world"
    command: "echo world"
"#;
    let adapter = MockAdapter::new()
        .with_response("echo hello", "hello")
        .with_response("echo world", "world");
    let result = recipe_runner_rs::run_recipe(yaml, adapter, None, false).unwrap();
    assert!(result.success);
    assert_eq!(result.step_results.len(), 2);
    assert_eq!(result.step_results[0].status, StepStatus::Completed);
    assert_eq!(result.step_results[1].status, StepStatus::Completed);
}

#[test]
fn test_run_recipe_invalid_yaml_errors() {
    let bad_yaml = "not: valid: yaml: [[[";
    let adapter = MockAdapter::new();
    let result = recipe_runner_rs::run_recipe(bad_yaml, adapter, None, false);
    assert!(result.is_err());
}

#[test]
fn test_run_recipe_empty_name_errors() {
    let yaml = r#"
name: ""
steps:
  - id: "s1"
    command: "echo hi"
"#;
    let adapter = MockAdapter::new();
    let result = recipe_runner_rs::run_recipe(yaml, adapter, None, false);
    assert!(result.is_err());
}

#[test]
fn test_run_recipe_dry_run() {
    let yaml = r#"
name: "dry-test"
steps:
  - id: "step1"
    command: "echo dry"
"#;
    let adapter = MockAdapter::new();
    let result = recipe_runner_rs::run_recipe(yaml, adapter, None, true).unwrap();
    assert!(result.success);
    assert_eq!(result.step_results[0].status, StepStatus::Skipped);
}

#[test]
fn test_run_recipe_by_name_valid() {
    let tmp = tempfile::tempdir().unwrap();
    let recipe_yaml = r#"
name: "my-recipe"
steps:
  - id: "s1"
    command: "echo from recipe"
"#;
    std::fs::write(tmp.path().join("my-recipe.yaml"), recipe_yaml).unwrap();

    // Set env var so discovery finds the recipe
    let adapter = MockAdapter::new().with_response("echo from recipe", "done");
    // Point default discovery at the temp dir so run_recipe_by_name finds the recipe.
    // SAFETY: This test is single-threaded and restores the var immediately after use.
    unsafe {
        std::env::set_var("RECIPE_RUNNER_RECIPE_DIRS", tmp.path().to_str().unwrap());
    }
    let result = recipe_runner_rs::run_recipe_by_name("my-recipe", adapter, None, false);
    unsafe {
        std::env::remove_var("RECIPE_RUNNER_RECIPE_DIRS");
    }
    let result = result.expect("run_recipe_by_name should find the recipe");
    assert!(result.success, "recipe should succeed: {:?}", result);
}

#[test]
fn test_run_recipe_by_name_nonexistent_errors() {
    let adapter = MockAdapter::new();
    let result = recipe_runner_rs::run_recipe_by_name(
        "absolutely-nonexistent-recipe-xyz",
        adapter,
        None,
        false,
    );
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not found"),
        "Error should mention 'not found', got: {}",
        err_msg
    );
}

// -- RR-M6: Hook execution verification --

/// Adapter that actually runs bash commands but doesn't require the `claude`
/// binary, so hook tests work in CI where only bash is available.
struct RealBashAdapter;

impl Adapter for RealBashAdapter {
    fn execute_agent_step(
        &self,
        _prompt: &str,
        _agent_name: Option<&str>,
        _system_prompt: Option<&str>,
        _mode: Option<&str>,
        _working_dir: &str,
        _model: Option<&str>,
    ) -> Result<String, anyhow::Error> {
        Ok("mock agent response".to_string())
    }

    fn execute_bash_step(
        &self,
        command: &str,
        working_dir: &str,
        _timeout: Option<u64>,
        extra_env: &std::collections::HashMap<String, String>,
    ) -> Result<String, anyhow::Error> {
        let output = std::process::Command::new("/bin/bash")
            .args(["-c", command])
            .current_dir(working_dir)
            .envs(extra_env)
            .output()
            .map_err(|e| anyhow::anyhow!("Failed to run bash: {}: {}", command, e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "Command failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn is_available(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "real-bash"
    }
}

#[test]
fn test_hook_pre_step_actually_executes() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("hook_ran.txt");

    let yaml = format!(
        r#"
name: "hook-exec-test"
hooks:
  pre_step: "touch {}"
steps:
  - id: "step1"
    command: "echo hello"
"#,
        marker.display()
    );

    let adapter = RealBashAdapter;
    let parser = RecipeParser::new();
    let recipe = parser.parse(&yaml).unwrap();
    let runner = RecipeRunner::new(adapter).with_working_dir(tmp.path().to_str().unwrap());
    let result = runner.execute(&recipe, None);

    assert!(result.success, "Recipe should succeed: {:?}", result);
    assert!(
        marker.exists(),
        "pre_step hook should have created marker file at {}",
        marker.display()
    );
}

#[test]
fn test_hook_post_step_actually_executes() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("post_hook_ran.txt");

    let yaml = format!(
        r#"
name: "post-hook-test"
hooks:
  post_step: "touch {}"
steps:
  - id: "step1"
    command: "echo hello"
"#,
        marker.display()
    );

    let adapter = RealBashAdapter;
    let parser = RecipeParser::new();
    let recipe = parser.parse(&yaml).unwrap();
    let runner = RecipeRunner::new(adapter).with_working_dir(tmp.path().to_str().unwrap());
    let result = runner.execute(&recipe, None);

    assert!(result.success, "Recipe should succeed: {:?}", result);
    assert!(
        marker.exists(),
        "post_step hook should have created marker file at {}",
        marker.display()
    );
}

#[test]
fn test_hook_on_error_actually_executes() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("on_error_ran.txt");

    let yaml = format!(
        r#"
name: "on-error-hook-test"
hooks:
  on_error: "touch {}"
steps:
  - id: "failing-step"
    command: "exit 1"
    continue_on_error: true
"#,
        marker.display()
    );

    let adapter = RealBashAdapter;
    let parser = RecipeParser::new();
    let recipe = parser.parse(&yaml).unwrap();
    let runner = RecipeRunner::new(adapter).with_working_dir(tmp.path().to_str().unwrap());
    let result = runner.execute(&recipe, None);

    assert!(
        result.success,
        "Recipe should succeed (continue_on_error): {:?}",
        result
    );
    assert!(
        marker.exists(),
        "on_error hook should have created marker file at {}",
        marker.display()
    );
}

// -- Test: model parameter passthrough --

#[test]
fn test_model_parameter_passed_to_adapter() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct ModelCapturingAdapter {
        captured_models: Arc<Mutex<Vec<Option<String>>>>,
    }

    impl Adapter for ModelCapturingAdapter {
        fn execute_agent_step(
            &self,
            _prompt: &str,
            _agent_name: Option<&str>,
            _system_prompt: Option<&str>,
            _mode: Option<&str>,
            _working_dir: &str,
            model: Option<&str>,
        ) -> Result<String, anyhow::Error> {
            self.captured_models
                .lock()
                .unwrap()
                .push(model.map(|s| s.to_string()));
            Ok("ok".to_string())
        }

        fn execute_bash_step(
            &self,
            _command: &str,
            _working_dir: &str,
            _timeout: Option<u64>,
            _extra_env: &std::collections::HashMap<String, String>,
        ) -> Result<String, anyhow::Error> {
            Ok("ok".to_string())
        }

        fn is_available(&self) -> bool {
            true
        }
        fn name(&self) -> &str {
            "model-capturing"
        }
    }

    let yaml = r#"
name: model-test
steps:
  - id: fast-classify
    agent: classifier
    prompt: "Classify this"
    model: haiku

  - id: default-agent
    agent: worker
    prompt: "Do work"
"#;

    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();

    let captured = Arc::new(Mutex::new(Vec::new()));
    let adapter = ModelCapturingAdapter {
        captured_models: captured.clone(),
    };
    let runner = RecipeRunner::new(adapter);
    let result = runner.execute(&recipe, None);

    assert!(result.success, "Recipe should succeed: {:?}", result);

    let models = captured.lock().unwrap();
    assert_eq!(models.len(), 2, "Should have captured 2 agent calls");
    assert_eq!(
        models[0],
        Some("haiku".to_string()),
        "First step should pass model=haiku"
    );
    assert_eq!(models[1], None, "Second step should pass model=None");
}
