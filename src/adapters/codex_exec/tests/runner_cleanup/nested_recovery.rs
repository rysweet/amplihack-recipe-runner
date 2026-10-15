use super::fixtures::*;

#[test]
fn nested_cleanup_cannot_be_recovered_as_success() {
    let root = tempfile::tempdir().unwrap();
    write_child(
        root.path(),
        "child",
        &format!("  - id: cleanup\n    prompt: cleanup\n    fatal: false\n{AFTER}"),
    );
    let adapter = Scripted::new([Reply::Cleanup]);
    let state = adapter.state.clone();
    let result = runner(adapter, root.path()).execute(&recipe(&format!(
        "  - id: child\n    type: recipe\n    recipe: child\n    recovery_on_failure: true\n    continue_on_error: true\n{AFTER}"), ""), None);
    assert_terminal(&result, &state, 1);
}

#[test]
fn multilevel_cleanup_blocks_each_recovery_and_later_step() {
    let root = tempfile::tempdir().unwrap();
    write_child(
        root.path(),
        "leaf",
        &format!("  - id: cleanup\n    prompt: cleanup\n{AFTER}"),
    );
    write_child(
        root.path(),
        "middle",
        &format!(
            "  - id: leaf\n    type: recipe\n    recipe: leaf\n    recovery_on_failure: true\n    fatal: false\n{AFTER}"
        ),
    );
    let adapter = Scripted::new([Reply::Cleanup]);
    let state = adapter.state.clone();
    let result = runner(adapter, root.path()).execute(&recipe(&format!(
        "  - id: middle\n    type: recipe\n    recipe: middle\n    recovery_on_failure: true\n    fatal: false\n{AFTER}"), ""), None);
    assert_terminal(&result, &state, 1);
}

#[test]
fn cleanup_during_recovery_blocks_enclosing_recovery() {
    let root = tempfile::tempdir().unwrap();
    write_child(
        root.path(),
        "leaf",
        "  - id: ordinary\n    prompt: ordinary\n",
    );
    write_child(
        root.path(),
        "middle",
        &format!(
            "  - id: leaf\n    type: recipe\n    recipe: leaf\n    recovery_on_failure: true\n    fatal: false\n{AFTER}"
        ),
    );
    let adapter = Scripted::new([Reply::Ordinary, Reply::Cleanup]);
    let state = adapter.state.clone();
    let result = runner(adapter, root.path()).execute(&recipe(&format!(
        "  - id: middle\n    type: recipe\n    recipe: middle\n    recovery_on_failure: true\n    fatal: false\n{AFTER}"), ""), None);
    assert_terminal(&result, &state, 2);
    assert!(format!("{result:?}").contains("ordinary cleanup/cancelled failure"));
}

#[test]
fn ordinary_nested_failure_still_recovers() {
    let root = tempfile::tempdir().unwrap();
    write_child(
        root.path(),
        "child",
        "  - id: ordinary\n    prompt: ordinary\n",
    );
    let adapter = Scripted::new([Reply::Ordinary, Reply::Success]);
    let state = adapter.state.clone();
    let result = runner(adapter, root.path()).execute(&recipe(&format!(
        "  - id: child\n    type: recipe\n    recipe: child\n    recovery_on_failure: true\n{AFTER}"), ""), None);
    assert!(result.success, "ordinary recovery changed: {result:?}");
    assert_eq!(state.agents(), 3);
    assert!(state.events().contains(&"bash:forbidden-bash".into()));
}
