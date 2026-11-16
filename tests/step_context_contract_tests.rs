//! #219: the per-step launch contract for lightweight agent steps.
//!
//! The runner declares three facts about an agent step on the child process
//! environment and nowhere else:
//!
//! | Variable                 | Set when                                     |
//! | ------------------------ | -------------------------------------------- |
//! | `AMPLIHACK_STEP_CONTEXT` | the step declared `context_mode: minimal`    |
//! | `AMPLIHACK_STEP_EFFORT`  | the rendered `effort` is non-empty           |
//! | `AMPLIHACK_STEP_ID`      | every agent launch, bar an unusable id       |
//!
//! The runner does not implement the minimal start: it trims no prompt, drops
//! no system prompt and changes no argv token on account of `context_mode`.
//! These tests therefore assert what reaches a stand-in launcher's environment
//! and argument vector, never what an agent does with it.
//!
//! The end-to-end tests run the built binary against a launcher script that
//! records each call, following `tests/env_allow_tests.rs` and
//! `tests/model_propagation_tests.rs`. The last section drives the adapter
//! trait directly, with stand-in adapters.
//!
//! Reference: `docs/src/step-context-contract.md`.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CONTEXT: &str = "AMPLIHACK_STEP_CONTEXT";
const EFFORT: &str = "AMPLIHACK_STEP_EFFORT";
const STEP_ID: &str = "AMPLIHACK_STEP_ID";

// ── harness ────────────────────────────────────────────────────────────────

/// A launcher stand-in that records the argument vector and environment of
/// each call under `dir`, then answers with `ok`.
///
/// Call `n` writes `argv-<n>` (NUL-separated arguments) and `env-<n>`
/// (`NAME=value` lines). The Codex path is served too: when the call carries
/// `--output-last-message`, the answer goes to the named file instead of
/// stdout.
fn write_recording_launcher(dir: &Path) -> PathBuf {
    let launcher = dir.join("amplihack");
    let d = dir.display();
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\n\
             set -u\n\
             c=$(cat '{d}/count' 2>/dev/null || echo 0)\n\
             c=$((c+1))\n\
             echo \"$c\" > '{d}/count'\n\
             final=\n\
             : > '{d}/argv-'\"$c\"\n\
             for a in \"$@\"; do printf '%s\\0' \"$a\" >> '{d}/argv-'\"$c\"; done\n\
             while [ $# -gt 0 ]; do\n\
             \x20 if [ \"$1\" = --output-last-message ]; then shift; final=$1; fi\n\
             \x20 shift\n\
             done\n\
             cat >/dev/null\n\
             env > '{d}/env-'\"$c\"\n\
             if [ -n \"$final\" ]; then printf 'ok' > \"$final\"; else printf 'ok'; fi\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    launcher
}

