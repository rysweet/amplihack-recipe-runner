/// Tests for new features: continue_on_error, recursion limits, hooks,
/// tag filtering, audit log, timing, and property-based tests.
use recipe_runner_rs::adapters::Adapter;
use recipe_runner_rs::context::RecipeContext;
use recipe_runner_rs::models::{StepResult, StepStatus, StepType};
use recipe_runner_rs::parser::RecipeParser;
use recipe_runner_rs::runner::{ExecutionListener, RecipeRunner};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Re-use mock adapter pattern
struct MockAdapter;
impl Adapter for MockAdapter {
    fn execute_agent_step(
        &self,
        prompt: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
        _: Option<u64>,
    ) -> Result<String, anyhow::Error> {
        if prompt.contains("FAIL") {
            anyhow::bail!("Simulated failure");
        }
        Ok(format!("[mock-agent] {}", &prompt[..prompt.len().min(60)]))
    }
    fn execute_bash_step(
        &self,
        command: &str,
        _: &str,
        _: Option<u64>,
    ) -> Result<String, anyhow::Error> {
        if command.contains("FAIL") {
            anyhow::bail!("Simulated failure");
        }
        Ok(format!("[mock-bash] {}", command))
    }
    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "mock"
    }
}

fn parse_and_run(yaml: &str) -> recipe_runner_rs::models::RecipeResult {
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    RecipeRunner::new(MockAdapter).execute(&recipe, None)
}

// ═══════════════════════════════════════════════════════════════════════════
// CONTINUE_ON_ERROR
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_continue_on_error_allows_subsequent_steps() {
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: will-fail
    command: "FAIL"
    continue_on_error: true
  - id: still-runs
    command: "echo ok"
  - id: also-runs
    command: "echo done"
"#,
    );
    assert!(r.success);
    assert_eq!(r.step_results.len(), 3);
    assert_eq!(r.step_results[0].status, StepStatus::Failed);
    assert_eq!(r.step_results[1].status, StepStatus::Completed);
    assert_eq!(r.step_results[2].status, StepStatus::Completed);
}

#[test]
fn test_continue_on_error_default_false_still_fails() {
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: will-fail
    command: "FAIL"
  - id: never-runs
    command: "echo ok"
"#,
    );
    assert!(!r.success);
    assert_eq!(r.step_results.len(), 1);
    assert_eq!(r.step_results[0].status, StepStatus::Failed);
}

#[test]
fn test_continue_on_error_with_parse_json() {
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: bad-json
    prompt: "return something"
    parse_json: true
    output: "data"
    continue_on_error: true
  - id: still-runs
    command: "echo ok"
"#,
    );
    assert!(r.success);
    assert_eq!(r.step_results[0].status, StepStatus::Failed);
    assert_eq!(r.step_results[1].status, StepStatus::Completed);
}

// ═══════════════════════════════════════════════════════════════════════════
// RECURSION LIMITS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_recipe_level_recursion_depth_limit() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("self-ref.yaml"),
        r#"
name: self-ref
steps:
  - id: recurse
    recipe: "self-ref"
"#,
    )
    .unwrap();

    let parser = RecipeParser::new();
    // Recipe with max_depth: 2 (stricter than default 6)
    let recipe = parser
        .parse(
            r#"
name: parent
recursion:
  max_depth: 2
steps:
  - id: start
    recipe: "self-ref"
"#,
        )
        .unwrap();

    let runner =
        RecipeRunner::new(MockAdapter).with_recipe_search_dirs(vec![tmp.path().to_path_buf()]);
    let r = runner.execute(&recipe, None);
    assert!(!r.success);
    // Error should mention the sub-recipe failure (depth guard is inside the sub-recipe)
    let last_error = &r.step_results.last().unwrap().error;
    assert!(
        last_error.contains("failed") || last_error.contains("depth"),
        "expected depth/failure error, got: {}",
        last_error
    );
}

#[test]
fn test_total_step_limit_enforced() {
    let r = parse_and_run(
        r#"
name: t
recursion:
  max_total_steps: 3
steps:
  - id: s1
    command: "echo 1"
  - id: s2
    command: "echo 2"
  - id: s3
    command: "echo 3"
  - id: s4
    command: "echo 4"
"#,
    );
    assert!(!r.success);
    assert_eq!(r.step_results.len(), 4);
    assert_eq!(r.step_results[3].status, StepStatus::Failed);
    assert!(r.step_results[3].error.contains("step limit"));
}

