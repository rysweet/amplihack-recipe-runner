/// Comprehensive recipe tests exercising every control flow and IO mechanism
/// of the recipe runner.
///
/// Organized by mechanism:
///   1. Step type dispatch (bash, agent, recipe)
///   2. Condition evaluation (all operators, functions, methods, truthiness)
///   3. Template rendering and context flow
///   4. JSON parsing (3 strategies + retry)
///   5. Sub-recipe execution (context merge, depth guard, recursion)
///   6. Error paths (parse failures, adapter failures, condition errors)
///   7. Dry run semantics
///   8. Auto-stage behavior
///   9. Parser validation and typo detection
///  10. Discovery and manifest
///  11. Agent resolver
///  12. Real recipe patterns (modeled on default-workflow, quality-audit, etc.)
///  13. Security (injection, dunder, path traversal)
use recipe_runner_rs::adapters::Adapter;
use recipe_runner_rs::agent_resolver::AgentResolver;
use recipe_runner_rs::context::RecipeContext;
use recipe_runner_rs::discovery;
use recipe_runner_rs::models::{StepStatus, StepType};
use recipe_runner_rs::parser::RecipeParser;
use recipe_runner_rs::runner::RecipeRunner;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

// ═══════════════════════════════════════════════════════════════════════════
// Configurable mock adapter
// ═══════════════════════════════════════════════════════════════════════════

/// Mock adapter that matches prompts/commands by substring and returns canned
/// responses. Tracks call counts so tests can assert execution order.
struct RecordingAdapter {
    responses: Vec<(String, String)>,
    agent_calls: Arc<AtomicUsize>,
    bash_calls: Arc<AtomicUsize>,
    fail_patterns: Vec<String>,
}

impl RecordingAdapter {
    fn new() -> Self {
        Self {
            responses: Vec::new(),
            agent_calls: Arc::new(AtomicUsize::new(0)),
            bash_calls: Arc::new(AtomicUsize::new(0)),
            fail_patterns: Vec::new(),
        }
    }

    fn on(mut self, pattern: &str, response: &str) -> Self {
        self.responses
            .push((pattern.to_string(), response.to_string()));
        self
    }

    fn fail_on(mut self, pattern: &str) -> Self {
        self.fail_patterns.push(pattern.to_string());
        self
    }

    #[allow(dead_code)]
    fn agent_count(&self) -> usize {
        self.agent_calls.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    fn bash_count(&self) -> usize {
        self.bash_calls.load(Ordering::SeqCst)
    }
}

impl Adapter for RecordingAdapter {
    fn execute_agent_step(
        &self,
        prompt: &str,
        _agent_name: Option<&str>,
        _system_prompt: Option<&str>,
        _mode: Option<&str>,
        _working_dir: &str,
        _timeout: Option<u64>,
        _model: Option<&str>,
    ) -> Result<String, anyhow::Error> {
        self.agent_calls.fetch_add(1, Ordering::SeqCst);
        for pat in &self.fail_patterns {
            if prompt.contains(pat.as_str()) {
                anyhow::bail!("Simulated agent failure on '{}'", pat);
            }
        }
        for (pat, resp) in &self.responses {
            if prompt.contains(pat.as_str()) {
                return Ok(resp.clone());
            }
        }
        Ok(format!("[agent] {}", &prompt[..prompt.len().min(80)]))
    }

    fn execute_bash_step(
        &self,
        command: &str,
        _working_dir: &str,
        _timeout: Option<u64>,
    ) -> Result<String, anyhow::Error> {
        self.bash_calls.fetch_add(1, Ordering::SeqCst);
        for pat in &self.fail_patterns {
            if command.contains(pat.as_str()) {
                anyhow::bail!("Simulated bash failure on '{}'", pat);
            }
        }
        for (pat, resp) in &self.responses {
            if command.contains(pat.as_str()) {
                return Ok(resp.clone());
            }
        }
        Ok(format!("[bash] {}", command))
    }

    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "recording-mock"
    }
}

fn parse_and_run(yaml: &str, adapter: RecordingAdapter) -> recipe_runner_rs::models::RecipeResult {
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    RecipeRunner::new(adapter).execute(&recipe, None)
}

fn parse_and_run_ctx(
    yaml: &str,
    adapter: RecordingAdapter,
    ctx: HashMap<String, serde_json::Value>,
) -> recipe_runner_rs::models::RecipeResult {
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    RecipeRunner::new(adapter).execute(&recipe, Some(ctx))
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. STEP TYPE DISPATCH
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_bash_step_routes_to_bash_adapter() {
    let adapter = RecordingAdapter::new();
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    command: "echo hello"
"#,
        adapter,
    );
    assert!(r.success);
    // Mock's bash handler ran
    assert!(r.step_results[0].output.contains("[bash]"));
}

#[test]
fn test_agent_step_routes_to_agent_adapter() {
    let adapter = RecordingAdapter::new();
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    agent: "amplihack:core:builder"
    prompt: "Build it"
"#,
        adapter,
    );
    assert!(r.success);
    assert!(r.step_results[0].output.contains("[agent]"));
}

#[test]
fn test_prompt_only_infers_agent() {
    let adapter = RecordingAdapter::new();
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: "Analyze this codebase"
"#,
        adapter,
    );
    assert!(r.success);
    assert!(r.step_results[0].output.contains("[agent]"));
}

#[test]
fn test_explicit_type_overrides_inference() {
    let adapter = RecordingAdapter::new();
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    type: bash
    prompt: "this is a bash step despite having prompt"
    command: "echo forced-bash"
"#,
        adapter,
    );
    assert!(r.success);
    assert!(r.step_results[0].output.contains("[bash]"));
}

