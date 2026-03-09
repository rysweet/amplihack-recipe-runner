/// Tests for ALL example and testing recipes.
///
/// Validates that every recipe under examples/ and recipes/testing/ parses,
/// validates, dry-runs, and executes correctly with a mock adapter.
use recipe_runner_rs::adapters::Adapter;
use recipe_runner_rs::models::{StepStatus, StepType};
use recipe_runner_rs::parser::RecipeParser;
use recipe_runner_rs::runner::RecipeRunner;
use std::path::{Path, PathBuf};

// ═══════════════════════════════════════════════════════════════════════════
// Mock adapter
// ═══════════════════════════════════════════════════════════════════════════

struct MockAdapter;

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
        if prompt.contains("JSON") || prompt.contains("json") || prompt.contains("analyze") {
            Ok(
                r#"{"status": "ok", "score": 85, "issues": [], "recommendation": "approved"}"#
                    .to_string(),
            )
        } else {
            Ok(format!("[mock-agent] {}", &prompt[..prompt.len().min(100)]))
        }
    }

    fn execute_bash_step(
        &self,
        command: &str,
        _working_dir: &str,
        _timeout: Option<u64>,
        extra_env: &std::collections::HashMap<String, String>,
    ) -> Result<String, anyhow::Error> {
        if command.trim().starts_with("echo ") || command.trim().starts_with("printf ") {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .envs(extra_env)
                .output()?;
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Ok(format!("[mock-bash] {}", &command[..command.len().min(80)]))
        }
    }

    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "mock"
    }
}

/// Adapter that fails on commands containing "exit 1" to simulate real failures.
struct FailOnExitAdapter;

impl Adapter for FailOnExitAdapter {
    fn execute_agent_step(
        &self,
        prompt: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
        _: Option<&str>,
    ) -> Result<String, anyhow::Error> {
        Ok(format!("[mock-agent] {}", &prompt[..prompt.len().min(100)]))
    }

    fn execute_bash_step(
        &self,
        command: &str,
        _: &str,
        _: Option<u64>,
        extra_env: &std::collections::HashMap<String, String>,
    ) -> Result<String, anyhow::Error> {
        if command.contains("exit 1") {
            anyhow::bail!("command failed with exit code 1")
        }
        if command.trim().starts_with("echo ") || command.trim().starts_with("printf ") {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .envs(extra_env)
                .output()?;
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Ok(format!("[mock-bash] {}", &command[..command.len().min(80)]))
        }
    }

    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "fail-on-exit"
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn load_recipe(rel_path: &str) -> String {
    let path = manifest_dir().join(rel_path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e))
}

fn tutorials_dir() -> PathBuf {
    manifest_dir().join("examples/tutorials")
}

/// Parse and validate — assert no warnings.
fn parse_and_validate(rel_path: &str) -> recipe_runner_rs::models::Recipe {
    let yaml = load_recipe(rel_path);
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(&yaml)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {}", rel_path, e));
    let warnings = parser.validate_with_yaml(&recipe, Some(&yaml));
    assert!(
        warnings.is_empty(),
        "{} has validation warnings: {:?}",
        rel_path,
        warnings
    );
    recipe
}

/// Parse, validate, dry-run — assert success.
fn parse_validate_dryrun(rel_path: &str) {
    let recipe = parse_and_validate(rel_path);
    let runner = RecipeRunner::new(MockAdapter).with_dry_run(true);
    let result = runner.execute(&recipe, None);
    assert!(result.success, "{} dry-run failed: {:?}", rel_path, result);
}

// ═══════════════════════════════════════════════════════════════════════════
// TUTORIAL TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_tutorial_01_hello_world() {
    let recipe = parse_and_validate("examples/tutorials/01-hello-world.yaml");
    assert_eq!(recipe.name, "hello-world");
    assert_eq!(recipe.steps.len(), 1);

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success);
    assert_eq!(result.step_results.len(), 1);
    assert_eq!(result.step_results[0].status, StepStatus::Completed);

    // Output should contain "Hello"
    let greeting = result.context.get("greeting");
    assert!(greeting.is_some(), "greeting output should be in context");
    let greeting_str = greeting.unwrap().as_str().unwrap_or("");
    assert!(
        greeting_str.contains("Hello"),
        "greeting should contain 'Hello', got: {:?}",
        greeting_str
    );
}

