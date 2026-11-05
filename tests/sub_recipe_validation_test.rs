//! Real CLI regressions for issue #194. Marker files prove execution timing.

use serde_json::json;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    root: tempfile::TempDir,
    parent: PathBuf,
    search: PathBuf,
    child: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let search = root.path().join("search");
        std::fs::create_dir(&search).unwrap();
        let child = search.join("child.yaml");
        std::fs::write(
            &child,
            "name: child\nsteps:\n  - id: child-command\n    command: 'printf CHILD_EXECUTED; printf child > child-marker'\n",
        )
        .unwrap();
        let parent = root.path().join("parent.yaml");
        Self {
            root,
            parent,
            search,
            child,
        }
    }

    fn write_parent(&self, reference: &str) {
        let yaml = serde_yaml::to_string(&json!({
            "name": "parent",
            "steps": [
                {"id": "earlier", "command": "printf FIRST_STEP_EXECUTED; printf earlier > earlier-marker"},
                {"id": "dispatch", "recipe": reference}
            ]
        })).unwrap();
        std::fs::write(&self.parent, yaml).unwrap();
    }

    fn cli(&self, validate_only: bool) -> Output {
        self.cli_with_args(validate_only, &[])
    }

    fn cli_with_args(&self, validate_only: bool, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"));
        command
            .current_dir(self.root.path())
            .arg(&self.parent)
            .arg("--working-dir")
            .arg(self.root.path())
            .arg("--recipe-dir")
            .arg(&self.search)
            .args([
                "--no-default-search",
                "--no-update-check",
                "--no-auto-stage",
            ])
            .env_remove("RECIPE_RUNNER_LOG_DIR")
            .env_remove("RECIPE_RUNNER_REQUIRE_AUDIT")
            .env_remove("RUST_LOG");
        if validate_only {
            command.arg("--validate-only");
        }
        command.args(args);
        command
            .output()
            .expect("run the current-source Cargo executable")
    }

    fn assert_no_effects(&self) {
        assert!(
            !self.root.path().join("earlier-marker").exists(),
            "earlier command ran before literal rejection"
        );
        assert!(
            !self.root.path().join("child-marker").exists(),
            "child command ran"
        );
    }
}

fn transcript(output: &Output) -> String {
    format!(
        "exit: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_reference_failure(output: &Output, resolved: bool) {
    let text = transcript(output);
    assert!(
        !output.status.success(),
        "invalid reference accepted:\n{text}"
    );
    assert!(text.contains("dispatch"), "missing offending step:\n{text}");
    assert!(
        text.contains("[A-Za-z0-9_-]+"),
        "missing canonical-name rule:\n{text}"
    );
    assert!(
        text.contains("--recipe-dir"),
        "missing search-directory advice:\n{text}"
    );
    if resolved {
        assert!(
            text.to_lowercase().contains("resolved"),
            "missing runtime stage:\n{text}"
        );
    }
}

#[test]
fn cli_validation_rejects_absolute_sub_recipe_file_with_actionable_step_diagnostic() {
    let fixture = Fixture::new();
    fixture.write_parent(fixture.child.to_str().unwrap());
    let output = fixture.cli(true);
    assert_reference_failure(&output, false);
    fixture.assert_no_effects();
}

#[test]
fn cli_execution_rejects_literal_before_earlier_command_or_child_effects() {
    // Include an existing absolute file: failure must be the name policy, not missing files.
    let fixture = Fixture::new();
    let absolute = fixture.child.to_str().unwrap().to_owned();
    for reference in [
        absolute.as_str(),
        "child/name",
        "child\\name",
        "../child",
        "",
        "child.yaml",
        "child name",
    ] {
        fixture.write_parent(reference);
        let output = fixture.cli(false);
        fixture.assert_no_effects();
        assert_reference_failure(&output, false);
        assert!(
            !transcript(&output).contains("FIRST_STEP_EXECUTED"),
            "{}",
            transcript(&output)
        );
        assert!(
            !transcript(&output).contains("CHILD_EXECUTED"),
            "{}",
            transcript(&output)
        );
    }
}

#[test]
fn cli_canonical_name_with_explicit_search_directory_validates_and_executes() {
    let fixture = Fixture::new();
    fixture.write_parent("child");
    let validation = fixture.cli(true);
    assert!(validation.status.success(), "{}", transcript(&validation));
    fixture.assert_no_effects();
    let execution = fixture.cli(false);
    assert!(execution.status.success(), "{}", transcript(&execution));
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("earlier-marker")).unwrap(),
        "earlier"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("child-marker")).unwrap(),
        "child"
    );
}

#[test]
fn cli_dynamic_path_passes_static_validation_but_fails_after_rendering() {
    let fixture = Fixture::new();
    fixture.write_parent("/{{selected}}");
    let validation = fixture.cli(true);
    assert!(validation.status.success(), "{}", transcript(&validation));
    fixture.assert_no_effects();
    // The missing variable renders as empty using the existing rendering contract.
    let execution = fixture.cli(false);
    assert_reference_failure(&execution, true);
    assert!(fixture.root.path().join("earlier-marker").exists());
    assert!(!fixture.root.path().join("child-marker").exists());
}