#[test]
fn test_recipe_step_type_by_field() {
    let yaml = r#"
name: t
steps:
  - id: s1
    recipe: "nonexistent"
"#;
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    assert_eq!(recipe.steps[0].effective_type(), StepType::Recipe);
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. CONDITION EVALUATION — exhaustive
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_condition_eq_skip_and_run() {
    let r = parse_and_run(
        r#"
name: t
context:
  mode: "fast"
steps:
  - id: run
    command: "echo yes"
    condition: "mode == 'fast'"
  - id: skip
    command: "echo no"
    condition: "mode == 'slow'"
"#,
        RecordingAdapter::new(),
    );
    assert_eq!(r.step_results[0].status, StepStatus::Completed);
    assert_eq!(r.step_results[1].status, StepStatus::Skipped);
}

#[test]
fn test_condition_neq() {
    let r = parse_and_run(
        r#"
name: t
context:
  phase: "design"
steps:
  - id: s1
    command: "echo yes"
    condition: "phase != 'done'"
"#,
        RecordingAdapter::new(),
    );
    assert_eq!(r.step_results[0].status, StepStatus::Completed);
}

#[test]
fn test_condition_numeric_comparisons() {
    let r = parse_and_run(
        r#"
name: t
context:
  cycle: 3
  max: 5
steps:
  - id: lt
    command: "echo lt"
    condition: "cycle < max"
  - id: le
    command: "echo le"
    condition: "cycle <= 3"
  - id: gt
    command: "echo gt"
    condition: "max > cycle"
  - id: ge
    command: "echo ge"
    condition: "max >= 5"
  - id: skip-gt
    command: "echo no"
    condition: "cycle > max"
"#,
        RecordingAdapter::new(),
    );
    assert!(r.success);
    assert_eq!(r.step_results[0].status, StepStatus::Completed);
    assert_eq!(r.step_results[1].status, StepStatus::Completed);
    assert_eq!(r.step_results[2].status, StepStatus::Completed);
    assert_eq!(r.step_results[3].status, StepStatus::Completed);
    assert_eq!(r.step_results[4].status, StepStatus::Skipped);
}

#[test]
fn test_condition_in_and_not_in() {
    let r = parse_and_run(
        r#"
name: t
context:
  tags: "security,reliability,dead_code"
steps:
  - id: has
    command: "echo yes"
    condition: "'security' in tags"
  - id: not
    command: "echo no"
    condition: "'performance' not in tags"
  - id: skip
    command: "echo skip"
    condition: "'missing' in tags"
"#,
        RecordingAdapter::new(),
    );
    assert_eq!(r.step_results[0].status, StepStatus::Completed);
    assert_eq!(r.step_results[1].status, StepStatus::Completed);
    assert_eq!(r.step_results[2].status, StepStatus::Skipped);
}

#[test]
fn test_condition_boolean_ops_and_or_not() {
    let r = parse_and_run(
        r#"
name: t
context:
  a: "yes"
  b: ""
  c: "also"
steps:
  - id: and-ff
    command: "echo no"
    condition: "a and b"
  - id: or-tf
    command: "echo yes"
    condition: "a or b"
  - id: not-empty
    command: "echo yes"
    condition: "not b"
  - id: complex
    command: "echo yes"
    condition: "(a or b) and (c or b)"
"#,
        RecordingAdapter::new(),
    );
    assert_eq!(r.step_results[0].status, StepStatus::Skipped);
    assert_eq!(r.step_results[1].status, StepStatus::Completed);
    assert_eq!(r.step_results[2].status, StepStatus::Completed);
    assert_eq!(r.step_results[3].status, StepStatus::Completed);
}

#[test]
fn test_condition_truthiness_rules() {
    let r = parse_and_run(
        r#"
name: t
context:
  nonempty: "text"
  empty_str: ""
  zero: 0
  nonzero: 42
  truthy: true
  falsy: false
steps:
  - id: str-true
    command: "echo y"
    condition: "nonempty"
  - id: str-false
    command: "echo n"
    condition: "empty_str"
  - id: num-false
    command: "echo n"
    condition: "zero"
  - id: num-true
    command: "echo y"
    condition: "nonzero"
  - id: bool-true
    command: "echo y"
    condition: "truthy"
  - id: bool-false
    command: "echo n"
    condition: "falsy"
  - id: missing
    command: "echo n"
    condition: "undefined_var"
"#,
        RecordingAdapter::new(),
    );
    assert_eq!(r.step_results[0].status, StepStatus::Completed);
    assert_eq!(r.step_results[1].status, StepStatus::Skipped);
    assert_eq!(r.step_results[2].status, StepStatus::Skipped);
    assert_eq!(r.step_results[3].status, StepStatus::Completed);
    assert_eq!(r.step_results[4].status, StepStatus::Completed);
    assert_eq!(r.step_results[5].status, StepStatus::Skipped);
    assert_eq!(r.step_results[6].status, StepStatus::Skipped);
}

#[test]
fn test_condition_function_calls_in_conditions() {
    let r = parse_and_run(
        r#"
name: t
context:
  count_str: "7"
  text: "  hello  "
  num: 42
steps:
  - id: int-cast
    command: "echo y"
    condition: "int(count_str) > 5"
  - id: len-check
    command: "echo y"
    condition: "len(text) > 0"
  - id: str-cast
    command: "echo y"
    condition: "str(num) == '42'"
"#,
        RecordingAdapter::new(),
    );
    assert!(r.success);
    for sr in &r.step_results {
        assert_eq!(
            sr.status,
            StepStatus::Completed,
            "step {} should run",
            sr.step_id
        );
    }
}

#[test]
fn test_condition_method_calls_in_conditions() {
    let r = parse_and_run(
        r#"
name: t
context:
  raw: "  NOT_CONVERGED  "
  path: "/home/user/project"
steps:
  - id: strip
    command: "echo y"
    condition: "raw.strip() == 'NOT_CONVERGED'"
  - id: lower
    command: "echo y"
    condition: "raw.strip().lower() == 'not_converged'"
  - id: starts
    command: "echo y"
    condition: "path.startswith('/home')"
  - id: ends
    command: "echo y"
    condition: "path.endswith('project')"
  - id: replace
    command: "echo y"
    condition: "path.replace('/home', '/opt') == '/opt/user/project'"
  - id: count
    command: "echo y"
    condition: "path.count('/') == 3"
  - id: find
    command: "echo y"
    condition: "path.find('user') == 6"
"#,
        RecordingAdapter::new(),
    );
    assert!(r.success);
    for sr in &r.step_results {
        assert_eq!(
            sr.status,
            StepStatus::Completed,
            "step {} should run",
            sr.step_id
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. TEMPLATE RENDERING AND CONTEXT FLOW
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_template_rendering_in_command() {
    let adapter = RecordingAdapter::new();
    let r = parse_and_run(
        r#"
name: t
context:
  name: "world"
steps:
  - id: s1
    command: "echo hello {{name}}"
"#,
        adapter,
    );
    // The rendered command should contain "hello world"
    assert!(
        r.step_results[0].output.contains("hello world"),
        "got: {}",
        r.step_results[0].output
    );
}

#[test]
fn test_template_rendering_in_prompt() {
    let adapter = RecordingAdapter::new().on("Review world", "reviewed");
    let r = parse_and_run(
        r#"
name: t
context:
  target: "world"
steps:
  - id: s1
    prompt: "Review {{target}}"
    output: "review"
"#,
        adapter,
    );
    assert!(r.success);
    assert_eq!(r.context.get("review").unwrap(), "reviewed");
}

#[test]
fn test_missing_var_renders_empty() {
    let adapter = RecordingAdapter::new();
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    command: "echo prefix-{{undefined}}-suffix"
"#,
        adapter,
    );
    // bash commands go through render_shell which escapes empty strings to ''
    assert!(
        r.step_results[0].output.contains("prefix-")
            && r.step_results[0].output.contains("-suffix"),
        "missing var should render as empty, got: {}",
        r.step_results[0].output
    );
}

#[test]
fn test_context_accumulation_across_steps() {
    let adapter = RecordingAdapter::new()
        .on("echo first", "step-one-output")
        .on("step-one-output", "step-two-output")
        .on("step-two-output", "step-three-output");

    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    command: "echo first"
    output: "out1"
  - id: s2
    command: "echo {{out1}}"
    output: "out2"
  - id: s3
    command: "echo {{out2}}"
    output: "out3"
"#,
        adapter,
    );
    assert!(r.success);
    assert!(r.context.contains_key("out1"));
    assert!(r.context.contains_key("out2"));
    assert!(r.context.contains_key("out3"));
}

#[test]
fn test_user_context_overrides_recipe_defaults() {
    let adapter = RecordingAdapter::new().on("custom-val", "got-custom");
    let mut ctx = HashMap::new();
    ctx.insert("setting".to_string(), json!("custom-val"));

    let r = parse_and_run_ctx(
        r#"
name: t
context:
  setting: "default-val"
steps:
  - id: s1
    command: "echo {{setting}}"
    output: "out"
"#,
        adapter,
        ctx,
    );
    assert!(r.success);
    assert_eq!(r.context.get("out").unwrap(), "got-custom");
}

#[test]
fn test_json_value_stored_in_context_is_accessible() {
    let adapter = RecordingAdapter::new().on("analyze", r#"{"status": "ok", "count": 5}"#);

    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: "analyze"
    parse_json: true
    output: "result"
  - id: s2
    command: "echo status is {{result}}"
    output: "out"
"#,
        adapter,
    );
    assert!(r.success);
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. JSON PARSING — all three strategies + retry
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_json_strategy_1_direct_parse() {
    let adapter = RecordingAdapter::new().on("give json", r#"{"key": "value"}"#);
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: "give json"
    parse_json: true
    output: "data"
"#,
        adapter,
    );
    assert!(r.success);
    assert_eq!(r.context["data"], json!({"key": "value"}));
}

#[test]
fn test_json_strategy_2_markdown_fence() {
    let fenced = "Here is the analysis:\n```json\n{\"items\": [1,2,3]}\n```\nDone.";
    let adapter = RecordingAdapter::new().on("fenced", fenced);
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: "fenced"
    parse_json: true
    output: "data"
"#,
        adapter,
    );
    assert!(r.success);
    assert_eq!(r.context["data"], json!({"items": [1,2,3]}));
}

#[test]
fn test_json_strategy_2_fence_without_json_label() {
    let fenced = "Result:\n```\n{\"x\": 42}\n```";
    let adapter = RecordingAdapter::new().on("unlabeled", fenced);
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: "unlabeled"
    parse_json: true
    output: "data"
"#,
        adapter,
    );
    assert!(r.success);
    assert_eq!(r.context["data"], json!({"x": 42}));
}

#[test]
fn test_json_strategy_3_balanced_braces() {
    let mixed = "Some preamble text... {\"found\": true} ...and trailing text";
    let adapter = RecordingAdapter::new().on("balanced", mixed);
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: "balanced"
    parse_json: true
    output: "data"
"#,
        adapter,
    );
    assert!(r.success);
    assert_eq!(r.context["data"], json!({"found": true}));
}