/// Run the built binary on `yaml` with the recording launcher in place.
///
/// `parent` adds variables to the runner's own environment, so that a test can
/// show what the runner passes on and what it replaces.
fn run_recipe(dir: &Path, yaml: &str, extra_args: &[&str], parent: &[(&str, &str)]) -> Output {
    let launcher = write_recording_launcher(dir);
    let recipe = dir.join("recipe.yaml");
    std::fs::write(&recipe, yaml).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"));
    cmd.arg(&recipe)
        .args(["--output-format", "json"])
        .args(extra_args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TMPDIR", std::env::temp_dir())
        .env("HOME", dir)
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
        .env("AMPLIHACK_RATELIMIT_MAX_RETRIES", "0")
        .env("AMPLIHACK_LAUNCHER_BINARY", &launcher);
    for (name, value) in parent {
        cmd.env(name, value);
    }
    cmd.output().unwrap()
}

/// The environment of launcher call `n`, as a map.
fn recorded_env(dir: &Path, call: usize) -> HashMap<String, String> {
    let path = dir.join(format!("env-{call}"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("launcher call {call} recorded no environment: {e}"));
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The argument vector of launcher call `n`.
fn recorded_argv(dir: &Path, call: usize) -> Vec<String> {
    let path = dir.join(format!("argv-{call}"));
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("launcher call {call} recorded no arguments: {e}"))
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

fn assert_ran(out: &Output) {
    assert!(
        out.status.success(),
        "run failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn step_output(out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout[stdout.find('{').expect("JSON result")..]).unwrap();
    json["step_results"][0]["output"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The error recorded against `step_id`, and the runner's own log line for it.
///
/// Asserting on these rather than on the whole run output matters for the
/// tests below: `--output-format json` reports every step's output and the
/// recipe's context, so a value the *recipe* produced appears there whatever
/// the error message says about it.
fn step_error(out: &Output, step_id: &str) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout[stdout.find('{').expect("JSON result")..])
            .unwrap_or_else(|e| panic!("no JSON result: {e}\n{stdout}"));
    let recorded = json["step_results"]
        .as_array()
        .expect("step_results array")
        .iter()
        .find(|r| r["step_id"] == step_id)
        .unwrap_or_else(|| panic!("no record for step {step_id}: {json}"))["error"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    format!("{recorded}\n{}", String::from_utf8_lossy(&out.stderr))
}

/// A single agent step, with whatever extra fields the test needs.
fn one_agent_step(extra: &str) -> String {
    format!("name: contract\nsteps:\n  - id: only\n    type: agent\n    prompt: hello\n{extra}")
}

// ── the context variable ───────────────────────────────────────────────────

#[test]
fn a_minimal_step_announces_itself_on_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_recipe(
        dir.path(),
        &one_agent_step("    context_mode: minimal\n"),
        &[],
        &[],
    );
    assert_ran(&out);
    let env = recorded_env(dir.path(), 1);
    assert_eq!(env.get(CONTEXT).map(String::as_str), Some("minimal"));
    assert_eq!(env.get(STEP_ID).map(String::as_str), Some("only"));
}

#[test]
fn opting_in_is_per_step_within_one_recipe() {
    // The declaration belongs to the step, not the run: a lightweight step and
    // an ordinary one sit side by side in the same recipe.
    let dir = tempfile::tempdir().unwrap();
    let out = run_recipe(
        dir.path(),
        "name: contract\nsteps:\n  - id: light\n    type: agent\n    prompt: hello\n    context_mode: minimal\n    effort: low\n  - id: heavy\n    type: agent\n    prompt: hello\n",
        &[],
        &[],
    );
    assert_ran(&out);

    let light = recorded_env(dir.path(), 1);
    assert_eq!(light.get(CONTEXT).map(String::as_str), Some("minimal"));
    assert_eq!(light.get(EFFORT).map(String::as_str), Some("low"));
    assert_eq!(light.get(STEP_ID).map(String::as_str), Some("light"));

    let heavy = recorded_env(dir.path(), 2);
    assert!(
        !heavy.contains_key(CONTEXT),
        "a step that declares nothing must start exactly as it does today"
    );
    assert!(!heavy.contains_key(EFFORT), "no effort was declared");
    assert_eq!(heavy.get(STEP_ID).map(String::as_str), Some("heavy"));
}

/// An argument vector with absolute paths elided, so that two runs can be
/// compared: the agent system-prompt file lives in a fresh temporary directory
/// on every launch.
fn stable_argv(dir: &Path, call: usize) -> Vec<String> {
    recorded_argv(dir, call)
        .into_iter()
        .map(|arg| {
            if arg.starts_with('/') {
                "<path>".to_string()
            } else {
                arg
            }
        })
        .collect()
}

#[test]
fn the_context_mode_changes_no_argument() {
    // The runner declares the minimal start; amplihack implements it. The two
    // argument vectors must therefore be identical.
    let plain = tempfile::tempdir().unwrap();
    let out = run_recipe(plain.path(), &one_agent_step(""), &[], &[]);
    assert_ran(&out);
    let without = stable_argv(plain.path(), 1);

    let minimal = tempfile::tempdir().unwrap();
    let out = run_recipe(
        minimal.path(),
        &one_agent_step("    context_mode: minimal\n"),
        &[],
        &[],
    );
    assert_ran(&out);
    let with = stable_argv(minimal.path(), 1);

    assert_eq!(
        without, with,
        "context_mode must not reach the command line"
    );
    assert!(
        without.len() > 2,
        "the comparison must be over a real command line: {without:?}"
    );
}

#[test]
fn the_codex_path_carries_the_same_contract() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_recipe(
        dir.path(),
        &one_agent_step("    context_mode: minimal\n    effort: low\n"),
        &["--agent-binary", "codex"],
        &[],
    );
    assert_ran(&out);
    let env = recorded_env(dir.path(), 1);
    assert_eq!(env.get(CONTEXT).map(String::as_str), Some("minimal"));
    assert_eq!(env.get(EFFORT).map(String::as_str), Some("low"));
    assert_eq!(env.get(STEP_ID).map(String::as_str), Some("only"));
}

#[test]
fn the_json_repair_retry_carries_the_same_declaration() {
    // The repair call is the same step's work, so it inherits the step's
    // declaration rather than starting over.
    let dir = tempfile::tempdir().unwrap();
    let launcher = dir.path().join("amplihack");
    let d = dir.path().display();
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\n\
             set -u\n\
             c=$(cat '{d}/count' 2>/dev/null || echo 0)\n\
             c=$((c+1))\n\
             echo \"$c\" > '{d}/count'\n\
             : > '{d}/argv-'\"$c\"\n\
             for a in \"$@\"; do printf '%s\\0' \"$a\" >> '{d}/argv-'\"$c\"; done\n\
             cat >/dev/null\n\
             env > '{d}/env-'\"$c\"\n\
             if [ \"$c\" = 1 ]; then printf 'not json'; else printf '{{\"ok\":true}}'; fi\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let recipe = dir.path().join("recipe.yaml");
    std::fs::write(
        &recipe,
        "name: contract\nsteps:\n  - id: only\n    type: agent\n    prompt: give json\n    parse_json: true\n    context_mode: minimal\n    model: haiku\n    effort: low\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .arg(&recipe)
        .args(["--output-format", "json"])
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TMPDIR", std::env::temp_dir())
        .env("HOME", dir.path())
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
        .env("AMPLIHACK_RATELIMIT_MAX_RETRIES", "0")
        .env("AMPLIHACK_LAUNCHER_BINARY", &launcher)
        .output()
        .unwrap();
    assert_ran(&out);
    for call in [1, 2] {
        let env = recorded_env(dir.path(), call);
        assert_eq!(
            env.get(CONTEXT).map(String::as_str),
            Some("minimal"),
            "call {call} is the same step's work"
        );
        assert_eq!(env.get(EFFORT).map(String::as_str), Some("low"));
        assert_eq!(env.get(STEP_ID).map(String::as_str), Some("only"));
        let argv = recorded_argv(dir.path(), call);
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--model" && w[1] == "haiku"),
            "call {call} keeps the step's model: {argv:?}"
        );
    }
}

#[test]
fn bash_steps_receive_no_contract_variables() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = format!(
        "name: contract\nsteps:\n  - id: shell\n    type: bash\n    command: \"printf '%s|%s|%s' \\\"${{{CONTEXT}-unset}}\\\" \\\"${{{EFFORT}-unset}}\\\" \\\"${{{STEP_ID}-unset}}\\\"\"\n    effort: low\n"
    );
    let out = run_recipe(dir.path(), &yaml, &[], &[]);
    assert_ran(&out);
    assert_eq!(
        step_output(&out),
        "unset|unset|unset",
        "the contract is an agent-launch contract only"
    );
}

// ── inherited values ───────────────────────────────────────────────────────

#[test]
fn stale_contract_variables_do_not_survive_into_an_agent_child() {
    // A runner started from inside an agent step inherits that step's
    // variables. Absence must mean absence, so every launch removes all three
    // before deciding what to set. Without this, one variable set once
    // silently degrades every agent step of every recipe beneath it.
    let dir = tempfile::tempdir().unwrap();
    let out = run_recipe(
        dir.path(),
        &one_agent_step(""),
        &[],
        &[
            (CONTEXT, "minimal"),
            (EFFORT, "high"),
            (STEP_ID, "a-step-of-the-outer-run"),
        ],
    );
    assert_ran(&out);
    let env = recorded_env(dir.path(), 1);
    assert!(
        !env.contains_key(CONTEXT),
        "an inherited {CONTEXT} must not be mistaken for this step's declaration"
    );
    assert!(!env.contains_key(EFFORT), "an inherited effort is stale");
    assert_eq!(
        env.get(STEP_ID).map(String::as_str),
        Some("only"),
        "the step id is this run's, not the outer run's"
    );
}

#[test]
fn stale_contract_variables_do_not_reach_a_bash_step() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = format!(
        "name: contract\nsteps:\n  - id: shell\n    type: bash\n    command: \"printf '%s|%s|%s' \\\"${{{CONTEXT}-unset}}\\\" \\\"${{{EFFORT}-unset}}\\\" \\\"${{{STEP_ID}-unset}}\\\"\"\n"
    );
    let out = run_recipe(
        dir.path(),
        &yaml,
        &[],
        &[
            (CONTEXT, "minimal"),
            (EFFORT, "high"),
            (STEP_ID, "a-step-of-the-outer-run"),
        ],
    );
    assert_ran(&out);
    assert_eq!(step_output(&out), "unset|unset|unset");
}

#[test]
fn naming_a_contract_variable_to_env_allow_does_not_let_it_through() {
    // The removal is unconditional and happens before the allowlist, so the
    // allowlist cannot be used to re-admit an inherited value.
    let dir = tempfile::tempdir().unwrap();
    let out = run_recipe(
        dir.path(),
        &one_agent_step(""),
        &[
            "--env-allow",
            "PATH",
            "--env-allow",
            "HOME",
            "--env-allow",
            CONTEXT,
            "--env-allow",
            EFFORT,
            "--env-allow",
            STEP_ID,
        ],
        &[
            (CONTEXT, "minimal"),
            (EFFORT, "high"),
            (STEP_ID, "a-step-of-the-outer-run"),
        ],
    );
    assert_ran(&out);
    let env = recorded_env(dir.path(), 1);
    assert!(!env.contains_key(CONTEXT), "{env:?}");
    assert!(!env.contains_key(EFFORT), "{env:?}");
    assert_eq!(
        env.get(STEP_ID).map(String::as_str),
        Some("only"),
        "the run's own value is inserted after the filter"
    );
}

#[test]
fn the_contract_survives_env_allow() {
    // The three variables are inserted after the allowlist filter, as the
    // session-tree variables are, so `--env-allow` does not describe an agent
    // child's environment exhaustively.
    let dir = tempfile::tempdir().unwrap();
    let out = run_recipe(
        dir.path(),
        &one_agent_step("    context_mode: minimal\n    effort: low\n"),
        &["--env-allow", "PATH", "--env-allow", "HOME"],
        &[],
    );
    assert_ran(&out);
    let env = recorded_env(dir.path(), 1);
    assert_eq!(env.get(CONTEXT).map(String::as_str), Some("minimal"));
    assert_eq!(env.get(EFFORT).map(String::as_str), Some("low"));
    assert_eq!(env.get(STEP_ID).map(String::as_str), Some("only"));
}

// ── the step id ────────────────────────────────────────────────────────────

#[test]
fn a_long_step_id_reaches_the_launch_whole() {
    // Nothing truncates the id: a recipe may call a step whatever it likes.
    let dir = tempfile::tempdir().unwrap();
    let long = "s".repeat(300);
    let out = run_recipe(
        dir.path(),
        &format!("name: contract\nsteps:\n  - id: {long}\n    type: agent\n    prompt: hello\n"),
        &[],
        &[],
    );
    assert_ran(&out);
    assert_eq!(
        recorded_env(dir.path(), 1).get(STEP_ID).map(String::as_str),
        Some(long.as_str())
    );
}

#[test]
fn an_id_that_cannot_travel_costs_one_variable_not_the_run() {
    // A control character cannot be placed in the environment safely. The
    // variable is omitted and the step still runs, which is why a consumer
    // must treat the id as usually present rather than guaranteed.
    let dir = tempfile::tempdir().unwrap();
    let out = run_recipe(
        dir.path(),
        "name: contract\nsteps:\n  - id: \"step\\u0007one\"\n    type: agent\n    prompt: hello\n    context_mode: minimal\n",
        &[],
        &[],
    );
    assert_ran(&out);
    let env = recorded_env(dir.path(), 1);
    assert!(
        !env.contains_key(STEP_ID),
        "an unusable id is omitted rather than placed in the environment"
    );
    assert_eq!(
        env.get(CONTEXT).map(String::as_str),
        Some("minimal"),
        "the rest of the contract is unaffected"
    );
}

// ── effort ─────────────────────────────────────────────────────────────────

#[test]
fn effort_reaches_the_launch_rendered_not_literal() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "name: contract\ncontext:\n  chosen: low\nsteps:\n  - id: only\n    type: agent\n    prompt: hello\n    effort: \"{{chosen}}\"\n";
    let out = run_recipe(dir.path(), yaml, &[], &[]);
    assert_ran(&out);
    assert_eq!(
        recorded_env(dir.path(), 1).get(EFFORT).map(String::as_str),
        Some("low"),
        "effort is rendered with the step's context"
    );
}

