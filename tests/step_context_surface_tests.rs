//! #219: the recipe surface of lightweight agent steps, and the records they
//! leave behind.
//!
//! Two fields are added to a step:
//!
//! * `context_mode` — accepted on an agent step only, with exactly one
//!   accepted value, `minimal`. There is no value that spells "full": a step
//!   asks for the normal start by omitting the field.
//! * `effort` — free-form, rendered, accepted wherever `model` is and with no
//!   step-type restriction of any kind.
//!
//! Three fields are added to a step record — `context_mode`, `model` and
//! `effort` — each written only when it has something to say, so that a recipe
//! untouched by #219 produces the bytes it produced before.
//!
//! The launch half of the contract (which environment variables reach which
//! child) lives in `tests/step_context_contract_tests.rs`.
//!
//! Reference: `docs/src/step-context-contract.md`,
//! `docs/src/yaml-format.md#lightweight-agent-steps-context_mode`.

use recipe_runner_rs::models::{Recipe, StepContextMode, StepResult, StepType};
use recipe_runner_rs::parser::{ParseError, RecipeParser};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

// ── harness ────────────────────────────────────────────────────────────────

fn parse(yaml: &str) -> Result<Recipe, ParseError> {
    RecipeParser::new().without_field_warnings().parse(yaml)
}

/// The text of the error a bad recipe produces.
fn parse_error(yaml: &str) -> String {
    parse(yaml)
        .expect_err("this recipe must be rejected")
        .to_string()
}

fn warnings(yaml: &str) -> Vec<String> {
    recipe_runner_rs::validate_recipe(yaml).expect("recipe parses")
}

/// One agent step with whatever extra fields the test needs.
fn agent_step(extra: &str) -> String {
    format!("name: p\nsteps:\n  - id: a\n    type: agent\n    prompt: hi\n{extra}")
}

