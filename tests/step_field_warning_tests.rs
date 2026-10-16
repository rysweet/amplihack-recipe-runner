//! #130 PART 2 (outside-in): unknown and retired step fields are reported on
//! stderr when a recipe is parsed for a run, even without `RUST_LOG`, and
//! `--validate-only` reports them once on stdout instead of twice.

use std::path::{Path, PathBuf};
use std::process::Command;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_recipe-runner-rs"))
}

fn run(recipe: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(binary_path())
        .arg(recipe)
        .args(args)
        .env_remove("RUST_LOG")
        .output()
        .expect("failed to run recipe-runner-rs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write(dir: &Path, yaml: &str) -> PathBuf {
    let p = dir.join("recipe.yaml");
    std::fs::write(&p, yaml).unwrap();
    p
}

const RETIRED_RECIPE: &str = "name: retired\nsteps:\n  - id: checkpoint\n    type: bash\n    command: echo hi\n    max_env_value_bytes: 65536\n";

#[test]
fn run_warns_on_stderr_about_retired_max_env_value_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let recipe = write(dir.path(), RETIRED_RECIPE);
    let (code, _stdout, stderr) = run(&recipe, &["--dry-run"]);
    assert_eq!(code, 0, "an unknown field must not fail the run: {stderr}");
    let hits: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("max_env_value_bytes"))
        .collect();
    assert_eq!(hits.len(), 1, "exactly one warning expected: {stderr}");
    assert!(hits[0].contains("step 'checkpoint'"), "{}", hits[0]);
    assert!(hits[0].contains("retired"), "{}", hits[0]);
}

#[test]
fn run_warns_on_stderr_about_generic_unknown_fields() {
    let dir = tempfile::tempdir().unwrap();
    let recipe = write(
        dir.path(),
        "name: typo\nsteps:\n  - id: s1\n    type: bash\n    command: echo hi\n    foo: 1\n",
    );
    let (code, _stdout, stderr) = run(&recipe, &["--dry-run"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("recipe 'typo': step 's1' has unknown field 'foo' (ignored)"),
        "{stderr}"
    );
}

#[test]
fn validate_only_reports_the_retired_field_once_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let recipe = write(dir.path(), RETIRED_RECIPE);
    let (_code, stdout, stderr) = run(&recipe, &["--validate-only"]);
    assert_eq!(
        stdout.matches("max_env_value_bytes").count(),
        1,
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("step 'checkpoint': field 'max_env_value_bytes' is retired"),
        "stdout: {stdout}"
    );
    assert!(
        !stderr.contains("max_env_value_bytes"),
        "--validate-only must not also warn at parse time: {stderr}"
    );
}

#[test]
fn known_fields_including_fatal_and_nonfatal_are_silent() {
    let dir = tempfile::tempdir().unwrap();
    let recipe = write(
        dir.path(),
        "name: ok\nsteps:\n  - id: a\n    type: bash\n    command: echo a\n    fatal: false\n  - id: b\n    type: bash\n    command: echo b\n    nonfatal: true\n",
    );
    let (code, _stdout, stderr) = run(&recipe, &["--dry-run"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(!stderr.contains("unknown field"), "{stderr}");
    assert!(!stderr.contains("retired"), "{stderr}");
}

#[test]
fn a_sub_recipe_run_twice_warns_once() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("child.yaml"),
        "name: child\nsteps:\n  - id: c1\n    type: bash\n    command: echo child\n    foo: 1\n",
    )
    .unwrap();
    let parent = dir.path().join("parent.yaml");
    std::fs::write(
        &parent,
        "name: parent\nsteps:\n  - id: first\n    type: recipe\n    recipe: child\n  - id: second\n    type: recipe\n    recipe: child\n",
    )
    .unwrap();
    let dir_arg = dir.path().to_str().unwrap();
    let (code, _stdout, stderr) = run(&parent, &["-R", dir_arg]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stderr
            .matches("recipe 'child': step 'c1' has unknown field 'foo'")
            .count(),
        1,
        "{stderr}"
    );
}