#[test]
fn effort_rendered_from_an_earlier_step_reaches_the_launch() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "name: contract\nsteps:\n  - id: pick\n    type: bash\n    command: \"printf medium\"\n    output: chosen\n  - id: only\n    type: agent\n    prompt: hello\n    effort: \"{{chosen}}\"\n";
    let out = run_recipe(dir.path(), yaml, &[], &[]);
    assert_ran(&out);
    assert_eq!(
        recorded_env(dir.path(), 1).get(EFFORT).map(String::as_str),
        Some("medium")
    );
}

#[test]
fn an_effort_that_renders_empty_sets_no_variable() {
    for (case, effort) in [
        ("absent", "\"{{absent}}\""),
        ("empty", "\"\""),
        ("blank", "\"   \""),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let out = run_recipe(
            dir.path(),
            &one_agent_step(&format!("    effort: {effort}\n")),
            &[],
            &[],
        );
        assert_ran(&out);
        assert!(
            !recorded_env(dir.path(), 1).contains_key(EFFORT),
            "{case}: an empty render leaves the launcher's default in place"
        );
    }
}

#[test]
fn an_oversized_effort_fails_the_step_before_the_spawn() {
    // The value is inserted after the inherited-value length filter, and the
    // `AMPLIHACK_` prefix protects it from budget eviction, so an unbounded
    // value would stop the spawn with an opaque budget error instead.
    let dir = tempfile::tempdir().unwrap();
    let long = "e".repeat(200);
    let out = run_recipe(
        dir.path(),
        &one_agent_step(&format!("    effort: \"{long}\"\n")),
        &[],
        &[],
    );
    assert!(!out.status.success(), "an oversized effort must fail loud");
    let text = step_error(&out, "only");
    assert!(text.contains("only"), "the message names the step: {text}");
    assert!(
        text.contains("effort"),
        "the message names the field: {text}"
    );
    assert!(
        !text.contains(&long),
        "the message must not echo the rejected value: {text}"
    );
    assert!(
        !dir.path().join("argv-1").exists(),
        "the step must fail before the launcher is started"
    );
}

