use super::fixtures::run;
use serde_json::json;
use std::fs;

fn assert_envelope(task: String) {
    let (root, result, record) = run("success", &task, None, "codex");
    assert_eq!(result["ok"], "  FINAL\n\0Unicode: λ\n\n");
    let args = record["args"].as_array().unwrap();
    assert_eq!(&args[..3], &[json!("codex"), json!("--"), json!("exec")]);
    assert_eq!(args.last().unwrap(), "-");
    for forbidden in ["-p", "--model", "--add-dir", "--system-prompt", "--json"] {
        assert!(!args.contains(&json!(forbidden)));
    }
    let input = fs::read_to_string(root.path().join("stdin")).unwrap();
    for text in [
        &task[..],
        "PERSONA_SENTINEL",
        "Do not invoke",
        "Proceed autonomously",
    ] {
        assert!(input.contains(text), "missing {text:?}");
    }
    assert_eq!(input.matches("Proceed autonomously").count(), 1);
    assert_eq!(record["absent"], true);
    assert_eq!(record["directory_mode"], 0o700);
    assert_eq!(record["file_mode"], 0o600);
    assert!(!std::path::Path::new(record["final"].as_str().unwrap()).exists());
}
#[test]
fn small_stdin_envelope() {
    assert_envelope("small task".into());
}
#[test]
fn leading_dash_unicode_multiline_nul_stdin_envelope() {
    assert_envelope("-leading\nλ\0END".into());
}
#[test]
fn large_stdin_envelope() {
    assert_envelope(format!("{}TAIL_SENTINEL", "λ".repeat(100_000)));
}

#[test]
fn existing_claude_and_copilot_stdout_and_prompt_contracts_survive() {
    for provider in ["claude", "copilot"] {
        let (_, result, record) = run("success", "TASK", None, provider);
        assert_eq!(result["ok"], "PROGRESS_ONLY");
        let args = record["args"].as_array().unwrap();
        assert!(args.contains(&json!("-p")));
        assert!(!args.contains(&json!("exec")));
        assert!(args.contains(&json!("--add-dir")));
    }
}

#[test]
fn explicit_model_is_forwarded_without_an_implicit_default() {
    let (_, result, record) = run("success", "task", Some("caller-model"), "codex");
    assert!(result.get("ok").is_some());
    let args = record["args"].as_array().unwrap();
    let index = args
        .iter()
        .position(|arg| arg == "--model")
        .expect("explicit model flag");
    assert_eq!(args[index + 1], "caller-model");
}