fn run_binary(dir: &Path, yaml: &str, args: &[&str]) -> (i32, String, String) {
    let recipe = dir.join("recipe.yaml");
    std::fs::write(&recipe, yaml).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .arg(&recipe)
        .args(args)
        .current_dir(dir)
        .env_remove("RUST_LOG")
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
        .output()
        .expect("failed to run recipe-runner-rs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ── the one accepted context mode ──────────────────────────────────────────

#[test]
fn context_mode_is_carried_as_written_and_typed_at_the_boundary() {
    // The field deserialises as a plain string so that the validation pass —
    // which has the step in hand — can name the step that owns a bad value.
    // The typed form exists for the code that acts on it.
    let recipe = parse(&agent_step("    context_mode: minimal\n")).unwrap();
    assert_eq!(recipe.steps[0].context_mode.as_deref(), Some("minimal"));
    assert_eq!(
        StepContextMode::parse("minimal"),
        Some(StepContextMode::Minimal)
    );
    assert_eq!(StepContextMode::Minimal.as_str(), "minimal");
    assert_eq!(StepContextMode::MINIMAL, "minimal");
}

#[test]
fn there_is_no_mode_that_spells_full() {
    // Absence is the one way to ask for the normal start. A second spelling
    // would be a second thing to keep consistent with the missing field.
    assert_eq!(StepContextMode::parse("full"), None);
    assert_eq!(StepContextMode::parse(""), None);
    assert_eq!(StepContextMode::parse("Minimal"), None);
}

#[test]
fn an_omitted_context_mode_is_none_and_so_is_an_omitted_effort() {
    let recipe = parse(&agent_step("")).unwrap();
    assert_eq!(recipe.steps[0].context_mode, None);
    assert_eq!(recipe.steps[0].effort, None);
}

#[test]
fn context_mode_is_accepted_on_a_step_that_infers_its_agent_type() {
    // `type:` is optional; the check is on the effective type, not the
    // written one.
    let recipe = parse("name: p\nsteps:\n  - id: a\n    prompt: hi\n    context_mode: minimal\n")
        .expect("an inferred agent step may declare a minimal start");
    assert_eq!(recipe.steps[0].effective_type(), StepType::Agent);
    assert_eq!(recipe.steps[0].context_mode.as_deref(), Some("minimal"));
}

// ── rejections ─────────────────────────────────────────────────────────────

#[test]
fn an_unrecognised_context_mode_is_rejected_naming_step_value_and_minimal() {
    // A typo must not silently cost the saving. The message is the contract
    // quoted in docs/src/yaml-format.md.
    let text = parse_error(
        "name: p\nsteps:\n  - id: classify\n    type: agent\n    prompt: hi\n    context_mode: mininal\n",
    );
    assert_eq!(
        text,
        "Invalid recipe: Step 'classify': context_mode 'mininal' is not recognised; \
         the only accepted value is 'minimal'",
        "the documented message changed: {text}"
    );
}

#[test]
fn full_is_rejected_like_any_other_unrecognised_value() {
    let text = parse_error(&agent_step("    context_mode: full\n"));
    assert!(text.contains("'full'"), "{text}");
    assert!(text.contains("'minimal'"), "{text}");
}

#[test]
fn context_mode_is_case_sensitive() {
    for value in ["Minimal", "MINIMAL", " minimal"] {
        let text = parse_error(&agent_step(&format!("    context_mode: \"{value}\"\n")));
        assert!(
            text.contains("minimal"),
            "{value} must be rejected against the one accepted spelling: {text}"
        );
    }
}

#[test]
fn context_mode_on_a_bash_step_is_rejected_naming_the_step() {
    let text = parse_error(
        "name: p\nsteps:\n  - id: build\n    type: bash\n    command: echo hi\n    context_mode: minimal\n",
    );
    assert_eq!(
        text, "Invalid recipe: Step 'build': context_mode is only valid on an agent step",
        "the documented message changed: {text}"
    );
}

#[test]
fn context_mode_on_a_recipe_step_is_rejected_naming_the_step() {
    let text = parse_error(
        "name: p\nsteps:\n  - id: child\n    recipe: helper\n    context_mode: minimal\n",
    );
    assert_eq!(
        text,
        "Invalid recipe: Step 'child': context_mode is only valid on an agent step"
    );
}

#[test]
fn a_rejected_context_mode_is_escaped_for_display() {
    // The value is a literal written in the recipe, so echoing it is useful;
    // it is escaped the way every other parse diagnostic escapes an
    // identifier, so a terminal escape sequence cannot reach stderr raw.
    let text = parse_error(&agent_step("    context_mode: \"\\u001b[31mred\"\n"));
    assert!(
        !text.contains('\u{1b}'),
        "no raw escape character may reach the message: {text:?}"
    );
    assert!(
        text.contains("\\u{1b}"),
        "the value is shown, escaped: {text:?}"
    );
}

#[test]
fn a_rejected_step_id_is_escaped_for_display() {
    let text = parse_error(
        "name: p\nsteps:\n  - id: \"step\\u001bone\"\n    type: bash\n    command: echo hi\n    context_mode: minimal\n",
    );
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(text.contains("step\\u{1b}one"), "{text:?}");
}

#[test]
fn validate_only_rejects_a_bad_context_mode_with_the_parse_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run_binary(
        dir.path(),
        "name: p\nsteps:\n  - id: classify\n    type: agent\n    prompt: hi\n    context_mode: lightweight\n",
        &["--validate-only"],
    );
    assert_eq!(code, 2, "stdout: {stdout}\nstderr: {stderr}");
    let text = format!("{stdout}{stderr}");
    assert!(text.contains("classify"), "{text}");
    assert!(text.contains("minimal"), "{text}");
}

#[test]
fn validate_only_rejects_context_mode_on_a_non_agent_step() {
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run_binary(
        dir.path(),
        "name: p\nsteps:\n  - id: build\n    type: bash\n    command: echo hi\n    context_mode: minimal\n",
        &["--validate-only"],
    );
    assert_eq!(code, 2, "stdout: {stdout}\nstderr: {stderr}");
    assert!(format!("{stdout}{stderr}").contains("build"));
}

#[test]
fn a_bad_context_mode_stops_an_ordinary_run_too() {
    // The check lives in the parser, so it fires on every parse rather than
    // only under --validate-only.
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run_binary(
        dir.path(),
        "name: p\nsteps:\n  - id: first\n    type: bash\n    command: \"touch ran\"\n  - id: classify\n    type: agent\n    prompt: hi\n    context_mode: lightweight\n",
        &[],
    );
    assert_eq!(code, 2, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        !dir.path().join("ran").exists(),
        "no step runs when the recipe is rejected"
    );
}

#[test]
fn a_parent_recipe_cannot_smuggle_a_bad_step_in_through_extends() {
    // `extends` concatenates the parent's steps, and the parent is read with
    // the same parser, so a bad step is caught where it is written.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("parent.yaml"),
        "name: parent\nsteps:\n  - id: build\n    type: bash\n    command: echo hi\n    context_mode: minimal\n",
    )
    .unwrap();
    let mut child =
        parse("name: child\nextends: parent\nsteps:\n  - id: a\n    type: agent\n    prompt: hi\n")
            .expect("the child itself is valid");
    let err = recipe_runner_rs::parser::resolve_extends(&mut child, &[dir.path().to_path_buf()])
        .expect_err("the parent's bad step must be reported");
    assert!(err.to_string().contains("build"), "{err}");
}