#[test]
fn an_effort_carrying_a_control_character_fails_the_step() {
    for (case, value) in [("escape", "lo\\u001bw"), ("nul", "lo\\u0000w")] {
        let dir = tempfile::tempdir().unwrap();
        let out = run_recipe(
            dir.path(),
            &one_agent_step(&format!("    effort: \"{value}\"\n")),
            &[],
            &[],
        );
        assert!(!out.status.success(), "{case}: {}", combined(&out));
        let text = step_error(&out, "only");
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{0}'),
            "{case}: no raw control character reaches the error: {text:?}"
        );
        assert!(
            text.contains("effort"),
            "{case}: the field is named: {text}"
        );
        assert!(
            !dir.path().join("argv-1").exists(),
            "{case}: the step must fail before the launcher is started"
        );
    }
}

// ── model ──────────────────────────────────────────────────────────────────

#[test]
fn model_reaches_the_launch_rendered_not_literal() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "name: contract\ncontext:\n  tier: haiku\nsteps:\n  - id: only\n    type: agent\n    prompt: hello\n    model: \"{{tier}}\"\n";
    let out = run_recipe(dir.path(), yaml, &[], &[]);
    assert_ran(&out);
    let argv = recorded_argv(dir.path(), 1);
    assert!(
        argv.windows(2)
            .any(|w| w[0] == "--model" && w[1] == "haiku"),
        "the rendered model must reach the command line: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a.contains("{{")),
        "no template may survive to the launch: {argv:?}"
    );
}

#[test]
fn the_model_travels_only_as_an_argument() {
    // One route, as before #219. A second would be a second way to say the
    // same thing — and argv is world-readable where the environment is not.
    let dir = tempfile::tempdir().unwrap();
    let out = run_recipe(dir.path(), &one_agent_step("    model: haiku\n"), &[], &[]);
    assert_ran(&out);
    let env = recorded_env(dir.path(), 1);
    assert!(
        !env.contains_key("AMPLIHACK_STEP_MODEL"),
        "no model variable exists: {env:?}"
    );
}

#[test]
fn tier_words_pass_through_byte_for_byte() {
    // amplihack-rs #2722 carries `fast`, `standard` and `strong` in this same
    // field and resolves them in its launcher. The runner holds no vocabulary.
    for tier in ["fast", "standard", "strong"] {
        let dir = tempfile::tempdir().unwrap();
        let out = run_recipe(
            dir.path(),
            &one_agent_step(&format!("    model: {tier}\n")),
            &[],
            &[],
        );
        assert_ran(&out);
        let argv = recorded_argv(dir.path(), 1);
        assert!(
            argv.windows(2).any(|w| w[0] == "--model" && w[1] == tier),
            "tier word {tier} must pass through unchanged: {argv:?}"
        );
    }
}

#[test]
fn a_model_that_renders_empty_passes_no_model_argument() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_recipe(
        dir.path(),
        &one_agent_step("    model: \"{{absent}}\"\n"),
        &[],
        &[],
    );
    assert_ran(&out);
    let argv = recorded_argv(dir.path(), 1);
    assert!(
        !argv.iter().any(|a| a == "--model"),
        "an empty model leaves the launcher's default in place: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a.is_empty()),
        "no empty argument may be emitted: {argv:?}"
    );
}