#[test]
fn test_recursion_config_defaults() {
    let parser = RecipeParser::new();
    let recipe = parser
        .parse("name: t\nsteps:\n  - id: s1\n    command: echo")
        .unwrap();
    assert_eq!(recipe.recursion.max_depth, 6);
    assert_eq!(recipe.recursion.max_total_steps, 200);
}

// ═══════════════════════════════════════════════════════════════════════════
// HOOKS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_hooks_parsed_from_yaml() {
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: t
hooks:
  pre_step: "echo pre"
  post_step: "echo post"
  on_error: "echo err"
steps:
  - id: s1
    command: "echo hello"
"#,
        )
        .unwrap();
    assert_eq!(recipe.hooks.pre_step.as_deref(), Some("echo pre"));
    assert_eq!(recipe.hooks.post_step.as_deref(), Some("echo post"));
    assert_eq!(recipe.hooks.on_error.as_deref(), Some("echo err"));
}

#[test]
fn test_hooks_default_to_none() {
    let parser = RecipeParser::new();
    let recipe = parser
        .parse("name: t\nsteps:\n  - id: s1\n    command: echo")
        .unwrap();
    assert!(recipe.hooks.pre_step.is_none());
    assert!(recipe.hooks.post_step.is_none());
    assert!(recipe.hooks.on_error.is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// TAG FILTERING
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_include_tags_runs_only_matching() {
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: t
steps:
  - id: tagged
    command: "echo tagged"
    when_tags: ["fast"]
  - id: untagged
    command: "echo untagged"
  - id: wrong-tag
    command: "echo wrong"
    when_tags: ["slow"]
"#,
        )
        .unwrap();

    let runner = RecipeRunner::new(MockAdapter).with_tags(vec!["fast".to_string()], vec![]);
    let r = runner.execute(&recipe, None);
    assert!(r.success);
    assert_eq!(r.step_results[0].status, StepStatus::Completed); // "tagged" matches
    assert_eq!(r.step_results[1].status, StepStatus::Completed); // "untagged" has no when_tags
    assert_eq!(r.step_results[2].status, StepStatus::Skipped); // "wrong-tag" doesn't match
}

#[test]
fn test_exclude_tags_skips_matching() {
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: t
steps:
  - id: excluded
    command: "echo no"
    when_tags: ["slow"]
  - id: included
    command: "echo yes"
"#,
        )
        .unwrap();

    let runner = RecipeRunner::new(MockAdapter).with_tags(vec![], vec!["slow".to_string()]);
    let r = runner.execute(&recipe, None);
    assert!(r.success);
    assert_eq!(r.step_results[0].status, StepStatus::Skipped);
    assert_eq!(r.step_results[1].status, StepStatus::Completed);
}

// ═══════════════════════════════════════════════════════════════════════════
// TIMING
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_step_results_have_duration() {
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    command: "echo hello"
"#,
    );
    assert!(r.step_results[0].duration.is_some());
    assert!(r.duration.is_some());
}

#[test]
fn test_recipe_result_serializes_to_json() {
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    command: "echo hello"
"#,
    );
    let json = serde_json::to_string_pretty(&r).unwrap();
    assert!(json.contains("recipe_name"));
    assert!(json.contains("step_results"));
    assert!(json.contains("duration"));
}

