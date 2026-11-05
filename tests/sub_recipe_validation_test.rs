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