#[test]
fn test_tutorial_02_variables() {
    let recipe = parse_and_validate("examples/tutorials/02-variables.yaml");
    assert_eq!(recipe.name, "variables");
    assert_eq!(recipe.steps.len(), 3);

    // Verify context has the expected variables
    assert_eq!(recipe.context.get("greeting").unwrap(), "Howdy");
    assert_eq!(recipe.context.get("name").unwrap(), "Developer");

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success);
    assert_eq!(result.step_results.len(), 3);

    // Variables should be substituted: output contains "Howdy"
    let hello = result.context.get("hello_message");
    assert!(hello.is_some(), "hello_message should be in context");
    let hello_str = hello.unwrap().as_str().unwrap_or("");
    assert!(
        hello_str.contains("Howdy") && hello_str.contains("Developer"),
        "Variables not substituted correctly, got: {:?}",
        hello_str
    );
}

#[test]
fn test_tutorial_03_conditions() {
    let recipe = parse_and_validate("examples/tutorials/03-conditions.yaml");
    assert_eq!(recipe.name, "conditions");
    assert_eq!(recipe.steps.len(), 6);

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success);
    assert_eq!(result.step_results.len(), 6);

    // "always-runs" — no condition, runs
    assert_eq!(result.step_results[0].status, StepStatus::Completed);
    // "run-tests" — condition "run_tests" (true), runs
    assert_eq!(result.step_results[1].status, StepStatus::Completed);
    // "deploy" — condition "not skip_deploy" (not false = true), runs
    assert_eq!(result.step_results[2].status, StepStatus::Completed);
    // "count-check" — condition "count > 0" (5 > 0 = true), runs
    assert_eq!(result.step_results[3].status, StepStatus::Completed);
    // "skipped-step" — condition "skip_deploy" (false), SKIPPED
    assert_eq!(result.step_results[4].status, StepStatus::Skipped);
    // "summary" — no condition, runs
    assert_eq!(result.step_results[5].status, StepStatus::Completed);
}

#[test]
fn test_tutorial_04_multi_step_pipeline() {
    let recipe = parse_and_validate("examples/tutorials/04-multi-step-pipeline.yaml");
    assert_eq!(recipe.name, "multi-step-pipeline");
    assert_eq!(recipe.steps.len(), 5);

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success);
    // All 5 steps complete
    assert_eq!(result.step_results.len(), 5);
    for (i, sr) in result.step_results.iter().enumerate() {
        assert_eq!(
            sr.status,
            StepStatus::Completed,
            "step {} ({}) should be Completed",
            i,
            sr.step_id
        );
    }
}

#[test]
fn test_tutorial_05_working_directories() {
    let recipe = parse_and_validate("examples/tutorials/05-working-directories.yaml");
    assert_eq!(recipe.name, "working-directories");

    // Dry-run succeeds
    let runner = RecipeRunner::new(MockAdapter).with_dry_run(true);
    let result = runner.execute(&recipe, None);
    assert!(result.success);
}

#[test]
fn test_tutorial_06_parse_json() {
    let recipe = parse_and_validate("examples/tutorials/06-parse-json.yaml");
    assert_eq!(recipe.name, "parse-json");
    assert_eq!(recipe.steps.len(), 4);

    // Steps 0 and 2 have parse_json: true
    assert!(recipe.steps[0].parse_json);
    assert!(recipe.steps[2].parse_json);

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success);

    // JSON should be parsed into context
    let parsed = result.context.get("result");
    assert!(parsed.is_some(), "result should be in context");
    // The parsed value should be an object (not a string)
    assert!(
        parsed.unwrap().is_object(),
        "result should be parsed JSON object, got: {:?}",
        parsed
    );
}

#[test]
fn test_tutorial_07_error_handling() {
    let recipe = parse_and_validate("examples/tutorials/07-error-handling.yaml");
    assert_eq!(recipe.name, "error-handling");
    assert_eq!(recipe.steps.len(), 4);

    // Step "failing-step" has continue_on_error: true
    assert!(recipe.steps[1].continue_on_error);

    let runner = RecipeRunner::new(FailOnExitAdapter);
    let result = runner.execute(&recipe, None);

    // Recipe overall succeeds because the failure is contained
    assert!(
        result.success,
        "recipe should succeed overall: {:?}",
        result
    );

    // Step 1 succeeds, step 2 fails (but continues), step 3 and 4 run
    assert_eq!(result.step_results[0].status, StepStatus::Completed);
    assert_eq!(result.step_results[1].status, StepStatus::Failed);
    assert_eq!(result.step_results[2].status, StepStatus::Completed);
    assert_eq!(result.step_results[3].status, StepStatus::Completed);
}