#[test]
fn cli_nested_failure_escapes_identifiers_in_text_stderr_and_progress() {
    let fixture = Fixture::new();
    let recipe_name = "parent\n\r\u{1b}[31m\"\\name";
    let parent_id = "dispatch\n\r\u{1b}[31m\"\\step";
    let completed_id = "completed\n\r\u{1b}[31m\"\\step";
    let failed_id = "failed\n\r\u{1b}[31m\"\\step";
    let reference = "bad\n\r\u{1b}[31m\"\\name";
    std::fs::write(
        &fixture.parent,
        serde_yaml::to_string(&json!({
            "name": recipe_name,
            "steps": [{"id": parent_id, "recipe": "child"}]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        &fixture.child,
        serde_yaml::to_string(&json!({
            "name": "child",
            "context": {"selected": reference},
            "steps": [
                {"id": completed_id, "command": "true"},
                {"id": failed_id, "recipe": "{{selected}}"},
                {"id": "never", "command": "printf child > child-marker"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    for args in [vec![], vec!["--progress"]] {
        let output = fixture.cli_with_args(false, &args);
        assert!(!output.status.success(), "{}", transcript(&output));
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        for text in [&stdout, &stderr] {
            for id in [parent_id, completed_id, failed_id] {
                let escaped: String = id.escape_debug().collect();
                assert!(text.contains(&escaped), "missing {escaped}: {text:?}");
                assert!(!text.contains(id), "raw identifier: {text:?}");
            }
            assert!(!text.contains('\r'), "raw carriage return: {text:?}");
            assert!(!text.contains('\u{1b}'), "raw ESC: {text:?}");
            assert!(
                !text.contains("Searched the following directories"),
                "{text}"
            );
        }
        let escaped_name: String = recipe_name.escape_debug().collect();
        assert!(stdout.contains(&format!("Recipe '{escaped_name}': FAILED")));
        let escaped_completed: String = completed_id.escape_debug().collect();
        assert!(stdout.contains(&format!("Completed steps: {escaped_completed}")));
        assert!(
            !stdout.contains("completed\\\\n"),
            "error was escaped twice: {stdout}"
        );
        if !args.is_empty() {
            let escaped_parent: String = parent_id.escape_debug().collect();
            assert!(stderr.contains(&format!("▶ {escaped_parent} [sub-recipe]")));
            assert!(stderr.contains(&format!("✗ {escaped_parent}")));
            let path = stderr
                .lines()
                .find_map(|line| line.strip_prefix("[amplihack] recipe log: "))
                .unwrap();
            let events: Vec<serde_json::Value> = std::fs::read_to_string(path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(events[0]["recipe"], recipe_name);
            let transitions: Vec<_> = events[1..]
                .iter()
                .map(|event| {
                    (
                        event["step"].as_str().unwrap(),
                        event["status"].as_str().unwrap(),
                    )
                })
                .collect();
            assert_eq!(
                transitions,
                vec![
                    (parent_id, "start"),
                    (completed_id, "start"),
                    (completed_id, "done"),
                    (failed_id, "start"),
                    (failed_id, "fail"),
                    (parent_id, "fail"),
                ]
            );
            std::fs::remove_file(path).unwrap();
        }
        assert!(!fixture.root.path().join("child-marker").exists());
    }

    let output = fixture.cli_with_args(false, &["--output-format", "json"]);
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["recipe_name"], recipe_name);
    assert_eq!(result["step_results"][0]["step_id"], parent_id);
    let error = result["step_results"][0]["error"].as_str().unwrap();
    assert!(error.contains(&failed_id.escape_debug().to_string()));
    assert!(error.contains(&completed_id.escape_debug().to_string()));
    assert!(!error.contains(failed_id));
    assert!(!error.contains(completed_id));
}

#[test]
fn cli_progress_escapes_completed_step_identifiers() {
    let fixture = Fixture::new();
    let id = "completed\n\r\u{1b}[31m";
    std::fs::write(
        &fixture.parent,
        serde_yaml::to_string(&json!({
            "name": "progress", "steps": [{"id": id, "command": "true"}]
        }))
        .unwrap(),
    )
    .unwrap();
    let output = fixture.cli_with_args(false, &["--progress"]);
    assert!(output.status.success(), "{}", transcript(&output));
    let stderr = String::from_utf8(output.stderr).unwrap();
    let escaped: String = id.escape_debug().collect();
    assert!(stderr.contains(&format!("▶ {escaped}")), "{stderr:?}");
    assert!(stderr.contains(&format!("✓ {escaped}")), "{stderr:?}");
    assert!(!stderr.contains(id), "{stderr:?}");
    let path = stderr
        .lines()
        .find_map(|line| line.strip_prefix("[amplihack] recipe log: "))
        .unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cli_dry_run_json_preserves_identifiers_and_inspection_text_escapes_them() {
    let fixture = Fixture::new();
    let id = "identifier\n\r\u{1b}[31m\"\\tail";
    std::fs::write(
        &fixture.parent,
        serde_yaml::to_string(&json!({
            "name": id,
            "steps": [{"id": id, "command": "printf child > child-marker", "parse_json": true}]
        }))
        .unwrap(),
    )
    .unwrap();
    let output = fixture.cli_with_args(false, &["--dry-run", "--output-format", "json"]);
    assert!(output.status.success(), "{}", transcript(&output));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["recipe_name"], id);
    assert_eq!(result["step_results"][0]["step_id"], id);
    let step_output: serde_json::Value =
        serde_json::from_str(result["step_results"][0]["output"].as_str().unwrap()).unwrap();
    assert_eq!(step_output, json!({"dry_run": true, "step": id}));
    for args in [vec!["--validate-only"], vec!["--explain"]] {
        let output = fixture.cli_with_args(false, &args);
        assert!(output.status.success(), "{}", transcript(&output));
        let text = transcript(&output);
        assert!(text.contains(&id.escape_debug().to_string()), "{text:?}");
        assert!(!text.contains(id), "{text:?}");
    }
    fixture.assert_no_effects();
}