#[test]
fn a_model_that_renders_to_a_flag_fails_the_step() {
    // The model is a template sink fed by earlier step output. A value opening
    // with a hyphen would be read as an option by a launcher this runner
    // neither controls nor inspects.
    let dir = tempfile::tempdir().unwrap();
    let yaml = "name: contract\nsteps:\n  - id: pick\n    type: bash\n    command: \"printf -- --dangerously-skip-permissions\"\n    output: tier\n  - id: only\n    type: agent\n    prompt: hello\n    model: \"{{tier}}\"\n";
    let out = run_recipe(dir.path(), yaml, &[], &[]);
    assert!(
        !out.status.success(),
        "a model shaped like a flag must fail the step, not reach the command line"
    );
    let text = step_error(&out, "only");
    assert!(text.contains("only"), "the message names the step: {text}");
    assert!(
        text.contains("model"),
        "the message names the field: {text}"
    );
    assert!(
        !text.contains("dangerously-skip-permissions"),
        "the message must not echo the rejected value, which can carry a \
         secret or an escape sequence into stderr, the audit log and a CI \
         log: {text}"
    );
    assert!(
        !dir.path().join("argv-1").exists(),
        "the step must fail before the launcher is started"
    );
}

#[test]
fn a_model_carrying_a_shell_metacharacter_fails_the_step() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "name: contract\ncontext:\n  tier: \"haiku; rm -rf /\"\nsteps:\n  - id: only\n    type: agent\n    prompt: hello\n    model: \"{{tier}}\"\n";
    let out = run_recipe(dir.path(), yaml, &[], &[]);
    assert!(!out.status.success(), "{}", combined(&out));
    assert!(!dir.path().join("argv-1").exists());
}