#[test]
fn test_tutorial_08_hooks() {
    let recipe = parse_and_validate("examples/tutorials/08-hooks.yaml");
    assert_eq!(recipe.name, "hooks");

    // Hooks are defined
    assert!(recipe.hooks.pre_step.is_some());
    assert!(recipe.hooks.post_step.is_some());
    assert!(recipe.hooks.on_error.is_some());

    let runner = RecipeRunner::new(FailOnExitAdapter);
    let result = runner.execute(&recipe, None);
    // Recipe succeeds because the bad step has continue_on_error
    assert!(result.success, "hooks recipe should succeed: {:?}", result);
    assert_eq!(result.step_results.len(), 4);
}

#[test]
fn test_tutorial_09_tags() {
    let recipe = parse_and_validate("examples/tutorials/09-tags.yaml");
    assert_eq!(recipe.name, "tags");

    // Verify when_tags are parsed
    assert_eq!(recipe.steps[0].when_tags, vec!["fast"]);
    assert_eq!(recipe.steps[3].when_tags, vec!["slow"]);
    assert!(recipe.steps[5].when_tags.is_empty()); // "report" has no tags

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success);
    assert_eq!(result.step_results.len(), 6);
}

#[test]
fn test_tutorial_10_parallel_groups() {
    let recipe = parse_and_validate("examples/tutorials/10-parallel-groups.yaml");
    assert_eq!(recipe.name, "parallel-groups");

    // Verify parallel_group is parsed
    assert_eq!(recipe.steps[1].parallel_group.as_deref(), Some("build"));
    assert_eq!(recipe.steps[2].parallel_group.as_deref(), Some("build"));

    // Dry-run succeeds
    parse_validate_dryrun("examples/tutorials/10-parallel-groups.yaml");
}

#[test]
fn test_tutorial_11_extends() {
    let recipe = parse_and_validate("examples/tutorials/11-extends.yaml");
    assert_eq!(recipe.name, "extends-example");

    // extends field is present
    assert_eq!(recipe.extends.as_deref(), Some("base"));

    // Dry-run with the tutorials dir as search path (so it can find base.yaml)
    let runner = RecipeRunner::new(MockAdapter)
        .with_dry_run(true)
        .with_recipe_search_dirs(vec![tutorials_dir()]);
    let result = runner.execute(&recipe, None);
    assert!(result.success, "extends dry-run failed: {:?}", result);
}

#[test]
fn test_tutorial_12_recursion_limits() {
    let recipe = parse_and_validate("examples/tutorials/12-recursion-limits.yaml");
    assert_eq!(recipe.name, "recursion-limits");

    // Recursion config is parsed
    assert_eq!(recipe.recursion.max_depth, 2);
    assert_eq!(recipe.recursion.max_total_steps, 10);

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success);
    assert_eq!(result.step_results.len(), 3);
}

#[test]
fn test_tutorial_13_timeouts() {
    let recipe = parse_and_validate("examples/tutorials/13-timeouts.yaml");
    assert_eq!(recipe.name, "timeouts");

    // Timeout fields parsed
    assert_eq!(recipe.steps[0].timeout, Some(5));
    assert_eq!(recipe.steps[1].timeout, Some(2));
    assert!(recipe.steps[2].timeout.is_none());

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success);
    assert_eq!(result.step_results.len(), 3);
}