// ── effort is not restricted ───────────────────────────────────────────────

#[test]
fn effort_is_free_form_and_the_runner_holds_no_vocabulary() {
    // Accepted words differ per harness; amplihack owns the mapping.
    for value in ["low", "medium", "high", "xhigh", "whatever-codex-calls-it"] {
        let recipe = parse(&agent_step(&format!("    effort: {value}\n")))
            .unwrap_or_else(|e| panic!("effort: {value} must parse: {e}"));
        assert_eq!(recipe.steps[0].effort.as_deref(), Some(value));
    }
}

#[test]
fn effort_on_a_bash_step_is_accepted_in_silence() {
    // `model` on a bash step has always been silently unused. `effort` is the
    // same kind of field and behaves the same way: no error, no warning.
    let yaml =
        "name: p\nsteps:\n  - id: build\n    type: bash\n    command: echo hi\n    effort: low\n";
    parse(yaml).expect("effort carries no step-type restriction");
    let found = warnings(yaml);
    assert!(
        !found.iter().any(|w| w.contains("effort")),
        "effort draws no warning anywhere: {found:?}"
    );
}

#[test]
fn effort_on_a_recipe_step_is_accepted_in_silence() {
    for extra in ["", "    recovery_on_failure: true\n"] {
        let yaml =
            format!("name: p\nsteps:\n  - id: child\n    recipe: helper\n    effort: low\n{extra}");
        parse(&yaml).expect("effort carries no step-type restriction");
        let found = warnings(&yaml);
        assert!(
            !found.iter().any(|w| w.contains("effort")),
            "effort draws no warning anywhere: {found:?}"
        );
    }
}

#[test]
fn model_on_a_bash_step_is_still_accepted_in_silence() {
    // The baseline the rule above matches. Nothing about `model` changes.
    let yaml =
        "name: p\nsteps:\n  - id: build\n    type: bash\n    command: echo hi\n    model: haiku\n";
    parse(yaml).unwrap();
    let found = warnings(yaml);
    assert!(!found.iter().any(|w| w.contains("model")), "{found:?}");
}

#[test]
fn an_agent_step_declaring_both_fields_draws_no_warning() {
    let found = warnings(&agent_step("    context_mode: minimal\n    effort: low\n"));
    assert!(found.is_empty(), "{found:?}");
}

// ── the fields are known to the parser ─────────────────────────────────────

#[test]
fn neither_field_is_reported_as_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run_binary(
        dir.path(),
        &agent_step("    context_mode: minimal\n    effort: low\n"),
        &["--dry-run"],
    );
    assert_eq!(code, 0, "{stderr}");
    let text = format!("{stdout}{stderr}");
    assert!(
        !text.to_lowercase().contains("unknown field"),
        "both fields are known to the parser: {text}"
    );
}

#[test]
fn a_misspelled_key_is_still_only_a_warning() {
    // Unknown *names* stay warnings, as they always have been. Only a bad
    // *value* of a known field is a hard error.
    let found = warnings(&agent_step("    context_mdoe: minimal\n"));
    assert!(
        found.iter().any(|w| w.contains("context_mdoe")),
        "a misspelled key is reported: {found:?}"
    );
}