#[test]
fn an_oversized_model_fails_the_step() {
    let dir = tempfile::tempdir().unwrap();
    let long = "m".repeat(200);
    let out = run_recipe(
        dir.path(),
        &one_agent_step(&format!("    model: {long}\n")),
        &[],
        &[],
    );
    assert!(!out.status.success(), "{}", combined(&out));
    assert!(!dir.path().join("argv-1").exists());
}

#[test]
fn a_failing_model_does_not_fall_back_to_the_default() {
    // A template defect is loud rather than silent: the step fails rather than
    // launching with whatever the launcher would have chosen.
    let dir = tempfile::tempdir().unwrap();
    let yaml = "name: contract\ncontext:\n  tier: \"--add-dir\"\nsteps:\n  - id: only\n    type: agent\n    prompt: hello\n    model: \"{{tier}}\"\n  - id: after\n    type: bash\n    command: \"touch ran\"\n";
    let out = run_recipe(dir.path(), yaml, &[], &[]);
    assert!(!out.status.success(), "{}", combined(&out));
    assert!(
        !dir.path().join("ran").exists(),
        "the run stops at the failing step"
    );
}

// ── unchanged behaviour ────────────────────────────────────────────────────

#[test]
fn a_step_declaring_none_of_the_three_launches_as_it_always_did() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_recipe(dir.path(), &one_agent_step("    model: sonnet\n"), &[], &[]);
    assert_ran(&out);
    let argv = recorded_argv(dir.path(), 1);
    assert!(
        argv.windows(2)
            .any(|w| w[0] == "--model" && w[1] == "sonnet"),
        "{argv:?}"
    );
    let env = recorded_env(dir.path(), 1);
    assert!(!env.contains_key(CONTEXT));
    assert!(!env.contains_key(EFFORT));
    assert_eq!(env.get(STEP_ID).map(String::as_str), Some("only"));
}