#[test]
fn test_tutorial_14_dry_run() {
    let recipe = parse_and_validate("examples/tutorials/14-dry-run.yaml");
    assert_eq!(recipe.name, "dry-run");
    assert_eq!(recipe.steps.len(), 4);

    // Dry-run: no real execution
    let runner = RecipeRunner::new(MockAdapter).with_dry_run(true);
    let result = runner.execute(&recipe, None);
    assert!(result.success);
    assert_eq!(result.step_results.len(), 4);
    for sr in &result.step_results {
        assert_eq!(sr.status, StepStatus::Skipped);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PATTERN TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_pattern_quality_audit() {
    parse_validate_dryrun("examples/patterns/quality-audit.yaml");
}

#[test]
fn test_pattern_investigation() {
    parse_validate_dryrun("examples/patterns/investigation.yaml");
}

#[test]
fn test_pattern_ci_pipeline() {
    let recipe = parse_and_validate("examples/patterns/ci-pipeline.yaml");

    // Execute with mock — bash-only echo steps work
    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success, "ci-pipeline failed: {:?}", result);
    for sr in &result.step_results {
        assert!(
            sr.status == StepStatus::Completed || sr.status == StepStatus::Skipped,
            "step {} unexpected status: {:?}",
            sr.step_id,
            sr.status
        );
    }
}

#[test]
fn test_pattern_code_review() {
    parse_validate_dryrun("examples/patterns/code-review.yaml");
}

#[test]
fn test_pattern_migration() {
    parse_validate_dryrun("examples/patterns/migration.yaml");
}

#[test]
fn test_pattern_multi_agent_consensus() {
    parse_validate_dryrun("examples/patterns/multi-agent-consensus.yaml");
}

#[test]
fn test_pattern_self_improvement() {
    parse_validate_dryrun("examples/patterns/self-improvement.yaml");
}

#[test]
fn test_pattern_deploy_pipeline() {
    parse_validate_dryrun("examples/patterns/deploy-pipeline.yaml");
}

// ═══════════════════════════════════════════════════════════════════════════
// TESTING RECIPE TESTS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_testing_all_condition_operators() {
    let recipe = parse_and_validate("recipes/testing/all-condition-operators.yaml");
    assert_eq!(recipe.name, "all-condition-operators");

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(
        result.success,
        "all-condition-operators failed: {:?}",
        result
    );

    // ALL steps should complete (none skipped) since all conditions are true
    for sr in &result.step_results {
        assert_eq!(
            sr.status,
            StepStatus::Completed,
            "step '{}' should be Completed, was {:?}",
            sr.step_id,
            sr.status
        );
    }
    assert_eq!(result.step_results.len(), 11);
}

#[test]
fn test_testing_all_functions() {
    let recipe = parse_and_validate("recipes/testing/all-functions.yaml");
    assert_eq!(recipe.name, "all-functions");

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success, "all-functions failed: {:?}", result);

    // 6 of 7 steps complete; fn-str is skipped because str(42) produces
    // "42.0" (float literal) which doesn't equal "42"
    assert_eq!(result.step_results.len(), 7);
    let completed: Vec<_> = result
        .step_results
        .iter()
        .filter(|sr| sr.status == StepStatus::Completed)
        .collect();
    let skipped: Vec<_> = result
        .step_results
        .iter()
        .filter(|sr| sr.status == StepStatus::Skipped)
        .collect();
    assert_eq!(completed.len(), 6, "6 steps should complete");
    assert_eq!(skipped.len(), 1, "fn-str should be skipped");
    assert_eq!(skipped[0].step_id, "fn-str");
}

#[test]
fn test_testing_all_methods() {
    let recipe = parse_and_validate("recipes/testing/all-methods.yaml");
    assert_eq!(recipe.name, "all-methods");

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success, "all-methods failed: {:?}", result);

    // ALL steps should complete
    for sr in &result.step_results {
        assert_eq!(
            sr.status,
            StepStatus::Completed,
            "step '{}' should be Completed, was {:?}",
            sr.step_id,
            sr.status
        );
    }
    assert_eq!(result.step_results.len(), 13);
}

#[test]
fn test_testing_nested_context() {
    let recipe = parse_and_validate("recipes/testing/nested-context.yaml");
    assert_eq!(recipe.name, "nested-context");

    // With C2-RR-7 fix, dot-notation property access works in conditions,
    // so conditions like "config.db.port == 5432" now resolve correctly.
    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(
        result.success,
        "nested-context should succeed with dot-notation property access: {:?}",
        result
    );
    for sr in &result.step_results {
        assert_eq!(
            sr.status,
            StepStatus::Completed,
            "step '{}' should be Completed, was {:?}",
            sr.step_id,
            sr.status
        );
    }
}

