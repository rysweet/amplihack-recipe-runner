//! `--no-default-search` limits recipe, sub-recipe and agent-definition lookup
//! to the directories given with `--recipe-dir` and `--agent-dir`.
//!
//! Each test plants a definition in a default location (under HOME, in the
//! working directory's `amplifier-bundle/recipes`, or in a directory named by
//! RECIPE_RUNNER_RECIPE_DIRS), runs the built binary without the option to show
//! that the definition is found there, then with the option to show that it is
//! not, and finally with the option and an explicit directory to show that the
//! explicit directory still resolves. Agent steps go to a launcher script that
//! records its arguments; no agent is started.
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Layout {
    root: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
    env_dir: PathBuf,
    explicit: PathBuf,
}

impl Layout {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let work = root.path().join("work");
        let env_dir = root.path().join("env-recipes");
        let explicit = root.path().join("explicit");
        for dir in [&home, &work, &env_dir, &explicit] {
            std::fs::create_dir_all(dir).unwrap();
        }
        Self {
            root,
            home,
            work,
            env_dir,
            explicit,
        }
    }

    /// Run the binary in `work` with a fixed environment: no AMPLIHACK_HOME or
    /// AMPLIHACK_PACKAGE_RECIPE_DIR, no update check, and
    /// RECIPE_RUNNER_RECIPE_DIRS naming `env_dir`.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"));
        cmd.args(args)
            .current_dir(&self.work)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TMPDIR", std::env::temp_dir())
            .env("HOME", &self.home)
            .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
            .env("RECIPE_RUNNER_RECIPE_DIRS", &self.env_dir);
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn recipe(name: &str) -> String {
    format!("name: {name}\nsteps:\n  - id: one\n    command: echo {name}\n")
}