// ── the adapter contract ───────────────────────────────────────────────────

mod adapter {
    use recipe_runner_rs::adapters::{
        AgentLaunch, CONTRACT_ENV_NAMES, MINIMAL_CONTEXT, STEP_CONTEXT_ENV, STEP_EFFORT_ENV,
        STEP_ID_ENV,
    };
    use recipe_runner_rs::models::StepContextMode;
    use recipe_runner_rs::parser::RecipeParser;
    use recipe_runner_rs::runner::RecipeRunner;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[test]
    fn the_contract_owns_exactly_three_names() {
        // The removal loop reads this slice. A fourth variable added without
        // being listed here would reopen the inherited-value hole.
        let names: &[&str] = CONTRACT_ENV_NAMES;
        assert_eq!(names.len(), 3);
        for name in [STEP_ID_ENV, STEP_CONTEXT_ENV, STEP_EFFORT_ENV] {
            assert!(names.contains(&name), "{name} is not listed: {names:?}");
        }
        assert_eq!(STEP_ID_ENV, "AMPLIHACK_STEP_ID");
        assert_eq!(STEP_CONTEXT_ENV, "AMPLIHACK_STEP_CONTEXT");
        assert_eq!(STEP_EFFORT_ENV, "AMPLIHACK_STEP_EFFORT");
        assert_eq!(MINIMAL_CONTEXT, "minimal");
    }

    /// What a stand-in adapter saw of one launch.
    #[derive(Clone, Debug, Default)]
    struct Seen {
        step_id: String,
        model: Option<String>,
        effort: Option<String>,
        minimal: bool,
        contract_env: Vec<(String, String)>,
        debug: String,
    }

    /// An adapter written after #219: it overrides the launch method and
    /// records what it was handed.
    struct RecordingAdapter {
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl recipe_runner_rs::adapters::Adapter for RecordingAdapter {
        fn execute_agent_step(
            &self,
            _prompt: &str,
            _agent_name: Option<&str>,
            _system_prompt: Option<&str>,
            _mode: Option<&str>,
            _working_dir: &str,
            _model: Option<&str>,
            _timeout: Option<u64>,
        ) -> Result<String, anyhow::Error> {
            panic!("the runner must call the launch method, not the legacy one");
        }

        fn execute_agent_launch(&self, launch: &AgentLaunch) -> Result<String, anyhow::Error> {
            self.seen.lock().unwrap().push(Seen {
                step_id: launch.step_id.clone(),
                model: launch.model.clone(),
                effort: launch.effort.clone(),
                minimal: launch.context_mode == Some(StepContextMode::Minimal),
                contract_env: launch
                    .contract_env()
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                debug: format!("{launch:?}"),
            });
            Ok("ok".to_string())
        }

        fn execute_bash_step(
            &self,
            _command: &str,
            _working_dir: &str,
            _timeout: Option<u64>,
            _extra_env: &HashMap<String, String>,
        ) -> Result<String, anyhow::Error> {
            Ok(String::new())
        }

        fn is_available(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "recording"
        }
    }

    /// An adapter written before #219: it implements only the original method
    /// and has never heard of the contract. It must keep working untouched.
    /// Each legacy call, as the prompt it was given and the model it carried.
    type LegacyCalls = Arc<Mutex<Vec<(String, Option<String>)>>>;

