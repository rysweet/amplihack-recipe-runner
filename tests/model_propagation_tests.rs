//! The step's model reaches the agent launcher on the JSON repair call,
//! through the real CLI subprocess adapter and a launcher script that records
//! the arguments of each call.
use recipe_runner_rs::adapters::cli_subprocess::CLISubprocessAdapter;
use recipe_runner_rs::parser::RecipeParser;
use recipe_runner_rs::runner::RecipeRunner;
use std::os::unix::fs::PermissionsExt;

fn recorded_arguments(path: &std::path::Path) -> Vec<String> {
    std::fs::read(path)
        .unwrap_or_else(|e| panic!("launcher did not record {}: {e}", path.display()))
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

#[test]
fn json_repair_passes_the_step_model_to_the_launcher() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = dir.path().join("launcher.sh");
    let count = dir.path().join("count");
    // Each call writes its arguments, NUL-separated, to argv-<call>; the first
    // call answers with text that is not JSON, the second with JSON.
    std::fs::write(
        &launcher,
        format!(
            "#!/usr/bin/env bash\n\
             set -u\n\
             c=$(cat '{count}' 2>/dev/null || echo 0)\n\
             c=$((c+1))\n\
             echo \"$c\" > '{count}'\n\
             : > '{dir}/argv-'\"$c\"\n\
             for a in \"$@\"; do printf '%s\\0' \"$a\" >> '{dir}/argv-'\"$c\"; done\n\
             if [ \"$c\" = 1 ]; then echo 'not json'; else echo '{{\"ok\":true}}'; fi\n",
            count = count.display(),
            dir = dir.path().display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: this test binary holds one test, so no other thread reads the
    // environment while it is changed.
    unsafe {
        std::env::set_var("AMPLIHACK_LAUNCHER_BINARY", &launcher);
        std::env::set_var("AMPLIHACK_AGENT_BINARY", "claude");
        std::env::set_var("AMPLIHACK_RATELIMIT_MAX_RETRIES", "0");
    }
    let recipe = RecipeParser::new()
        .parse(
            "name: repair\nsteps:\n  - id: ask\n    type: agent\n    prompt: give json\n    model: explicit\n    parse_json: true\n",
        )
        .unwrap();
    let result = RecipeRunner::new(CLISubprocessAdapter::new())
        .with_working_dir(dir.path().to_str().unwrap())
        .with_auto_stage(false)
        .execute(&recipe, None);
    assert!(result.success, "{result:?}");
    for call in ["argv-1", "argv-2"] {
        let argv = recorded_arguments(&dir.path().join(call));
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--model" && w[1] == "explicit"),
            "{call} must pass the step's model: {argv:?}"
        );
    }
    assert!(
        !dir.path().join("argv-3").exists(),
        "exactly one repair call"
    );
}
