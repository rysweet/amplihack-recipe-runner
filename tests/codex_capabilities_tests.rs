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
        json!({"schema_version":1,"version":env!("CARGO_PKG_VERSION"),"capabilities":if cfg!(unix) { vec!["codex_exec", "durable_recovery_v1", "context_file_v1", "agent_prompt_file_v1"] } else { vec![] }})
    );
    assert_eq!(std::fs::read_to_string(cache).unwrap(), sentinel);
}

#[test]
fn agent_prompt_transport_is_advertised_as_its_own_member() {
    // #205/#211: consumers detect off-argv agent prompt delivery by this exact
    // member; it is not implied by `context_file_v1`.
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .arg("--capabilities")
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "probe failed: {:?}", out);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let members = value["capabilities"].as_array().unwrap();
    let count = |name: &str| members.iter().filter(|v| *v == name).count();
    assert_eq!(count("agent_prompt_file_v1"), usize::from(cfg!(unix)));
    assert_eq!(count("context_file_v1"), usize::from(cfg!(unix)));
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

#[test]
fn mixed_context_file_probe_exits_before_filesystem_access() {
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("missing-context.json");
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .args(["--capabilities", "--context-file"])
        .arg(&missing)
        .env("HOME", home.path())
        .env("RUST_LOG", "trace")
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "--capabilities must be used alone"
    );
    assert!(!missing.exists());
}