    struct LegacyAdapter {
        calls: LegacyCalls,
    }

    impl recipe_runner_rs::adapters::Adapter for LegacyAdapter {
        fn execute_agent_step(
            &self,
            prompt: &str,
            _agent_name: Option<&str>,
            _system_prompt: Option<&str>,
            _mode: Option<&str>,
            _working_dir: &str,
            model: Option<&str>,
            _timeout: Option<u64>,
        ) -> Result<String, anyhow::Error> {
            self.calls
                .lock()
                .unwrap()
                .push((prompt.to_string(), model.map(str::to_string)));
            Ok("ok".to_string())
        }

        fn execute_bash_step(
            &self,
            _command: &str,
            _working_dir: &str,
            _timeout: Option<u64>,
            _extra_env: &HashMap<String, String>,
        ) -> Result<String, anyhow::Error> {
            Ok(String::new())
        }

        fn is_available(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "legacy"
        }
    }

    const RECIPE: &str = "name: contract\ncontext:\n  tier: haiku\nsteps:\n  - id: only\n    type: agent\n    prompt: a client's private instructions\n    context_mode: minimal\n    model: \"{{tier}}\"\n    effort: low\n";

    fn run_with<A: recipe_runner_rs::adapters::Adapter>(adapter: A) {
        let dir = tempfile::tempdir().unwrap();
        let recipe = RecipeParser::new().parse(RECIPE).unwrap();
        let result = RecipeRunner::new(adapter)
            .with_working_dir(dir.path().to_str().unwrap())
            .with_auto_stage(false)
            .execute(&recipe, None);
        assert!(result.success, "{result:?}");
    }

    #[test]
    fn the_runner_hands_the_adapter_a_resolved_launch() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        run_with(RecordingAdapter {
            seen: Arc::clone(&seen),
        });
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let launch = &seen[0];
        assert_eq!(launch.step_id, "only");
        assert_eq!(
            launch.model.as_deref(),
            Some("haiku"),
            "the adapter receives the rendered model, not the template"
        );
        assert_eq!(launch.effort.as_deref(), Some("low"));
        assert!(launch.minimal);
    }

    #[test]
    fn contract_env_names_only_what_the_step_declared() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        run_with(RecordingAdapter {
            seen: Arc::clone(&seen),
        });
        let env: HashMap<String, String> = seen.lock().unwrap()[0]
            .contract_env
            .iter()
            .cloned()
            .collect();
        assert_eq!(env.get(STEP_ID_ENV).map(String::as_str), Some("only"));
        assert_eq!(
            env.get(STEP_CONTEXT_ENV).map(String::as_str),
            Some(MINIMAL_CONTEXT),
            "the only value this variable ever takes"
        );
        assert_eq!(env.get(STEP_EFFORT_ENV).map(String::as_str), Some("low"));
        assert!(
            !env.contains_key("AMPLIHACK_STEP_MODEL"),
            "the model travels as an argument: {env:?}"
        );
        assert_eq!(env.len(), 3, "and nothing else: {env:?}");
    }

    #[test]
    fn debug_reports_the_prompt_by_length_and_never_by_content() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        run_with(RecordingAdapter {
            seen: Arc::clone(&seen),
        });
        let shown = seen.lock().unwrap()[0].debug.clone();
        assert!(
            !shown.contains("private instructions"),
            "the prompt must never be formatted: {shown}"
        );
        assert!(
            shown.contains("prompt_len"),
            "its size is reported instead: {shown}"
        );
        assert!(
            shown.contains("only"),
            "the step id is small and is shown: {shown}"
        );
    }

    #[test]
    fn an_adapter_that_never_heard_of_the_contract_still_runs_the_step() {
        // The launch method carries a default body that forwards to the
        // original one, so every adapter written before #219 compiles and
        // behaves as it did.
        let calls = Arc::new(Mutex::new(Vec::new()));
        run_with(LegacyAdapter {
            calls: Arc::clone(&calls),
        });
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].0.contains("private instructions"));
        assert_eq!(
            calls[0].1.as_deref(),
            Some("haiku"),
            "the rendered model still reaches the legacy method"
        );
    }
}
