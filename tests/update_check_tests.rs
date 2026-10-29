//! `--no-update-check` turns the startup update check off for one run: the
//! built binary writes no update cache file under HOME.
use std::process::Command;

#[test]
fn no_update_check_option_writes_no_update_cache() {
    let home = tempfile::tempdir().unwrap();
    let recipe = home.path().join("recipe.yaml");
    std::fs::write(
        &recipe,
        "name: quiet\nsteps:\n  - id: one\n    command: echo one\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .args(["--no-update-check", "--validate-only"])
        .arg(&recipe)
        .env("HOME", home.path())
        .env_remove("RECIPE_RUNNER_NO_UPDATE_CHECK")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !home
            .path()
            .join(".config/recipe-runner-rs/last_update_check")
            .exists(),
        "the update check ran despite --no-update-check"
    );
}