// ═══════════════════════════════════════════════════════════════════════════
// AUDIT LOG
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_audit_log_creates_jsonl_file() {
    let tmp = tempfile::tempdir().unwrap();
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: "audit-test"
steps:
  - id: s1
    command: "echo hello"
  - id: s2
    command: "echo world"
"#,
        )
        .unwrap();

    let runner = RecipeRunner::new(MockAdapter).with_audit_dir(tmp.path().to_path_buf());
    runner.execute(&recipe, None);

    // Should have created a .jsonl file
    let files: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    assert_eq!(files.len(), 1, "expected 1 audit log file");

    let content = std::fs::read_to_string(files[0].path()).unwrap();
    let lines: Vec<&str> = content.trim().lines().collect();
    assert_eq!(lines.len(), 2, "expected 2 audit entries");

    // Verify each line is valid JSON
    for line in &lines {
        let entry: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(entry.get("step_id").is_some());
        assert!(entry.get("status").is_some());
        assert!(entry.get("duration_ms").is_some());
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// PARALLEL GROUP EXECUTION
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_parallel_group_basic() {
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: a
    command: "echo a"
    parallel_group: "p1"
    output: "out_a"
  - id: b
    command: "echo b"
    parallel_group: "p1"
    output: "out_b"
"#,
    );
    assert!(r.success);
    assert_eq!(r.step_results.len(), 2);
    assert_eq!(r.step_results[0].status, StepStatus::Completed);
    assert_eq!(r.step_results[1].status, StepStatus::Completed);
    // Verify outputs were captured
    assert!(r.context.contains_key("out_a"));
    assert!(r.context.contains_key("out_b"));
}

#[test]
fn test_parallel_group_mixed() {
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: seq1
    command: "echo first"
    output: "r1"
  - id: par_a
    command: "echo a"
    parallel_group: "g1"
    output: "r2"
  - id: par_b
    command: "echo b"
    parallel_group: "g1"
    output: "r3"
  - id: seq2
    command: "echo last"
    output: "r4"
"#,
    );
    assert!(r.success);
    assert_eq!(r.step_results.len(), 4);
    assert_eq!(r.step_results[0].status, StepStatus::Completed); // seq1
    assert_eq!(r.step_results[1].status, StepStatus::Completed); // par_a
    assert_eq!(r.step_results[2].status, StepStatus::Completed); // par_b
    assert_eq!(r.step_results[3].status, StepStatus::Completed); // seq2
    // All outputs stored
    assert!(r.context.contains_key("r1"));
    assert!(r.context.contains_key("r2"));
    assert!(r.context.contains_key("r3"));
    assert!(r.context.contains_key("r4"));
}

#[test]
fn test_parallel_group_failure() {
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: ok
    command: "echo ok"
    parallel_group: "g1"
  - id: fail
    command: "FAIL"
    parallel_group: "g1"
  - id: should-not-run
    command: "echo after"
"#,
    );
    assert!(!r.success);
    // Both parallel steps should have results (they run concurrently)
    assert_eq!(r.step_results.len(), 2);
    let fail_result = r.step_results.iter().find(|r| r.step_id == "fail").unwrap();
    assert_eq!(fail_result.status, StepStatus::Failed);
    // Step after the group must not run
    assert!(!r.step_results.iter().any(|r| r.step_id == "should-not-run"));
}

// ═══════════════════════════════════════════════════════════════════════════
// RECIPE COMPOSITION (extends, when_tags)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_extends_field_parsed() {
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: child
extends: "parent-recipe"
steps:
  - id: s1
    command: echo
"#,
        )
        .unwrap();
    assert_eq!(recipe.extends.as_deref(), Some("parent-recipe"));
}

