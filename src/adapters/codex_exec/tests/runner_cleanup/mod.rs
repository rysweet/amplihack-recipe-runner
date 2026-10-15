//! Cleanup-only contracts: production error composition, real runner dispatch.
mod fixtures;
mod json;
mod nested_recovery;
mod parallel;
mod sequencing_hooks;

#[test]
fn cleanup_fixture_has_typed_marker_without_interruption() {
    let error = fixtures::cleanup_error();
    assert!(
        error
            .downcast_ref::<super::super::CleanupFailure>()
            .is_some()
    );
    assert!(error.downcast_ref::<super::super::Interruption>().is_none());
    assert!(format!("{error:#}").contains("owned resource teardown failed"));
}

#[test]
fn codex_integration_modules_stay_within_300_lines() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut files = vec![root.join("codex_exec_tests.rs")];
    let modules = root.join("codex_exec");
    let mut dirs = vec![modules];
    while let Some(dir) = dirs.pop() {
        if !dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }
    for file in files
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
    {
        let lines = std::fs::read_to_string(&file).unwrap().lines().count();
        assert!(
            lines <= 300,
            "{} has {lines} lines; limit is 300",
            file.display()
        );
    }
}

#[test]
fn combined_execution_and_cleanup_retains_typed_marker_and_both_causes() {
    use super::super::{CleanupFailure, Interruption, finish_resources};
    let error = finish_resources(
        Err(anyhow::anyhow!("primary failure")),
        Err(anyhow::anyhow!("teardown failure")),
    )
    .unwrap_err()
    .context("safe outer context");
    assert!(error.downcast_ref::<CleanupFailure>().is_some());
    assert!(error.downcast_ref::<Interruption>().is_none());
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.contains("primary failure"));
    assert!(diagnostic.contains("teardown failure"));
}