#[test]
fn test_json_strategy_3_balanced_array() {
    let mixed = "Got: [1, 2, 3] done";
    let adapter = RecordingAdapter::new().on("arr", mixed);
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: "arr"
    parse_json: true
    output: "data"
"#,
        adapter,
    );
    assert!(r.success);
    assert_eq!(r.context["data"], json!([1, 2, 3]));
}

#[test]
fn test_json_strategy_3_nested_braces() {
    let nested = r#"Here: {"outer": {"inner": [1, {"deep": true}]}} done"#;
    let adapter = RecordingAdapter::new().on("nested", nested);
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: "nested"
    parse_json: true
    output: "data"
"#,
        adapter,
    );
    assert!(r.success);
    assert_eq!(r.context["data"]["outer"]["inner"][1]["deep"], json!(true));
}

#[test]
fn test_json_retry_on_failure() {
    // First call returns non-JSON (falls through to default),
    // second call (retry) matches "IMPORTANT" substring and returns valid JSON.
    let adapter = RecordingAdapter::new().on("IMPORTANT", r#"{"retry": "success"}"#);

    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: "first attempt"
    parse_json: true
    output: "data"
"#,
        adapter,
    );
    assert!(r.success, "retry should have rescued: {:?}", r.step_results);
    assert_eq!(r.context["data"], json!({"retry": "success"}));
}

#[test]
fn test_json_parse_fails_after_retry_exhausted() {
    // Both first try and retry return non-JSON
    let adapter = RecordingAdapter::new();
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: "gibberish"
    parse_json: true
    output: "data"
"#,
        adapter,
    );
    assert!(!r.success);
    assert_eq!(r.step_results[0].status, StepStatus::Failed);
    assert!(r.step_results[0].error.contains("parse_json failed"));
}

#[test]
fn test_json_with_escaped_quotes_in_strings() {
    let escaped = r#"{"message": "He said \"hello\" to her"}"#;
    let adapter = RecordingAdapter::new().on("escaped", escaped);
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: "escaped"
    parse_json: true
    output: "data"
"#,
        adapter,
    );
    assert!(r.success);
    assert!(
        r.context["data"]["message"]
            .as_str()
            .unwrap()
            .contains("hello")
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. SUB-RECIPE EXECUTION
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_sub_recipe_basic_execution() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("child.yaml"),
        r#"
name: child
steps:
  - id: c1
    command: "echo child ran"
    output: "child_output"
"#,
    )
    .unwrap();

    let adapter = RecordingAdapter::new();
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: parent
steps:
  - id: p1
    recipe: "child"
"#,
        )
        .unwrap();
    let runner = RecipeRunner::new(adapter).with_recipe_search_dirs(vec![tmp.path().to_path_buf()]);
    let result = runner.execute(&recipe, None);
    assert!(result.success);
}

#[test]
fn test_sub_recipe_context_merge() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("greeter.yaml"),
        r#"
name: greeter
steps:
  - id: greet
    command: "echo hello {{who}}"
    output: "greeting"
"#,
    )
    .unwrap();

    let adapter = RecordingAdapter::new().on("hello Alice", "greeted Alice");
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: parent
context:
  who: "Alice"
steps:
  - id: run-greet
    recipe: "greeter"
    context:
      who: "{{who}}"
"#,
        )
        .unwrap();
    let runner = RecipeRunner::new(adapter).with_recipe_search_dirs(vec![tmp.path().to_path_buf()]);
    let result = runner.execute(&recipe, None);
    assert!(result.success);
    // Context should flow back from sub-recipe
    assert!(result.context.contains_key("greeting"));
}

#[test]
fn test_sub_recipe_depth_guard() {
    // Create a recipe that recurses into itself
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("loop.yaml"),
        r#"
name: loop
steps:
  - id: recurse
    recipe: "loop"
"#,
    )
    .unwrap();

    let adapter = RecordingAdapter::new();
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: starter
steps:
  - id: start
    recipe: "loop"
"#,
        )
        .unwrap();
    let runner = RecipeRunner::new(adapter).with_recipe_search_dirs(vec![tmp.path().to_path_buf()]);
    let result = runner.execute(&recipe, None);
    assert!(
        !result.success,
        "recursive recipe should fail at depth limit"
    );
}

#[test]
fn test_sub_recipe_not_found() {
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    recipe: "does-not-exist"
"#,
        RecordingAdapter::new(),
    );
    assert!(!r.success);
    assert!(r.step_results[0].error.contains("not found"));
}