#[test]
fn test_parallel_group_parsed() {
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: t
steps:
  - id: a
    command: "echo a"
    parallel_group: "phase1"
  - id: b
    command: "echo b"
    parallel_group: "phase1"
  - id: c
    command: "echo c"
"#,
        )
        .unwrap();
    assert_eq!(recipe.steps[0].parallel_group.as_deref(), Some("phase1"));
    assert_eq!(recipe.steps[1].parallel_group.as_deref(), Some("phase1"));
    assert!(recipe.steps[2].parallel_group.is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// PROPERTY-BASED TESTS (proptest)
// ═══════════════════════════════════════════════════════════════════════════

use proptest::prelude::*;

proptest! {
    /// The condition evaluator must NEVER panic on arbitrary input.
    #[test]
    fn prop_condition_evaluator_never_panics(s in "\\PC{0,200}") {
        let ctx = RecipeContext::new(HashMap::new());
        let _ = ctx.evaluate(&s);
    }

    /// Template rendering must never panic on arbitrary templates.
    #[test]
    fn prop_template_render_never_panics(template in "\\PC{0,200}") {
        let mut data = HashMap::new();
        data.insert("x".to_string(), json!("val"));
        let ctx = RecipeContext::new(data);
        let _ = ctx.render(&template);
        let _ = ctx.render_shell(&template);
    }

    /// The YAML parser must never panic on arbitrary input.
    #[test]
    fn prop_yaml_parser_never_panics(yaml in "\\PC{0,500}") {
        let parser = RecipeParser::new();
        let _ = parser.parse(&yaml);
    }

    /// Rendering the same template twice must produce the same result.
    #[test]
    fn prop_render_idempotent(key in "[a-z]{1,10}", value in "[a-zA-Z0-9 ]{0,50}") {
        let template = format!("pre-{{{{{}}}}}post", key);
        let mut data = HashMap::new();
        data.insert(key, json!(value));
        let ctx = RecipeContext::new(data);
        let r1 = ctx.render(&template);
        let r2 = ctx.render(&template);
        prop_assert_eq!(r1, r2);
    }

    /// Boolean conditions with known values must produce consistent results.
    #[test]
    fn prop_truthiness_consistent(val in prop_oneof![
        Just(json!(true)),
        Just(json!(false)),
        Just(json!("")),
        Just(json!("nonempty")),
        Just(json!(0)),
        Just(json!(42)),
        Just(json!(null)),
    ]) {
        let mut data = HashMap::new();
        data.insert("v".to_string(), val.clone());
        let ctx = RecipeContext::new(data);
        let r1 = ctx.evaluate("v");
        let r2 = ctx.evaluate("v");
        prop_assert_eq!(r1.is_ok(), r2.is_ok());
        if let (Ok(a), Ok(b)) = (r1, r2) {
            prop_assert_eq!(a, b);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// EXECUTION LISTENER (C2-RD-8)
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Clone)]
struct TrackingListener {
    started: Arc<Mutex<Vec<String>>>,
    completed: Arc<Mutex<Vec<String>>>,
}

impl TrackingListener {
    fn new() -> Self {
        Self {
            started: Arc::new(Mutex::new(Vec::new())),
            completed: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl ExecutionListener for TrackingListener {
    fn on_step_start(&self, step_id: &str, _step_type: StepType) {
        self.started.lock().unwrap().push(step_id.to_string());
    }
    fn on_step_complete(&self, result: &StepResult) {
        self.completed.lock().unwrap().push(result.step_id.clone());
    }
}

#[test]
fn test_execution_listener_callbacks_fire() {
    let listener = TrackingListener::new();
    let started = listener.started.clone();
    let completed = listener.completed.clone();

    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: listener-test
steps:
  - id: step-a
    command: "echo hello"
  - id: step-b
    command: "echo world"
"#,
        )
        .unwrap();

    let runner = RecipeRunner::new(MockAdapter).with_listener(Box::new(listener));
    let result = runner.execute(&recipe, None);

    assert!(result.success);
    let s = started.lock().unwrap();
    let c = completed.lock().unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(c.len(), 2);
    assert!(s.contains(&"step-a".to_string()));
    assert!(s.contains(&"step-b".to_string()));
    assert!(c.contains(&"step-a".to_string()));
    assert!(c.contains(&"step-b".to_string()));
}

// ═══════════════════════════════════════════════════════════════════════════
// CONTINUE_ON_ERROR + PARALLEL_GROUP (C2-RD-9)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_continue_on_error_in_parallel_group() {
    let r = parse_and_run(
        r#"
name: parallel-continue
steps:
  - id: pg-ok
    command: "echo ok"
    parallel_group: g1
  - id: pg-fail
    command: "FAIL"
    parallel_group: g1
    continue_on_error: true
  - id: after-group
    command: "echo after"
"#,
    );
    assert!(
        r.success,
        "Recipe should succeed despite failure in parallel group with continue_on_error"
    );
    assert_eq!(r.step_results.len(), 3);

    let pg_fail = r
        .step_results
        .iter()
        .find(|s| s.step_id == "pg-fail")
        .unwrap();
    assert_eq!(pg_fail.status, StepStatus::Failed);

    let after = r
        .step_results
        .iter()
        .find(|s| s.step_id == "after-group")
        .unwrap();
    assert_eq!(after.status, StepStatus::Completed);
}