fn text(out: &Output) -> String {
    format!(
        "status: {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn step_error(out: &Output, step_id: &str) -> String {
    let json: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("JSON result expected ({e}): {}", text(out)));
    json["step_results"]
        .as_array()
        .and_then(|steps| steps.iter().find(|s| s["step_id"] == step_id))
        .unwrap_or_else(|| panic!("no result for step {step_id}: {}", text(out)))["error"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_named_recipe_resolves_only_from_explicit_directories() {
    let l = Layout::new();
    write(
        &l.home
            .join(".amplihack/amplifier-bundle/recipes/planted.yaml"),
        &recipe("planted"),
    );
    write(&l.env_dir.join("from-env.yaml"), &recipe("from-env"));
    for name in ["planted", "from-env"] {
        let out = l.run(&[name, "--validate-only"]);
        assert!(
            out.status.success(),
            "default search finds {name}: {}",
            text(&out)
        );
        let out = l.run(&[name, "--validate-only", "--no-default-search"]);
        assert_eq!(out.status.code(), Some(3), "{name}: {}", text(&out));
    }
    write(&l.explicit.join("planted.yaml"), &recipe("planted"));
    let explicit = l.explicit.to_str().unwrap();
    let out = l.run(&[
        "planted",
        "--validate-only",
        "--no-default-search",
        "-R",
        explicit,
    ]);
    assert!(out.status.success(), "{}", text(&out));
}

#[test]
fn list_shows_only_explicit_directories() {
    let l = Layout::new();
    write(
        &l.home
            .join(".amplihack/amplifier-bundle/recipes/planted.yaml"),
        &recipe("planted"),
    );
    write(&l.env_dir.join("from-env.yaml"), &recipe("from-env"));
    write(&l.explicit.join("chosen.yaml"), &recipe("chosen"));
    let explicit = l.explicit.to_str().unwrap();

    let out = l.run(&["list"]);
    let listed = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        listed.contains("planted") && listed.contains("from-env"),
        "{listed}"
    );

    // The option is accepted after the subcommand as well as before it.
    for args in [
        vec!["list", "--no-default-search"],
        vec!["--no-default-search", "list"],
    ] {
        let out = l.run(&args);
        assert!(out.status.success(), "{args:?}: {}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("No recipes found."),
            "{args:?}: {}",
            text(&out)
        );
    }

    let out = l.run(&["list", "-R", explicit, "--no-default-search"]);
    let listed = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{}", text(&out));
    assert!(listed.contains("chosen"), "{listed}");
    assert!(
        !listed.contains("planted") && !listed.contains("from-env"),
        "{listed}"
    );
}

#[test]
fn a_sub_recipe_resolves_only_from_explicit_directories() {
    let l = Layout::new();
    let explicit = l.explicit.to_str().unwrap();
    // One child per kind of implicit location: anchored at the working
    // directory, under HOME, and named by RECIPE_RUNNER_RECIPE_DIRS.
    let children = [
        ("near-child", l.work.join("amplifier-bundle/recipes")),
        (
            "home-child",
            l.home.join(".amplihack/amplifier-bundle/recipes"),
        ),
        ("env-child", l.env_dir.clone()),
    ];
    for (child, location) in &children {
        write(&location.join(format!("{child}.yaml")), &recipe(child));
        let parent = l.root.path().join(format!("parent-{child}.yaml"));
        write(
            &parent,
            &format!("name: parent\nsteps:\n  - id: call\n    type: recipe\n    recipe: {child}\n"),
        );
        let parent = parent.to_str().unwrap();

        let out = l.run(&[parent, "--output-format", "json"]);
        assert!(
            out.status.success(),
            "default search finds {child}: {}",
            text(&out)
        );

        let out = l.run(&[parent, "--output-format", "json", "--no-default-search"]);
        assert_eq!(out.status.code(), Some(1), "{child}: {}", text(&out));
        let error = step_error(&out, "call");
        assert!(
            error.contains(&format!("Sub-recipe '{child}' not found"))
                && error.contains("default search is off"),
            "{child}: {error}"
        );

        write(&l.explicit.join(format!("{child}.yaml")), &recipe(child));
        let out = l.run(&[
            parent,
            "--output-format",
            "json",
            "--no-default-search",
            "-R",
            explicit,
        ]);
        assert!(out.status.success(), "{child}: {}", text(&out));
    }
}

#[test]
fn an_agent_definition_resolves_only_from_explicit_directories() {
    let l = Layout::new();
    write(
        &l.home.join(".amplihack/.claude/agents/planted.md"),
        "HOME AGENT DEFINITION",
    );
    write(&l.explicit.join("planted.md"), "EXPLICIT AGENT DEFINITION");
    let calls = l.root.path().join("calls");
    std::fs::create_dir(&calls).unwrap();
    // Each call writes its arguments, NUL-separated, to calls/<n>.
    let launcher = l.root.path().join("launcher.sh");
    std::fs::write(
        &launcher,
        format!(
            "#!/usr/bin/env bash\n\
             set -u\n\
             n=$(ls '{calls}' | wc -l)\n\
             for a in \"$@\"; do printf '%s\\0' \"$a\" >> '{calls}/'\"$n\"; done\n\
             echo done\n",
            calls = calls.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let recipe = l.root.path().join("agent.yaml");
    write(
        &recipe,
        "name: agent\nsteps:\n  - id: ask\n    type: agent\n    agent: amplihack:planted\n    prompt: hello\n",
    );
    let recipe = recipe.to_str().unwrap();
    let run = |args: &[&str]| {
        l.command(args)
            .env("AMPLIHACK_LAUNCHER_BINARY", &launcher)
            .env("AMPLIHACK_AGENT_BINARY", "claude")
            .env("AMPLIHACK_RATELIMIT_MAX_RETRIES", "0")
            .output()
            .unwrap()
    };
    let system_prompt = |call: &str| {
        let argv: Vec<String> = std::fs::read(calls.join(call))
            .unwrap_or_else(|e| panic!("launcher call {call} not recorded: {e}"))
            .split(|b| *b == 0)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        argv.windows(2)
            .find(|w| w[0] == "--system-prompt")
            .map(|w| w[1].clone())
            .unwrap_or_else(|| panic!("no --system-prompt in call {call}: {argv:?}"))
    };

    let out = run(&[recipe, "--output-format", "json"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(system_prompt("0").contains("HOME AGENT DEFINITION"));

    let out = run(&[recipe, "--output-format", "json", "--no-default-search"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let error = step_error(&out, "ask");
    assert!(
        error.contains("agent definition 'amplihack:planted' not resolved"),
        "{error}"
    );
    assert!(!calls.join("1").exists(), "the agent step must not launch");

    let explicit = l.explicit.to_str().unwrap();
    let out = run(&[
        recipe,
        "--output-format",
        "json",
        "--no-default-search",
        "--agent-dir",
        explicit,
    ]);
    assert!(out.status.success(), "{}", text(&out));
    let prompt = system_prompt("1");
    assert!(
        prompt.contains("EXPLICIT AGENT DEFINITION") && !prompt.contains("HOME AGENT DEFINITION"),
        "{prompt}"
    );
}