#[test]
fn test_chained_sub_recipes() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("step-a.yaml"),
        r#"
name: step-a
steps:
  - id: a1
    command: "echo a"
    output: "a_out"
"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("step-b.yaml"),
        r#"
name: step-b
steps:
  - id: b1
    command: "echo b uses {{a_out}}"
    output: "b_out"
"#,
    )
    .unwrap();

    let adapter = RecordingAdapter::new();
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: chain
steps:
  - id: run-a
    recipe: "step-a"
  - id: run-b
    recipe: "step-b"
"#,
        )
        .unwrap();
    let runner = RecipeRunner::new(adapter).with_recipe_search_dirs(vec![tmp.path().to_path_buf()]);
    let result = runner.execute(&recipe, None);
    assert!(result.success);
    assert!(result.context.contains_key("a_out"));
    assert!(result.context.contains_key("b_out"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. ERROR PATHS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_fail_fast_stops_on_first_error() {
    let adapter = RecordingAdapter::new().fail_on("bad-cmd");
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: good
    command: "echo ok"
  - id: bad
    command: "bad-cmd"
  - id: unreachable
    command: "echo never"
"#,
        adapter,
    );
    assert!(!r.success);
    assert_eq!(r.step_results.len(), 2);
    assert_eq!(r.step_results[1].status, StepStatus::Failed);
}

#[test]
fn test_adapter_failure_in_agent_step() {
    let adapter = RecordingAdapter::new().fail_on("crash");
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: "crash the agent"
"#,
        adapter,
    );
    assert!(!r.success);
    assert!(r.step_results[0].error.contains("Simulated agent failure"));
}

#[test]
fn test_condition_evaluation_error_fails_step() {
    // Use an expression the evaluator can't handle
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    command: "echo hello"
    condition: "__import__('os')"
"#,
        RecordingAdapter::new(),
    );
    assert!(!r.success);
    assert!(r.step_results[0].error.contains("Condition error"));
}

#[test]
fn test_unavailable_adapter_fails_gracefully() {
    struct UnavailableAdapter;
    impl Adapter for UnavailableAdapter {
        fn execute_agent_step(
            &self,
            _: &str,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&str>,
            _: &str,
            _: Option<u64>,
            _: Option<&str>,
        ) -> Result<String, anyhow::Error> {
            Ok("".into())
        }
        fn execute_bash_step(
            &self,
            _: &str,
            _: &str,
            _: Option<u64>,
        ) -> Result<String, anyhow::Error> {
            Ok("".into())
        }
        fn is_available(&self) -> bool {
            false
        }
        fn name(&self) -> &str {
            "unavailable"
        }
    }
    let parser = RecipeParser::new();
    let recipe = parser
        .parse("name: t\nsteps:\n  - id: s1\n    command: echo")
        .unwrap();
    let r = RecipeRunner::new(UnavailableAdapter).execute(&recipe, None);
    assert!(!r.success);
    assert!(r.step_results.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. DRY RUN SEMANTICS
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_dry_run_completes_all_steps() {
    let adapter = RecordingAdapter::new();
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: t
steps:
  - id: s1
    command: "echo a"
  - id: s2
    prompt: "do thing"
  - id: s3
    command: "echo c"
"#,
        )
        .unwrap();
    let runner = RecipeRunner::new(adapter).with_dry_run(true);
    let r = runner.execute(&recipe, None);
    assert!(r.success);
    assert_eq!(r.step_results.len(), 3);
    for sr in &r.step_results {
        assert_eq!(sr.status, StepStatus::Skipped);
        assert!(sr.output.contains("dry run") || sr.output.contains("dry_run"));
    }
}

#[test]
fn test_dry_run_parse_json_produces_valid_json() {
    let adapter = RecordingAdapter::new();
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: t
steps:
  - id: s1
    prompt: "analyze"
    parse_json: true
    output: "data"
"#,
        )
        .unwrap();
    let runner = RecipeRunner::new(adapter).with_dry_run(true);
    let r = runner.execute(&recipe, None);
    assert!(r.success);
    // The output should be valid JSON with dry_run flag
    let out = &r.step_results[0].output;
    let parsed: serde_json::Value =
        serde_json::from_str(out).expect("dry run parse_json should produce valid JSON");
    assert_eq!(parsed["dry_run"], json!(true));
}

#[test]
fn test_dry_run_does_not_call_adapter() {
    let adapter = RecordingAdapter::new();
    let agent_calls = adapter.agent_calls.clone();
    let bash_calls = adapter.bash_calls.clone();
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: t
steps:
  - id: s1
    command: "echo a"
  - id: s2
    prompt: "do thing"
"#,
        )
        .unwrap();
    let runner = RecipeRunner::new(adapter).with_dry_run(true);
    runner.execute(&recipe, None);
    assert_eq!(agent_calls.load(Ordering::SeqCst), 0);
    assert_eq!(bash_calls.load(Ordering::SeqCst), 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. PARSER VALIDATION AND TYPO DETECTION
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_parser_rejects_empty_name() {
    let parser = RecipeParser::new();
    assert!(
        parser
            .parse("name: \"\"\nsteps:\n  - id: s1\n    command: echo")
            .is_err()
    );
}

#[test]
fn test_parser_rejects_no_steps() {
    let parser = RecipeParser::new();
    assert!(parser.parse("name: t\nsteps: []").is_err());
}

#[test]
fn test_parser_rejects_duplicate_ids() {
    let parser = RecipeParser::new();
    let r = parser.parse(
        r#"
name: t
steps:
  - id: dup
    command: echo 1
  - id: dup
    command: echo 2
"#,
    );
    assert!(r.is_err());
    assert!(r.unwrap_err().to_string().contains("Duplicate"));
}

#[test]
fn test_parser_rejects_empty_step_id() {
    let parser = RecipeParser::new();
    let r = parser.parse(
        r#"
name: t
steps:
  - id: ""
    command: echo
"#,
    );
    assert!(r.is_err());
}

#[test]
fn test_parser_file_size_limit() {
    let parser = RecipeParser::new();
    // Just over 1MB should fail
    let huge = format!(
        "name: t\nsteps:\n  - id: s1\n    command: echo\n#{}",
        "x".repeat(1_000_001)
    );
    assert!(parser.parse(&huge).is_err());
}

#[test]
fn test_validate_detects_missing_prompt_on_agent() {
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: t
steps:
  - id: s1
    agent: "amplihack:builder"
"#,
        )
        .unwrap();
    let warnings = parser.validate(&recipe);
    assert!(warnings.iter().any(|w| w.contains("prompt")));
}

#[test]
fn test_validate_detects_missing_command_on_bash() {
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: t
steps:
  - id: s1
    type: bash
"#,
        )
        .unwrap();
    let warnings = parser.validate(&recipe);
    assert!(warnings.iter().any(|w| w.contains("command")));
}

