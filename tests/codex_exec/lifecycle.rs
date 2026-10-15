use super::fixtures::run;
use serde_json::{Value, json};
use std::fs;

#[test]
fn concurrent_attempts_have_distinct_private_paths() {
    let handles: Vec<_> = (0..4)
        .map(|_| std::thread::spawn(|| run("success", "task", None, "codex")))
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let mut paths = std::collections::HashSet::new();
    for (_, result, record) in &results {
        assert!(result.get("ok").is_some(), "{result}");
        assert!(paths.insert(record["final"].as_str().expect("final path")));
        assert_eq!(record["directory_mode"], 0o700);
    }
}

#[test]
fn rate_limit_retry_has_fresh_output_and_keeps_explicit_model() {
    let (root, result, _) = run("retry", "TASK_SENTINEL", Some("caller-model"), "codex");
    assert!(result.get("ok").is_some(), "{result}");
    let attempts: Vec<Value> = fs::read_to_string(root.path().join("attempts.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(attempts.len(), 2);
    assert_ne!(attempts[0]["final"], attempts[1]["final"]);
    for attempt in attempts {
        let args = attempt["args"].as_array().unwrap();
        assert!(!args.contains(&json!("auto")));
        assert!(args.contains(&json!("caller-model")));
    }
    let input = fs::read_to_string(root.path().join("stdin")).unwrap();
    assert!(
        input.contains("TASK_SENTINEL")
            && input.contains("PERSONA_SENTINEL")
            && input.contains("Do not invoke")
    );
}

fn assert_tree_cleanup(scenario: &str) {
    let (root, result, _) = run(scenario, "task", None, "codex");
    let pid: i32 = fs::read_to_string(root.path().join("descendant.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let state = fs::read_to_string(root.path().join("return-state")).unwrap();
    let leaked = !matches!(state.as_str(), "Z" | "X" | "absent");
    if leaked {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    assert!(
        !leaked,
        "{scenario}: descendant alive at adapter return: {state}"
    );
    assert_eq!(result.get("error").is_some(), scenario == "tree_timeout");
}
#[test]
fn timeout_terminates_owned_descendants() {
    assert_tree_cleanup("tree_timeout");
}
#[test]
fn nominal_exit_terminates_lingering_descendants() {
    assert_tree_cleanup("tree_success");
}

#[test]
fn zero_timeout_fails_even_before_launcher_executes() {
    let (_, result, _) = run("zero_timeout", "task", None, "codex");
    assert!(result["error"].as_str().unwrap().contains("timed out"));
}
#[test]
fn both_diagnostic_streams_are_drained_without_timeout() {
    let (_, result, _) = run("flood", "task", None, "codex");
    assert_eq!(result["ok"], "  FINAL\n\0Unicode: λ\n\n");
}

#[test]
fn writing_descendant_stops_before_final_extraction() {
    let (root, result, _) = run("writer", "task", None, "codex");
    let state = fs::read_to_string(root.path().join("return-state")).unwrap();
    assert!(matches!(state.as_str(), "Z" | "X" | "absent"), "{state}");
    let output = result["ok"].as_str().unwrap();
    assert!(!output.is_empty());
    assert!(output.bytes().all(|byte| byte == b'x'));
}

#[test]
fn resource_deletion_failure_prevents_rate_limit_retry() {
    let (root, result, record) = run("cleanup_failure", "task", None, "codex");
    let obstruction = std::path::Path::new(record["final"].as_str().unwrap())
        .parent()
        .unwrap();
    fs::remove_file(obstruction).unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("attempts.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(result.get("ok").is_none());
    let error = result["error"].as_str().unwrap();
    for detail in [
        "exit status: 7",
        "rate limit",
        "Failed to clean Codex resources",
    ] {
        assert!(error.contains(detail), "missing {detail}: {error}");
    }
    assert!(!error.contains("SECRET"));
}