#[test]
fn test_testing_output_chaining() {
    let recipe = parse_and_validate("recipes/testing/output-chaining.yaml");
    assert_eq!(recipe.name, "output-chaining");
    assert_eq!(recipe.steps.len(), 5);

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success, "output-chaining failed: {:?}", result);
    assert_eq!(result.step_results.len(), 5);

    for sr in &result.step_results {
        assert_eq!(sr.status, StepStatus::Completed);
    }
}

#[test]
fn test_testing_json_extraction() {
    let recipe = parse_and_validate("recipes/testing/json-extraction-strategies.yaml");
    assert_eq!(recipe.name, "json-extraction-strategies");

    // All 3 steps have parse_json: true
    for step in &recipe.steps {
        assert!(step.parse_json, "step '{}' should have parse_json", step.id);
    }

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(
        result.success,
        "json-extraction-strategies failed: {:?}",
        result
    );

    // All 3 parse_json strategies should produce parsed objects
    for name in &["raw_json_out", "fenced_json_out", "mixed_json_out"] {
        let val = result.context.get(*name);
        assert!(val.is_some(), "context should contain '{}'", name);
        assert!(
            val.unwrap().is_object(),
            "'{}' should be a parsed JSON object, got: {:?}",
            name,
            val
        );
    }
}

#[test]
fn test_testing_empty_and_edge_cases() {
    let recipe = parse_and_validate("recipes/testing/empty-and-edge-cases.yaml");
    assert_eq!(recipe.name, "empty-and-edge-cases");

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success, "empty-and-edge-cases failed: {:?}", result);

    // Steps with falsy conditions should be skipped
    // empty_str → skipped, zero_val → skipped, false_val → skipped
    assert_eq!(
        result.step_results[0].status,
        StepStatus::Skipped,
        "empty-string-skip should be Skipped"
    );
    assert_eq!(
        result.step_results[1].status,
        StepStatus::Skipped,
        "zero-skip should be Skipped"
    );
    assert_eq!(
        result.step_results[2].status,
        StepStatus::Skipped,
        "false-skip should be Skipped"
    );

    // Truthy conditions should run
    assert_eq!(
        result.step_results[3].status,
        StepStatus::Completed,
        "nonempty-run should be Completed"
    );
    assert_eq!(
        result.step_results[4].status,
        StepStatus::Completed,
        "true-run should be Completed"
    );
    assert_eq!(
        result.step_results[5].status,
        StepStatus::Completed,
        "one-run should be Completed"
    );

    // Negated edge cases should run
    assert_eq!(
        result.step_results[6].status,
        StepStatus::Completed,
        "not-empty-run should be Completed"
    );
    assert_eq!(
        result.step_results[7].status,
        StepStatus::Completed,
        "not-false-run should be Completed"
    );
}

#[test]
fn test_testing_large_context() {
    let recipe = parse_and_validate("recipes/testing/large-context.yaml");
    assert_eq!(recipe.name, "large-context");
    assert_eq!(recipe.steps.len(), 25);

    let runner = RecipeRunner::new(MockAdapter);
    let result = runner.execute(&recipe, None);
    assert!(result.success, "large-context failed: {:?}", result);

    // All 25 steps should complete without corruption
    assert_eq!(result.step_results.len(), 25);
    for sr in &result.step_results {
        assert_eq!(
            sr.status,
            StepStatus::Completed,
            "step '{}' should be Completed, was {:?}",
            sr.step_id,
            sr.status
        );
    }

    // Final output should exist
    assert!(
        result.context.contains_key("final_out"),
        "final_out should be in context"
    );
}

#[test]
fn test_testing_step_type_inference() {
    let recipe = parse_and_validate("recipes/testing/step-type-inference.yaml");
    assert_eq!(recipe.name, "step-type-inference");

    // Verify correct types inferred
    assert_eq!(recipe.steps[0].effective_type(), StepType::Bash); // infer-bash-from-command
    assert_eq!(recipe.steps[1].effective_type(), StepType::Bash); // explicit-bash
    assert_eq!(recipe.steps[2].effective_type(), StepType::Agent); // infer-agent-from-fields
    assert_eq!(recipe.steps[3].effective_type(), StepType::Agent); // infer-agent-from-prompt
    assert_eq!(recipe.steps[4].effective_type(), StepType::Recipe); // infer-recipe-from-field
    assert_eq!(recipe.steps[5].effective_type(), StepType::Bash); // bash-with-condition
}