// ── the step id gains no new rule ──────────────────────────────────────────

#[test]
fn a_long_step_id_still_parses() {
    // Carrying the id to a child environment is not a reason to narrow what a
    // recipe may call its steps. An id that cannot travel costs one variable
    // on one launch, not a rejected recipe.
    let long = "s".repeat(300);
    let recipe = parse(&format!(
        "name: p\nsteps:\n  - id: {long}\n    type: agent\n    prompt: hi\n"
    ))
    .expect("no length cap is added to step ids");
    assert_eq!(recipe.steps[0].id.len(), 300);
}

#[test]
fn a_step_id_holding_a_control_character_still_parses() {
    let recipe =
        parse("name: p\nsteps:\n  - id: \"step\\u0007one\"\n    type: agent\n    prompt: hi\n")
            .expect("no character rule is added to step ids");
    assert!(recipe.steps[0].id.contains('\u{7}'));
}

// ── serialisation and the replay fingerprint ───────────────────────────────

/// `recovery/replay.rs` fingerprints the serialised `Step`. A field that
/// serialises when it was never declared would invalidate every stored
/// recovery record for every unchanged recipe.
fn step_keys(yaml: &str) -> BTreeSet<String> {
    let recipe = parse(yaml).unwrap();
    serde_json::to_value(&recipe.steps[0])
        .unwrap()
        .as_object()
        .expect("a step serialises as an object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn the_serialised_step_is_unchanged_for_a_recipe_written_before_219() {
    let before: BTreeSet<String> = [
        "recovery",
        "id",
        "type",
        "command",
        "agent",
        "prompt",
        "output",
        "condition",
        "parse_json",
        "parse_json_required",
        "mode",
        "working_dir",
        "timeout",
        "auto_stage",
        "recipe",
        "context",
        "continue_on_error",
        "fatal",
        "parallel_group",
        "when_tags",
        "recovery_on_failure",
        "model",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(
        step_keys(&agent_step("")),
        before,
        "an unchanged step must serialise exactly as it did, or every stored \
         recovery record is invalidated"
    );
}

#[test]
fn the_existing_model_field_still_serialises_as_null_when_absent() {
    // The inverse danger: adding skip_serializing_if to `model` would break
    // the same stored records from the other direction.
    assert!(
        step_keys(&agent_step("")).contains("model"),
        "stored fingerprints already carry \"model\":null"
    );
}

#[test]
fn a_declared_field_does_appear_in_the_fingerprint() {
    // A record obtained under a normal start must not be replayed for a
    // minimal one, so both fields are inside the fingerprint when declared.
    let plain = serde_json::to_string(&parse(&agent_step("")).unwrap().steps[0]).unwrap();
    for extra in ["    context_mode: minimal\n", "    effort: low\n"] {
        let declared = serde_json::to_string(&parse(&agent_step(extra)).unwrap().steps[0]).unwrap();
        assert_ne!(plain, declared, "{extra} must change the fingerprint");
    }
    let low =
        serde_json::to_string(&parse(&agent_step("    effort: low\n")).unwrap().steps[0]).unwrap();
    let high =
        serde_json::to_string(&parse(&agent_step("    effort: high\n")).unwrap().steps[0]).unwrap();
    assert_ne!(low, high);
}

// ── the step record type ───────────────────────────────────────────────────

#[test]
fn a_default_step_result_carries_none_of_the_three() {
    let result = StepResult {
        step_id: "a".to_string(),
        ..Default::default()
    };
    assert_eq!(result.context_mode, None);
    assert_eq!(result.model, None);
    assert_eq!(result.effort, None);
    let value = serde_json::to_value(&result).unwrap();
    let keys: BTreeSet<&String> = value.as_object().unwrap().keys().collect();
    for added in ["context_mode", "model", "effort"] {
        assert!(!keys.contains(&added.to_string()), "{keys:?}");
    }
}

#[test]
fn an_old_record_without_the_three_fields_still_deserialises() {
    // The recovery store round-trips records written before #219.
    let old = r#"{"step_id":"a","status":"completed","output":"","error":""}"#;
    let result: StepResult = serde_json::from_str(old).unwrap();
    assert_eq!(result.step_id, "a");
    assert_eq!(result.context_mode, None);
    assert_eq!(result.model, None);
    assert_eq!(result.effort, None);
}

// ── step records, end to end ───────────────────────────────────────────────

fn write_echo_launcher(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let launcher = dir.join("amplihack");
    std::fs::write(&launcher, "#!/bin/sh\ncat >/dev/null\nprintf 'ok'\n").unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    launcher
}

/// Run `yaml` through a launcher that answers `ok`, and return both record
/// surfaces: the `step_results` array of `--output-format json` and the audit
/// JSONL lines.
fn records(dir: &Path, yaml: &str) -> (Vec<Value>, Vec<Value>) {
    let launcher = write_echo_launcher(dir);
    let audit = dir.join("audit");
    std::fs::create_dir_all(&audit).unwrap();
    let recipe = dir.join("recipe.yaml");
    std::fs::write(&recipe, yaml).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .arg(&recipe)
        .args(["--output-format", "json"])
        .args(["--audit-dir", audit.to_str().unwrap()])
        .current_dir(dir)
        .env_remove("RUST_LOG")
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
        .env("AMPLIHACK_RATELIMIT_MAX_RETRIES", "0")
        .env("AMPLIHACK_LAUNCHER_BINARY", &launcher)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value = serde_json::from_str(
        &stdout[stdout.find('{').unwrap_or_else(|| {
            panic!(
                "no JSON result\nstdout: {stdout}\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        })..],
    )
    .unwrap();
    let json_records = parsed["step_results"]
        .as_array()
        .expect("step_results array")
        .clone();
    let audit_records: Vec<Value> = std::fs::read_dir(&audit)
        .unwrap()
        .filter_map(Result::ok)
        .flat_map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap_or_default()
                .lines()
                .filter(|l| l.contains("\"step_id\""))
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .collect::<Vec<_>>()
        })
        .collect();
    (json_records, audit_records)
}

fn keys_of(value: &Value) -> BTreeSet<String> {
    value.as_object().unwrap().keys().cloned().collect()
}

#[test]
fn a_lightweight_step_records_all_three_on_both_surfaces() {
    let dir = tempfile::tempdir().unwrap();
    let (json, audit) = records(
        dir.path(),
        "name: r\ncontext:\n  tier: haiku\nsteps:\n  - id: a\n    type: agent\n    prompt: hi\n    context_mode: minimal\n    model: \"{{tier}}\"\n    effort: low\n",
    );
    for (surface, record) in [("json", &json[0]), ("audit", &audit[0])] {
        assert_eq!(record["context_mode"], Value::from("minimal"), "{surface}");
        assert_eq!(
            record["model"],
            Value::from("haiku"),
            "{surface}: the record shows the resolved model, not the template"
        );
        assert_eq!(record["effort"], Value::from("low"), "{surface}");
    }
}

#[test]
fn a_step_declaring_none_of_the_three_records_none_of_them() {
    let dir = tempfile::tempdir().unwrap();
    let (json, audit) = records(
        dir.path(),
        "name: r\nsteps:\n  - id: a\n    type: agent\n    prompt: hi\n",
    );
    for (surface, record) in [("json", &json[0]), ("audit", &audit[0])] {
        for added in ["context_mode", "model", "effort"] {
            assert!(
                !keys_of(record).contains(added),
                "{surface}: an unchanged step record gains no key: {record}"
            );
        }
    }
}

#[test]
fn an_unchanged_bash_step_writes_the_audit_line_it_always_wrote() {
    // `json!` writes null for an absent Option, which would append three keys
    // to every audit line in the repository.
    let dir = tempfile::tempdir().unwrap();
    let (_, audit) = records(
        dir.path(),
        "name: r\nsteps:\n  - id: a\n    type: bash\n    command: echo hi\n",
    );
    let before: BTreeSet<String> = ["step_id", "status", "duration_ms", "error", "output_len"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        keys_of(&audit[0]),
        before,
        "the audit line gains no key: {}",
        audit[0]
    );
}

#[test]
fn a_step_declaring_only_a_model_records_only_the_model() {
    // The comparison arm: the saving is measured against steps that did not
    // opt in.
    let dir = tempfile::tempdir().unwrap();
    let (json, _) = records(
        dir.path(),
        "name: r\nsteps:\n  - id: a\n    type: agent\n    prompt: hi\n    model: sonnet\n",
    );
    assert_eq!(json[0]["model"], Value::from("sonnet"));
    assert!(!keys_of(&json[0]).contains("context_mode"), "{}", json[0]);
    assert!(!keys_of(&json[0]).contains("effort"), "{}", json[0]);
}

#[test]
fn a_failed_lightweight_step_still_records_how_it_was_launched() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let launcher = dir.path().join("amplihack");
    std::fs::write(&launcher, "#!/bin/sh\ncat >/dev/null\nexit 3\n").unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let recipe = dir.path().join("recipe.yaml");
    std::fs::write(
        &recipe,
        "name: r\nsteps:\n  - id: a\n    type: agent\n    prompt: hi\n    context_mode: minimal\n    model: haiku\n    effort: low\n    continue_on_error: true\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .arg(&recipe)
        .args(["--output-format", "json"])
        .current_dir(dir.path())
        .env_remove("RUST_LOG")
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
        .env("AMPLIHACK_RATELIMIT_MAX_RETRIES", "0")
        .env("AMPLIHACK_LAUNCHER_BINARY", &launcher)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value =
        serde_json::from_str(&stdout[stdout.find('{').expect("JSON result")..]).unwrap();
    let record = &parsed["step_results"][0];
    assert_eq!(record["status"], Value::from("failed"), "{record}");
    assert_eq!(record["context_mode"], Value::from("minimal"));
    assert_eq!(record["model"], Value::from("haiku"));
    assert_eq!(record["effort"], Value::from("low"));
}

#[test]
fn a_step_that_never_ran_records_none_of_the_three() {
    let dir = tempfile::tempdir().unwrap();
    let (json, _) = records(
        dir.path(),
        "name: r\nsteps:\n  - id: skipped\n    type: agent\n    prompt: hi\n    context_mode: minimal\n    model: haiku\n    effort: low\n    condition: \"false\"\n",
    );
    assert_eq!(json[0]["status"], Value::from("skipped"), "{}", json[0]);
    for added in ["context_mode", "model", "effort"] {
        assert!(
            !keys_of(&json[0]).contains(added),
            "a step that never launched reports no launch: {}",
            json[0]
        );
    }
}

// ── existing recipes ───────────────────────────────────────────────────────

fn recipe_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            recipe_files(&path, found);
        } else if path.extension().is_some_and(|e| e == "yaml" || e == "yml") {
            found.push(path);
        }
    }
}

fn shipped_recipes() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    recipe_files(&root.join("recipes"), &mut found);
    recipe_files(&root.join("examples"), &mut found);
    assert!(!found.is_empty(), "some recipes were read");
    found
}

#[test]
fn no_shipped_recipe_opts_in_yet() {
    // Criterion 5: unchanged recipes behave exactly as before. Converting them
    // is amplihack's and the factory's work, not this change's.
    for path in shipped_recipes() {
        let text = std::fs::read_to_string(&path).unwrap();
        for line in text.lines() {
            assert!(
                !line.trim_start().starts_with("context_mode:"),
                "{} opts in: {line}",
                path.display()
            );
        }
    }
}

#[test]
fn every_shipped_recipe_still_parses() {
    // `context:` keeps its present meaning: the sub-recipe input map. The new
    // field is called `context_mode` precisely so that no shipped recipe has
    // to change.
    for path in shipped_recipes() {
        let text = std::fs::read_to_string(&path).unwrap();
        // Some fixtures are deliberately unparseable; a recipe that parsed
        // before #219 must still parse.
        if let Err(e) = parse(&text) {
            assert!(
                !e.to_string().contains("context_mode"),
                "{} is rejected by the new check: {e}",
                path.display()
            );
        }
    }
}
