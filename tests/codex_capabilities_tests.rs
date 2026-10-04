//! Frozen installer contract: exercise the shipped binary before initialization.
use serde_json::json;
use std::process::Command;

#[test]
fn standalone_probe_is_exact_and_has_no_update_side_effects() {
    let home = tempfile::tempdir().unwrap();
    let cache = home
        .path()
        .join(".config/recipe-runner-rs/last_update_check");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    // A fresh cache avoids external network in the baseline while exposing notices.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let sentinel = format!("999.0.0\n{now}");
    std::fs::write(&cache, &sentinel).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .arg("--capabilities")
        .env("HOME", home.path())
        .env("RUST_LOG", "trace")
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "0")
        .output()
        .unwrap();
    assert!(out.status.success(), "probe failed: {:?}", out);
    assert!(out.stderr.is_empty(), "probe stderr: {:?}", out.stderr);
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("exactly one JSON object");
    assert_eq!(
        value,
        json!({"schema_version":1,"version":env!("CARGO_PKG_VERSION"),"capabilities":["codex_exec"]})
    );
    assert_eq!(std::fs::read_to_string(cache).unwrap(), sentinel);
}

#[test]
fn mixed_probe_rejects_before_recipe_execution() {
    let home = tempfile::tempdir().unwrap();
    let marker = home.path().join("executed");
    let recipe = home.path().join("recipe.yaml");
    std::fs::write(
        &recipe,
        format!(
            "name: forbidden\nsteps:\n  - id: side-effect\n    command: touch {}\n",
            marker.display()
        ),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .arg("--capabilities")
        .arg(recipe)
        .env("HOME", home.path())
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!marker.exists());
}