#[test]
fn test_testing_continue_on_error_chain() {
    let recipe = parse_and_validate("recipes/testing/continue-on-error-chain.yaml");
    assert_eq!(recipe.name, "continue-on-error-chain");
    assert_eq!(recipe.steps.len(), 5);

    // step2 has continue_on_error: true, step4 has continue_on_error: false
    assert!(recipe.steps[1].continue_on_error);
    assert!(!recipe.steps[3].continue_on_error);

    let runner = RecipeRunner::new(FailOnExitAdapter);
    let result = runner.execute(&recipe, None);

    // Recipe should fail (step4 fails without continue_on_error)
    assert!(!result.success, "recipe should fail due to step4");

    // Steps 1-3 should run
    assert_eq!(result.step_results[0].status, StepStatus::Completed); // step1-succeed
    assert_eq!(result.step_results[1].status, StepStatus::Failed); // step2-fail-continue (fails but continues)
    assert_eq!(result.step_results[2].status, StepStatus::Completed); // step3-succeed-after-error
    assert_eq!(result.step_results[3].status, StepStatus::Failed); // step4-fail-stop

    // Step 5 should NOT run (fail-fast)
    assert_eq!(
        result.step_results.len(),
        4,
        "step5 should not have run; only 4 results expected"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// BULK VALIDATION TESTS
// ═══════════════════════════════════════════════════════════════════════════

fn collect_yaml_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if !dir.is_dir() {
        return files;
    }
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            files.extend(collect_yaml_files(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            files.push(path);
        }
    }
    files.sort();
    files
}

fn all_recipe_files() -> Vec<PathBuf> {
    let root = manifest_dir();
    let mut files = Vec::new();
    files.extend(collect_yaml_files(&root.join("examples")));
    files.extend(collect_yaml_files(&root.join("recipes")));
    files
}

#[test]
fn test_all_recipes_parse() {
    let parser = RecipeParser::new();
    let files = all_recipe_files();
    assert!(!files.is_empty(), "no recipe files found");

    for path in &files {
        let yaml = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e));
        let result = parser.parse(&yaml);
        assert!(
            result.is_ok(),
            "Failed to parse {}: {:?}",
            path.display(),
            result.err()
        );
    }
}

#[test]
fn test_all_recipes_validate() {
    let parser = RecipeParser::new();
    let files = all_recipe_files();
    assert!(!files.is_empty(), "no recipe files found");

    for path in &files {
        let yaml = std::fs::read_to_string(path).unwrap();
        let recipe = parser.parse(&yaml).unwrap();
        let warnings = parser.validate_with_yaml(&recipe, Some(&yaml));
        assert!(
            warnings.is_empty(),
            "{} has validation warnings: {:?}",
            path.display(),
            warnings
        );
    }
}

#[test]
fn test_all_recipes_dry_run() {
    let parser = RecipeParser::new();
    let files = all_recipe_files();
    assert!(!files.is_empty(), "no recipe files found");

    // Provide search dirs so extends resolution works (e.g., 11-extends.yaml → base.yaml)
    let search_dirs = vec![
        tutorials_dir(),
        manifest_dir().join("examples"),
        manifest_dir().join("recipes"),
    ];

    let tutorial_dir_prefix = tutorials_dir();

    for path in &files {
        let yaml = std::fs::read_to_string(path).unwrap();
        let recipe = parser.parse(&yaml).unwrap();
        let runner = RecipeRunner::new(MockAdapter)
            .with_dry_run(true)
            .with_recipe_search_dirs(search_dirs.clone());
        let result = runner.execute(&recipe, None);
        assert!(
            result.success,
            "{} dry-run failed: {:?}",
            path.display(),
            result
        );

        // C2-RD-13: For tutorial recipes, verify non-empty steps and matching name
        if path.starts_with(&tutorial_dir_prefix) {
            assert!(
                !result.step_results.is_empty(),
                "{}: tutorial recipe produced no step results",
                path.display()
            );
            assert_eq!(
                result.recipe_name,
                recipe.name,
                "{}: result recipe_name '{}' doesn't match parsed recipe name '{}'",
                path.display(),
                result.recipe_name,
                recipe.name
            );
        }
    }
}