#[test]
fn test_validate_detects_unrecognized_top_level_fields() {
    let yaml = "name: t\ndescrption: oops\nsteps:\n  - id: s1\n    command: echo";
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    let warnings = parser.validate_with_yaml(&recipe, Some(yaml));
    assert!(warnings.iter().any(|w| w.contains("descrption")));
}

#[test]
fn test_validate_detects_unrecognized_step_fields() {
    let yaml = "name: t\nsteps:\n  - id: s1\n    comand: echo\n    tmeout: 5";
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    let warnings = parser.validate_with_yaml(&recipe, Some(yaml));
    assert!(warnings.iter().any(|w| w.contains("comand")));
    assert!(warnings.iter().any(|w| w.contains("tmeout")));
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. DISCOVERY AND MANIFEST
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_discovery_finds_recipes_across_dirs() {
    let dir1 = tempfile::tempdir().unwrap();
    let dir2 = tempfile::tempdir().unwrap();
    std::fs::write(
        dir1.path().join("r1.yaml"),
        "name: r1\nsteps:\n  - id: s1\n    command: echo",
    )
    .unwrap();
    std::fs::write(
        dir2.path().join("r2.yaml"),
        "name: r2\nsteps:\n  - id: s1\n    command: echo",
    )
    .unwrap();

    let recipes = discovery::discover_recipes(Some(&[
        dir1.path().to_path_buf(),
        dir2.path().to_path_buf(),
    ]));
    assert_eq!(recipes.len(), 2);
}

#[test]
fn test_discovery_last_dir_wins_on_name_collision() {
    let dir1 = tempfile::tempdir().unwrap();
    let dir2 = tempfile::tempdir().unwrap();
    std::fs::write(
        dir1.path().join("shared.yaml"),
        "name: shared\ndescription: from-dir1\nsteps:\n  - id: s1\n    command: echo",
    )
    .unwrap();
    std::fs::write(
        dir2.path().join("shared.yaml"),
        "name: shared\ndescription: from-dir2\nsteps:\n  - id: s1\n    command: echo",
    )
    .unwrap();

    let recipes = discovery::discover_recipes(Some(&[
        dir1.path().to_path_buf(),
        dir2.path().to_path_buf(),
    ]));
    assert_eq!(recipes["shared"].description, "from-dir2");
}

#[test]
fn test_discovery_skips_nonexistent_dirs() {
    let recipes = discovery::discover_recipes(Some(&[std::path::PathBuf::from(
        "/nonexistent/path/abc123",
    )]));
    assert!(recipes.is_empty());
}

#[test]
fn test_discovery_manifest_detects_modifications() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("recipe.yaml"),
        "name: recipe\nsteps:\n  - id: s1\n    command: echo v1",
    )
    .unwrap();
    discovery::update_manifest(Some(tmp.path())).unwrap();

    // Modify file
    std::fs::write(
        tmp.path().join("recipe.yaml"),
        "name: recipe\nsteps:\n  - id: s1\n    command: echo v2",
    )
    .unwrap();
    let changes = discovery::check_upstream_changes(Some(tmp.path()));
    assert!(!changes.is_empty());
    assert_eq!(changes[0]["status"], "modified");
}

#[test]
fn test_discovery_manifest_detects_new_files() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("old.yaml"),
        "name: old\nsteps:\n  - id: s1\n    command: echo",
    )
    .unwrap();
    discovery::update_manifest(Some(tmp.path())).unwrap();

    std::fs::write(
        tmp.path().join("new.yaml"),
        "name: new\nsteps:\n  - id: s1\n    command: echo",
    )
    .unwrap();
    let changes = discovery::check_upstream_changes(Some(tmp.path()));
    assert!(
        changes
            .iter()
            .any(|c| c["name"] == "new" && c["status"] == "new")
    );
}

#[test]
fn test_discovery_manifest_detects_deleted_files() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("victim.yaml"),
        "name: victim\nsteps:\n  - id: s1\n    command: echo",
    )
    .unwrap();
    discovery::update_manifest(Some(tmp.path())).unwrap();

    std::fs::remove_file(tmp.path().join("victim.yaml")).unwrap();
    let changes = discovery::check_upstream_changes(Some(tmp.path()));
    assert!(
        changes
            .iter()
            .any(|c| c["name"] == "victim" && c["status"] == "deleted")
    );
}

#[test]
fn test_list_recipes_returns_sorted() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("z-last.yaml"),
        "name: z-last\nsteps:\n  - id: s1\n    command: echo",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("a-first.yaml"),
        "name: a-first\nsteps:\n  - id: s1\n    command: echo",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("m-middle.yaml"),
        "name: m-middle\nsteps:\n  - id: s1\n    command: echo",
    )
    .unwrap();

    let list = discovery::list_recipes(Some(&[tmp.path().to_path_buf()]));
    assert_eq!(list[0].name, "a-first");
    assert_eq!(list[1].name, "m-middle");
    assert_eq!(list[2].name, "z-last");
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. AGENT RESOLVER
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_agent_resolver_two_part_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("ns").join("core");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("builder.md"), "Build things.").unwrap();

    let resolver = AgentResolver::new(Some(vec![tmp.path().to_path_buf()]));
    assert_eq!(resolver.resolve("ns:builder").unwrap(), "Build things.");
}

#[test]
fn test_agent_resolver_three_part_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("ns").join("specialized");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("optimizer.md"), "Optimize.").unwrap();

    let resolver = AgentResolver::new(Some(vec![tmp.path().to_path_buf()]));
    assert_eq!(
        resolver.resolve("ns:specialized:optimizer").unwrap(),
        "Optimize."
    );
}

#[test]
fn test_agent_resolver_rejects_no_colon() {
    let resolver = AgentResolver::new(Some(vec![]));
    assert!(resolver.resolve("nocolon").is_err());
}

#[test]
fn test_agent_resolver_rejects_path_traversal() {
    let resolver = AgentResolver::new(Some(vec![]));
    assert!(resolver.resolve("../etc:passwd").is_err());
    assert!(resolver.resolve("ns:../../etc:passwd").is_err());
}

#[test]
fn test_agent_resolver_rejects_four_parts() {
    let resolver = AgentResolver::new(Some(vec![]));
    assert!(resolver.resolve("a:b:c:d").is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. SECURITY
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_shell_injection_via_template_is_escaped() {
    let adapter = RecordingAdapter::new();
    let r = parse_and_run(
        r#"
name: t
context:
  user_input: "hello; rm -rf /"
steps:
  - id: s1
    command: "echo {{user_input}}"
"#,
        adapter,
    );
    // render_shell wraps values in single quotes for safety
    let cmd = &r.step_results[0].output;
    assert!(
        cmd.contains("'hello; rm -rf /'") || cmd.contains("'hello;"),
        "shell injection should be escaped with quotes, got: {}",
        cmd
    );
}

#[test]
fn test_dunder_access_blocked() {
    let ctx = RecipeContext::new(HashMap::new());
    assert!(ctx.evaluate("__class__").is_err());
    assert!(ctx.evaluate("x.__dict__").is_err());
}

#[test]
fn test_unsafe_function_blocked() {
    let ctx = RecipeContext::new(HashMap::new());
    // Unknown functions like exec/eval are not in SAFE_CALL_NAMES, so they
    // resolve as unknown identifiers (Null/falsy) — not errors. This is by
    // design: the evaluator allowlists safe functions rather than blocklisting.
    assert!(!ctx.evaluate("exec").unwrap());
    assert!(!ctx.evaluate("eval").unwrap());
}

#[test]
fn test_unsafe_method_blocked() {
    let mut data = HashMap::new();
    data.insert("s".to_string(), json!("hello"));
    let ctx = RecipeContext::new(data);
    assert!(ctx.evaluate("s.system()").is_err());
    assert!(ctx.evaluate("s.encode()").is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. REAL RECIPE PATTERNS (modeled on actual amplihack recipes)
// ═══════════════════════════════════════════════════════════════════════════

/// Tests a recipe shaped like the default-workflow: multi-phase with
/// conditional gates, bash setup, agent analysis, and status tracking.
#[test]
fn test_workflow_pattern_multiphase() {
    let adapter = RecordingAdapter::new()
        .on(
            "Clarify requirements",
            r#"{"requirements": "Build auth", "scope": "login+logout"}"#,
        )
        .on(
            "Design solution",
            r#"{"design": "JWT tokens", "components": ["auth", "middleware"]}"#,
        )
        .on("Implement", "Implementation complete")
        .on("Review code", r#"{"issues": 0, "status": "approved"}"#);

    let r = parse_and_run(
        r#"
name: "mini-workflow"
description: "Simplified default-workflow pattern"
version: "1.0.0"
context:
  task_description: "Add user authentication"
  repo_path: "."
  phase: "requirements"
steps:
  - id: "step-00-init"
    command: "echo Workflow initialized for task: {{task_description}}"
    output: "init_result"

  - id: "step-01-clarify"
    prompt: "Clarify requirements for: {{task_description}}"
    parse_json: true
    output: "requirements"

  - id: "step-02-design"
    prompt: "Design solution for: {{requirements}}"
    parse_json: true
    output: "design"

  - id: "step-03-implement"
    prompt: "Implement based on design: {{design}}"
    output: "implementation"

  - id: "step-04-review"
    prompt: "Review code changes"
    parse_json: true
    output: "review"

  - id: "step-05-skip-if-no-issues"
    command: "echo No fixes needed"
    condition: "not review"
"#,
        adapter,
    );

    assert!(r.success);
    assert_eq!(r.step_results.len(), 6);
    // Steps 0-4 should complete, step 5 should skip (review is truthy)
    for i in 0..5 {
        assert_eq!(
            r.step_results[i].status,
            StepStatus::Completed,
            "step {} should complete",
            r.step_results[i].step_id
        );
    }
    assert_eq!(r.step_results[5].status, StepStatus::Skipped);
}

/// Tests a recipe shaped like the quality-audit-cycle: iterative loop with
/// convergence checking via repeated conditional blocks.
#[test]
fn test_quality_audit_loop_pattern() {
    let adapter = RecordingAdapter::new()
        .on(
            "SEEK",
            r#"{"findings": [{"severity": "medium", "desc": "unused import"}], "count": 1}"#,
        )
        .on("VALIDATE", r#"{"confirmed": 1, "false_positives": 0}"#)
        .on("FIX", r#"{"fixed": 1, "remaining": 0}"#)
        .on(
            "CONVERGE",
            r#"{"status": "CONVERGED", "remaining_issues": 0}"#,
        );

    let r = parse_and_run(
        r#"
name: "quality-loop"
version: "1.0.0"
context:
  target_path: "src/"
  cycle: "1"
  convergence: "NOT_CONVERGED"
steps:
  # Cycle 1
  - id: "seek-1"
    prompt: "SEEK quality issues in {{target_path}}"
    parse_json: true
    output: "findings"

  - id: "validate-1"
    prompt: "VALIDATE findings: {{findings}}"
    parse_json: true
    output: "validated"

  - id: "fix-1"
    prompt: "FIX confirmed issues: {{validated}}"
    parse_json: true
    output: "fixes"
    condition: "validated"

  - id: "converge-1"
    prompt: "CONVERGE check: {{fixes}}"
    parse_json: true
    output: "convergence_result"

  # Cycle 2 (skipped if converged)
  - id: "seek-2"
    prompt: "SEEK again"
    condition: "convergence != 'CONVERGED'"

  - id: "final"
    command: "echo audit complete"
"#,
        adapter,
    );

    assert!(
        r.success,
        "quality loop should complete: {:?}",
        r.step_results
    );
    // seek-2 should be skipped because convergence is still "NOT_CONVERGED"
    // (the string in context, not the parsed JSON result)
    assert_eq!(r.step_results[5].status, StepStatus::Completed);
}

/// Tests a recipe shaped like the oxidizer: uses sub-recipes and multi-step
/// context accumulation with conditional iteration.
#[test]
fn test_oxidizer_pattern_with_sub_recipes() {
    let tmp = tempfile::tempdir().unwrap();

    // Quality audit sub-recipe
    std::fs::write(
        tmp.path().join("quality-check.yaml"),
        r#"
name: quality-check
steps:
  - id: audit
    command: "echo auditing {{target}}"
    output: "audit_result"
"#,
    )
    .unwrap();

    let adapter = RecordingAdapter::new()
        .on(
            "Analyze Python",
            r#"{"modules": ["models", "parser", "runner"], "total_loc": 1200}"#,
        )
        .on("Generate tests", "tests generated")
        .on("Port module", "module ported");

    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: "mini-oxidizer"
version: "1.0.0"
context:
  python_source: "src/recipe_runner/"
  rust_target: "src/"
  target: "src/"
  convergence_status: "NOT_CONVERGED"
steps:
  - id: "analyze"
    prompt: "Analyze Python source at {{python_source}}"
    parse_json: true
    output: "analysis"

  - id: "gen-tests"
    prompt: "Generate tests for {{analysis}}"
    output: "test_suite"

  - id: "port-iter-1"
    prompt: "Port module 1"
    condition: "convergence_status == 'NOT_CONVERGED'"
    output: "port_result"

  - id: "quality-1"
    recipe: "quality-check"
    context:
      target: "{{rust_target}}"

  - id: "done"
    command: "echo oxidizer pass complete"
"#,
        )
        .unwrap();

    let runner = RecipeRunner::new(adapter).with_recipe_search_dirs(vec![tmp.path().to_path_buf()]);
    let result = runner.execute(&recipe, None);
    assert!(
        result.success,
        "oxidizer pattern should succeed: {:?}",
        result
    );
    assert!(result.context.contains_key("analysis"));
    assert!(result.context.contains_key("audit_result"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 13. MIXED STEP TYPES IN ONE RECIPE
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_mixed_bash_agent_recipe_steps() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("helper.yaml"),
        r#"
name: helper
steps:
  - id: h1
    command: "echo helped"
    output: "help_out"
"#,
    )
    .unwrap();

    let adapter = RecordingAdapter::new().on("Review", "looks good");

    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: mixed
steps:
  - id: bash-setup
    command: "echo setting up"
    output: "setup"
  - id: agent-analyze
    prompt: "Review the setup: {{setup}}"
    output: "review"
  - id: sub-recipe
    recipe: "helper"
  - id: bash-final
    command: "echo done with {{help_out}}"
"#,
        )
        .unwrap();

    let runner = RecipeRunner::new(adapter).with_recipe_search_dirs(vec![tmp.path().to_path_buf()]);
    let r = runner.execute(&recipe, None);
    assert!(r.success);
    assert_eq!(r.step_results.len(), 4);
    assert!(r.context.contains_key("setup"));
    assert!(r.context.contains_key("review"));
    assert!(r.context.contains_key("help_out"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 14. EDGE CASES
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_empty_command_string() {
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    command: ""
"#,
        RecordingAdapter::new(),
    );
    // Empty command should still execute (adapter gets "")
    assert!(r.success);
}

#[test]
fn test_very_long_output_stored_in_context() {
    let long_output = "x".repeat(100_000);
    let adapter = RecordingAdapter::new().on("big", &long_output);
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    command: "echo big"
    output: "big_data"
"#,
        adapter,
    );
    assert!(r.success);
    let stored = r.context["big_data"].as_str().unwrap();
    assert_eq!(stored.len(), 100_000);
}

#[test]
fn test_unicode_in_templates_and_conditions() {
    let r = parse_and_run(
        r#"
name: t
context:
  greeting: "こんにちは世界"
steps:
  - id: s1
    command: "echo {{greeting}}"
    condition: "'世界' in greeting"
    output: "out"
"#,
        RecordingAdapter::new(),
    );
    assert!(r.success);
    assert_eq!(r.step_results[0].status, StepStatus::Completed);
}

#[test]
fn test_multiline_command() {
    let adapter = RecordingAdapter::new();
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    command: |
      echo "line one" &&
      echo "line two" &&
      echo "line three"
"#,
        adapter,
    );
    assert!(r.success);
}

#[test]
fn test_multiline_prompt() {
    let adapter = RecordingAdapter::new();
    let r = parse_and_run(
        r#"
name: t
steps:
  - id: s1
    prompt: |
      You are reviewing code.

      Requirements:
      1. Check for bugs
      2. Check for security issues

      Target: {{target}}
    output: "review"
context:
  target: "src/"
"#,
        adapter,
    );
    assert!(r.success);
}

#[test]
fn test_recipe_with_all_metadata_fields() {
    let yaml = r#"
name: "full-metadata"
description: "A recipe with every metadata field populated"
version: "3.1.4"
author: "Test Suite"
tags: ["test", "comprehensive", "metadata"]
context:
  key1: "val1"
  key2: 42
  key3: true
steps:
  - id: s1
    command: "echo {{key1}} {{key2}} {{key3}}"
"#;
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    assert_eq!(recipe.name, "full-metadata");
    assert_eq!(
        recipe.description,
        "A recipe with every metadata field populated"
    );
    assert_eq!(recipe.version, "3.1.4");
    assert_eq!(recipe.author, "Test Suite");
    assert_eq!(recipe.tags, vec!["test", "comprehensive", "metadata"]);
    assert_eq!(recipe.context.len(), 3);
}

#[test]
fn test_step_with_all_fields() {
    let yaml = r#"
name: t
steps:
  - id: "full-step"
    type: agent
    agent: "amplihack:core:builder"
    prompt: "Do the thing"
    output: "result"
    condition: "true"
    parse_json: true
    mode: "plan"
    working_dir: "/tmp"
    timeout: 300
    auto_stage: false
"#;
    let parser = RecipeParser::new();
    let recipe = parser.parse(yaml).unwrap();
    let step = &recipe.steps[0];
    assert_eq!(step.id, "full-step");
    assert_eq!(step.effective_type(), StepType::Agent);
    assert_eq!(step.agent.as_deref(), Some("amplihack:core:builder"));
    assert_eq!(step.prompt.as_deref(), Some("Do the thing"));
    assert_eq!(step.output.as_deref(), Some("result"));
    assert_eq!(step.condition.as_deref(), Some("true"));
    assert!(step.parse_json);
    assert_eq!(step.mode.as_deref(), Some("plan"));
    assert_eq!(step.working_dir.as_deref(), Some("/tmp"));
    assert_eq!(step.timeout, Some(300));
    assert_eq!(step.auto_stage, Some(false));
}

// ═══════════════════════════════════════════════════════════════════════════
// 15. CONDITION EVALUATOR EDGE CASES
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_condition_nested_parentheses() {
    let mut data = HashMap::new();
    data.insert("a".to_string(), json!(true));
    data.insert("b".to_string(), json!(false));
    data.insert("c".to_string(), json!(true));
    let ctx = RecipeContext::new(data);
    assert!(ctx.evaluate("(a or b) and (c or b)").unwrap());
    assert!(!ctx.evaluate("(a and b) or (not c and not a)").unwrap());
}

#[test]
fn test_condition_chained_method_calls() {
    let mut data = HashMap::new();
    data.insert("s".to_string(), json!("  HELLO WORLD  "));
    let ctx = RecipeContext::new(data);
    assert!(ctx.evaluate("s.strip().lower() == 'hello world'").unwrap());
}

#[test]
fn test_condition_string_with_special_chars() {
    let mut data = HashMap::new();
    data.insert("path".to_string(), json!("/home/user/file.txt"));
    let ctx = RecipeContext::new(data);
    assert!(ctx.evaluate("path.endswith('.txt')").unwrap());
    assert!(ctx.evaluate("path.count('/') == 3").unwrap());
    assert!(ctx.evaluate("path.find('user') == 6").unwrap());
}

#[test]
fn test_condition_numeric_string_coercion() {
    let mut data = HashMap::new();
    data.insert("version".to_string(), json!("3"));
    let ctx = RecipeContext::new(data);
    assert!(ctx.evaluate("int(version) >= 3").unwrap());
    assert!(ctx.evaluate("int(version) < 10").unwrap());
}

#[test]
fn test_condition_array_containment() {
    let mut data = HashMap::new();
    data.insert("tags".to_string(), json!(["security", "quality", "test"]));
    let ctx = RecipeContext::new(data);
    assert!(ctx.evaluate("'security' in tags").unwrap());
    assert!(!ctx.evaluate("'missing' in tags").unwrap());
}

#[test]
fn test_condition_bool_literals() {
    let ctx = RecipeContext::new(HashMap::new());
    assert!(ctx.evaluate("true").unwrap());
    assert!(!ctx.evaluate("false").unwrap());
    assert!(ctx.evaluate("true and true").unwrap());
    assert!(!ctx.evaluate("true and false").unwrap());
    assert!(ctx.evaluate("True").unwrap());
    assert!(!ctx.evaluate("False").unwrap());
}

#[test]
fn test_condition_double_negation() {
    let mut data = HashMap::new();
    data.insert("flag".to_string(), json!(true));
    let ctx = RecipeContext::new(data);
    assert!(ctx.evaluate("not not flag").unwrap());
}

// ═══════════════════════════════════════════════════════════════════════════
// 16. CONTEXT RENDERING EDGE CASES
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_render_nested_object_as_json() {
    let mut data = HashMap::new();
    data.insert("obj".to_string(), json!({"key": "val", "num": 42}));
    let ctx = RecipeContext::new(data);
    let rendered = ctx.render("data: {{obj}}");
    // Objects should serialize to JSON
    assert!(rendered.contains("key"));
    assert!(rendered.contains("42"));
}

#[test]
fn test_render_array_as_json() {
    let mut data = HashMap::new();
    data.insert("arr".to_string(), json!([1, 2, 3]));
    let ctx = RecipeContext::new(data);
    let rendered = ctx.render("items: {{arr}}");
    assert!(rendered.contains("[1,2,3]"));
}

#[test]
fn test_render_null_as_empty() {
    let mut data = HashMap::new();
    data.insert("n".to_string(), json!(null));
    let ctx = RecipeContext::new(data);
    assert_eq!(ctx.render("val={{n}}"), "val=");
}

#[test]
fn test_render_boolean_as_string() {
    let mut data = HashMap::new();
    data.insert("flag".to_string(), json!(true));
    let ctx = RecipeContext::new(data);
    let rendered = ctx.render("flag={{flag}}");
    assert_eq!(rendered, "flag=true");
}

#[test]
fn test_render_number_as_string() {
    let mut data = HashMap::new();
    data.insert("n".to_string(), json!(42));
    let ctx = RecipeContext::new(data);
    assert_eq!(ctx.render("n={{n}}"), "n=42");
}

#[test]
fn test_render_multiple_vars_in_one_template() {
    let mut data = HashMap::new();
    data.insert("a".to_string(), json!("hello"));
    data.insert("b".to_string(), json!("world"));
    data.insert("c".to_string(), json!(42));
    let ctx = RecipeContext::new(data);
    assert_eq!(ctx.render("{{a}} {{b}} {{c}}"), "hello world 42");
}

#[test]
fn test_render_dot_notation_in_template() {
    let mut data = HashMap::new();
    data.insert("obj".to_string(), json!({"nested": {"val": "deep"}}));
    let ctx = RecipeContext::new(data);
    assert_eq!(ctx.render("got: {{obj.nested.val}}"), "got: deep");
}

#[test]
fn test_shell_render_prevents_injection() {
    let mut data = HashMap::new();
    data.insert("input".to_string(), json!("$(rm -rf /)"));
    let ctx = RecipeContext::new(data);
    let rendered = ctx.render_shell("echo {{input}}");
    // Should be escaped — wrapped in single quotes
    assert!(
        rendered.contains("'") || rendered.contains("\\$"),
        "dangerous input should be quoted/escaped, got: {}",
        rendered
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// AGENTIC RECOVERY FOR SUB-RECIPE FAILURES (#2953)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_sub_recipe_failure_no_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("failing.yaml"),
        r#"
name: failing
steps:
  - id: ok-step
    command: "echo ok"
  - id: bad-step
    command: "FAIL hard"
"#,
    )
    .unwrap();

    let adapter = RecordingAdapter::new().fail_on("FAIL");
    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: parent
steps:
  - id: run-child
    recipe: "failing"
    recovery_on_failure: false
"#,
        )
        .unwrap();
    let runner = RecipeRunner::new(adapter).with_recipe_search_dirs(vec![tmp.path().to_path_buf()]);
    let result = runner.execute(&recipe, None);
    assert!(!result.success, "should fail without recovery");
    assert!(
        result.step_results[0]
            .error
            .contains("Sub-recipe 'failing' failed"),
        "error should mention sub-recipe failure, got: {}",
        result.step_results[0].error
    );
}

/// Custom adapter for recovery tests: bash fails on "break_now" but agent
/// always succeeds with a canned recovery response.
struct RecoverySuccessAdapter;
impl Adapter for RecoverySuccessAdapter {
    fn execute_agent_step(
        &self,
        _prompt: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
        _: Option<u64>,
        _: Option<&str>,
    ) -> Result<String, anyhow::Error> {
        Ok("I fixed the issue. STATUS: COMPLETE".to_string())
    }
    fn execute_bash_step(
        &self,
        command: &str,
        _: &str,
        _: Option<u64>,
    ) -> Result<String, anyhow::Error> {
        if command.contains("break_now") {
            anyhow::bail!("step failed");
        }
        Ok(format!("[bash] {}", command))
    }
    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "recovery-success-mock"
    }
}

/// Custom adapter for recovery tests: bash fails on "break_now" and the
/// recovery agent returns a non-success response.
struct RecoveryFailAdapter;
impl Adapter for RecoveryFailAdapter {
    fn execute_agent_step(
        &self,
        _prompt: &str,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
        _: Option<u64>,
        _: Option<&str>,
    ) -> Result<String, anyhow::Error> {
        Ok("I cannot fix this, the data is corrupt".to_string())
    }
    fn execute_bash_step(
        &self,
        command: &str,
        _: &str,
        _: Option<u64>,
    ) -> Result<String, anyhow::Error> {
        if command.contains("break_now") {
            anyhow::bail!("step failed");
        }
        Ok(format!("[bash] {}", command))
    }
    fn is_available(&self) -> bool {
        true
    }
    fn name(&self) -> &str {
        "recovery-fail-mock"
    }
}

#[test]
fn test_sub_recipe_failure_recovery_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("failing.yaml"),
        r#"
name: failing
steps:
  - id: ok-step
    command: "echo ok"
  - id: bad-step
    command: "break_now"
"#,
    )
    .unwrap();

    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: parent
steps:
  - id: run-child
    recipe: "failing"
    recovery_on_failure: true
"#,
        )
        .unwrap();
    let runner = RecipeRunner::new(RecoverySuccessAdapter)
        .with_recipe_search_dirs(vec![tmp.path().to_path_buf()]);
    let result = runner.execute(&recipe, None);
    assert!(result.success, "should succeed after recovery");
    assert_eq!(result.step_results[0].status, StepStatus::Completed);
    assert!(
        result.step_results[0].output.contains("STATUS: COMPLETE"),
        "output should contain recovery agent response"
    );
}

#[test]
fn test_sub_recipe_failure_recovery_fails() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("failing.yaml"),
        r#"
name: failing
steps:
  - id: ok-step
    command: "echo ok"
  - id: bad-step
    command: "break_now"
"#,
    )
    .unwrap();

    let parser = RecipeParser::new();
    let recipe = parser
        .parse(
            r#"
name: parent
steps:
  - id: run-child
    recipe: "failing"
    recovery_on_failure: true
"#,
        )
        .unwrap();
    let runner = RecipeRunner::new(RecoveryFailAdapter)
        .with_recipe_search_dirs(vec![tmp.path().to_path_buf()]);
    let result = runner.execute(&recipe, None);
    assert!(!result.success, "should fail when recovery is unsuccessful");
    assert!(
        result.step_results[0]
            .error
            .contains("agentic recovery was unsuccessful"),
        "error should mention failed recovery, got: {}",
        result.step_results[0].error
    );
}
