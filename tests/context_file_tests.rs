//! Issue #211: public CLI behavior, with local shell/launcher substitutes only.
//! No implementation helpers are exposed just for testing. Synchronization uses
//! an inherited socket, never sleeps, readiness polling, or process-name cleanup.

#[cfg(not(unix))]
#[test]
fn unsupported_platform_reports_bad_args_without_advertising_support() {
    use std::process::Command;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("context.json");
    std::fs::write(&path, br#"{"secret":"UNSUPPORTED_CONTEXT_SENTINEL"}"#).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .arg("unused-recipe.yaml")
        .arg("--context-file")
        .arg(&path)
        .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        format!(
            "invalid context file '{}': unsupported platform",
            path.display()
        )
    );
    assert!(out.stdout.is_empty());
    let probe = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .arg("--capabilities")
        .output()
        .unwrap();
    assert!(probe.status.success());
    let report: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert!(
        !report["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "context_file_v1")
    );
}

#[cfg(unix)]
mod unix {
    use serde_json::{Value, json};
    use std::fs::{self, File};
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Output, Stdio};

    const SENTINEL: &str = "CONTEXT_FILE_PRIVATE_211_λ";

    struct Fixture {
        dir: tempfile::TempDir,
        context: PathBuf,
    }

    impl Fixture {
        fn new(recipe: Value) -> Self {
            let dir = tempfile::tempdir().unwrap();
            fs::write(
                dir.path().join("recipe.yaml"),
                serde_yaml::to_string(&recipe).unwrap(),
            )
            .unwrap();
            fs::create_dir(dir.path().join("tmp")).unwrap();
            let context = dir.path().join("context.json");
            Self { dir, context }
        }

        fn simple() -> Self {
            Self::new(json!({"name":"context-file", "steps":[{"id":"one", "command":"printf ok"}]}))
        }

        fn write(&self, value: &Value) {
            fs::write(&self.context, serde_json::to_vec(value).unwrap()).unwrap();
        }

        fn command(&self) -> Command {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"));
            cmd.arg(self.dir.path().join("recipe.yaml"))
                .args([
                    "--output-format",
                    "json",
                    "--no-auto-stage",
                    "--no-update-check",
                    "-C",
                ])
                .arg(self.dir.path())
                .current_dir(self.dir.path())
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", self.dir.path())
                .env("TMPDIR", self.dir.path().join("tmp"))
                .env("AMPLIHACK_BASH", "/bin/bash")
                .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1");
            cmd
        }

        fn file_command(&self) -> Command {
            let mut cmd = self.command();
            cmd.arg("--context-file").arg(&self.context);
            cmd
        }
    }

    fn result(out: Output) -> Value {
        assert!(
            out.status.success(),
            "CLI failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let result: Value = serde_json::from_slice(&out.stdout).expect("one JSON recipe result");
        assert_eq!(result["success"], true);
        result
    }

    fn bad_args(out: Output, path: &Path, categories: &[&str], private: &[&str]) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(4), "{stderr}");
        let allowed: Vec<_> = categories
            .iter()
            .map(|category| format!("invalid context file '{}': {category}", path.display()))
            .collect();
        assert!(
            allowed.iter().any(|expected| stderr.trim() == expected),
            "uncontrolled diagnostic: {stderr}"
        );
        assert!(
            stdout.is_empty(),
            "invalid context must not execute the recipe"
        );
        for value in private {
            assert!(
                !stderr.contains(value) && !stdout.contains(value),
                "fixture content disclosed"
            );
        }
    }

    #[test]
    fn file_and_set_deliver_equivalent_bash_environment_and_agent_prompt() {
        let f = Fixture::new(json!({
            "name":"delivery",
            "steps":[
                {"id":"bash", "command":"printf '%s|%s|%s|%s|%s' \"$RECIPE_VAR_task_description\" \"$RECIPE_VAR_enabled\" \"$RECIPE_VAR_count\" \"$RECIPE_VAR_nested\" {{task_description}}"},
                {"id":"agent", "type":"agent", "prompt":"{{task_description}}|{{enabled}}|{{count}}|{{nested}}"}
            ]
        }));
        // Existing Codex stdin/final-message protocol avoids dependence on the
        // separate #205 Claude prompt transport work. No agent service runs.
        let launcher = f.dir.path().join("launcher");
        fs::write(&launcher, "#!/bin/sh\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --output-last-message ]; then shift; final=$1; fi\n  shift\ndone\ncat > \"$final\"\n").unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
        let text = format!("{SENTINEL} quotes ' \" ; $(printf unsafe)\nsecond line");
        let values = json!({"task_description":text, "enabled":"true", "count":"42", "nested":"{\"inside\":\"42\"}"});
        f.write(&values);
        let mut from_set = f.command();
        for (key, value) in values.as_object().unwrap() {
            from_set
                .arg("--set")
                .arg(format!("{key}={}", value.as_str().unwrap()));
        }
        from_set
            .arg("--launcher")
            .arg(&launcher)
            .args(["--agent-binary", "codex"]);
        let baseline = result(from_set.output().unwrap());
        let prefix = format!("{text}|true|42|{{\"inside\":\"42\"}}");
        assert_eq!(
            baseline["step_results"][0]["output"],
            format!("{prefix}|{text}")
        );
        assert!(
            baseline["step_results"][1]["output"]
                .as_str()
                .unwrap()
                .contains(&prefix)
        );
        let from_file = result(
            f.file_command()
                .arg("--launcher")
                .arg(&launcher)
                .args(["--agent-binary", "codex"])
                .output()
                .unwrap(),
        );
        assert_eq!(from_file["context"], baseline["context"]);
        for index in 0..2 {
            assert_eq!(
                from_file["step_results"][index]["output"],
                baseline["step_results"][index]["output"]
            );
        }
    }

    #[test]
    fn only_top_level_strings_are_normalized_and_all_native_json_types_survive() {
        let f = Fixture::simple();
        f.write(&json!({
            "lower_true":"true", "capital_false":"False", "integer":"42", "leading_zero":"0042",
            "float":"1.25", "string_null":"null", "string_array":"[\"true\",42]",
            "string_object":"{\"nested\":\"42\"}", "quoted":"\"true\"", "empty":"", "text":SENTINEL,
            "boolean":false, "number":2.5, "null":null,
            "array":["true", "42", null, false, 3, {"leaf":"False"}], "object":{"bool":"true", "num":"42"}
        }));
        let expected = json!({
            "lower_true":true, "capital_false":false, "integer":42, "leading_zero":42,
            "float":1.25, "string_null":null, "string_array":["true",42],
            "string_object":{"nested":"42"}, "quoted":"\"true\"", "empty":"", "text":SENTINEL,
            "boolean":false, "number":2.5, "null":null,
            "array":["true", "42", null, false, 3, {"leaf":"False"}], "object":{"bool":"true", "num":"42"}
        });
        let actual = result(f.file_command().output().unwrap());
        for (key, value) in expected.as_object().unwrap() {
            assert_eq!(&actual["context"][key], value, "context key {key}");
        }
    }

    #[test]
    fn defaults_then_file_then_set_precedence_is_independent_of_option_order() {
        let f = Fixture::new(
            json!({"name":"precedence", "context":{"winner":"default", "file_wins":"default", "default_only":7}, "steps":[{"id":"one", "command":"printf ok"}]}),
        );
        f.write(&json!({"winner":"file", "file_wins":"file", "file_only":true}));
        for file_first in [true, false] {
            let mut cmd = f.command();
            if file_first {
                cmd.arg("--context-file").arg(&f.context);
            }
            cmd.args(["--set", "winner=first", "--set", "winner=42"]);
            if !file_first {
                cmd.arg("--context-file").arg(&f.context);
            }
            let actual = result(cmd.output().unwrap());
            assert_eq!(actual["context"]["winner"], 42);
            assert_eq!(actual["context"]["file_wins"], "file");
            assert_eq!(actual["context"]["default_only"], 7);
            assert_eq!(actual["context"]["file_only"], true);
        }
    }

    #[test]
    fn empty_object_and_trailing_whitespace_preserve_existing_no_file_behavior() {
        let f = Fixture::simple();
        let baseline = result(f.command().args(["--set", "answer=42"]).output().unwrap());
        for contents in ["{}", "{} \n\t\r\n"] {
            fs::write(&f.context, contents).unwrap();
            let loaded = result(
                f.file_command()
                    .args(["--set", "answer=42"])
                    .output()
                    .unwrap(),
            );
            assert_eq!(loaded["context"], baseline["context"]);
            assert_eq!(loaded["step_results"][0]["output"], "ok");
        }
    }

    #[test]
    fn duplicate_object_keys_keep_existing_json_parser_last_value_behavior() {
        let f = Fixture::simple();
        fs::write(
            &f.context,
            r#"{"answer":"false","answer":"42","nested":{"key":1,"key":"true"}}"#,
        )
        .unwrap();
        let actual = result(f.file_command().output().unwrap());
        assert_eq!(actual["context"]["answer"], 42);
        assert_eq!(actual["context"]["nested"], json!({"key":"true"}));
    }

    #[test]
    fn malformed_json_invalid_utf8_and_trailing_values_have_controlled_diagnostics() {
        let f = Fixture::simple();
        let cases: Vec<(Vec<u8>, &[&str])> = vec![
            (Vec::new(), &["invalid JSON"]),
            (
                format!("{{\"secret\":\"{SENTINEL}\", broken}}").into_bytes(),
                &["invalid JSON"],
            ),
            (
                format!("{{\"secret\":\"{SENTINEL}\"}} {{}}").into_bytes(),
                &["invalid JSON"],
            ),
            (
                [
                    format!("{{\"secret\":\"{SENTINEL}").into_bytes(),
                    vec![0xff],
                    b"\"}".to_vec(),
                ]
                .concat(),
                &["invalid JSON", "cannot read file"],
            ),
        ];
        for (contents, categories) in cases {
            fs::write(&f.context, contents).unwrap();
            bad_args(
                f.file_command().output().unwrap(),
                &f.context,
                categories,
                &[SENTINEL, "broken"],
            );
        }
    }

    #[test]
    fn every_non_object_root_is_bad_args_without_content_disclosure() {
        let f = Fixture::simple();
        for value in [
            json!(SENTINEL),
            json!([SENTINEL]),
            json!(true),
            json!(42),
            Value::Null,
        ] {
            f.write(&value);
            bad_args(
                f.file_command().output().unwrap(),
                &f.context,
                &["expected JSON object"],
                &[SENTINEL],
            );
        }
    }

    #[test]
    fn missing_and_unreadable_files_return_bad_args() {
        let f = Fixture::simple();
        bad_args(
            f.file_command().output().unwrap(),
            &f.context,
            &["cannot open file"],
            &[SENTINEL],
        );
        f.write(&json!({"secret":SENTINEL}));
        fs::set_permissions(&f.context, fs::Permissions::from_mode(0o000)).unwrap();
        let mut cmd = f.file_command();
        // A root test process must shed DAC override for a real access failure.
        // SAFETY: geteuid only reads the current process identity.
        if unsafe { libc::geteuid() } == 0 {
            cmd.gid(65534).uid(65534);
        }
        let out = cmd.output().unwrap();
        fs::set_permissions(&f.context, fs::Permissions::from_mode(0o600)).unwrap();
        bad_args(out, &f.context, &["cannot open file"], &[SENTINEL]);
    }

    #[test]
    fn final_component_symlinks_and_directories_are_not_loaded() {
        let f = Fixture::simple();
        let target = f.dir.path().join("target.json");
        let contents = serde_json::to_vec(&json!({"secret":SENTINEL})).unwrap();
        fs::write(&target, &contents).unwrap();
        symlink(&target, &f.context).unwrap();
        bad_args(
            f.file_command().output().unwrap(),
            &f.context,
            &["cannot open file"],
            &[SENTINEL],
        );
        assert!(
            fs::symlink_metadata(&f.context)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&target).unwrap(), contents);
        fs::remove_file(&f.context).unwrap();
        fs::create_dir(&f.context).unwrap();
        bad_args(
            f.file_command().output().unwrap(),
            &f.context,
            &["not a regular file"],
            &[SENTINEL],
        );
    }

    #[test]
    fn fifo_socket_and_device_inputs_return_bad_args_without_waiting_for_a_writer() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let f = Fixture::simple();
        let name = CString::new(f.context.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a live NUL-terminated pathname in this test's directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        bad_args(
            f.file_command().output().unwrap(),
            &f.context,
            &["not a regular file"],
            &[SENTINEL],
        );
        fs::remove_file(&f.context).unwrap();
        let _socket = UnixListener::bind(&f.context).unwrap();
        bad_args(
            f.file_command().output().unwrap(),
            &f.context,
            &["cannot open file", "not a regular file"],
            &[SENTINEL],
        );
        let device = Path::new("/dev/null");
        bad_args(
            f.command()
                .arg("--context-file")
                .arg(device)
                .output()
                .unwrap(),
            device,
            &["not a regular file"],
            &[SENTINEL],
        );
    }

    #[test]
    fn parent_symlinks_and_ordinary_permissions_are_allowed_and_file_is_untouched() {
        let f = Fixture::simple();
        f.write(&json!({"secret":SENTINEL}));
        fs::set_permissions(&f.context, fs::Permissions::from_mode(0o644)).unwrap();
        let before_bytes = fs::read(&f.context).unwrap();
        let before = fs::metadata(&f.context).unwrap();
        let parent_link = f.dir.path().join("parent-link");
        symlink(f.dir.path(), &parent_link).unwrap();
        let actual = result(
            f.command()
                .arg("--context-file")
                .arg(parent_link.join("context.json"))
                .output()
                .unwrap(),
        );
        assert_eq!(actual["context"]["secret"], SENTINEL);
        assert_eq!(fs::read(&f.context).unwrap(), before_bytes);
        let after = fs::metadata(&f.context).unwrap();
        assert_eq!(
            (
                after.dev(),
                after.ino(),
                after.mode(),
                after.mtime(),
                after.mtime_nsec()
            ),
            (
                before.dev(),
                before.ino(),
                before.mode(),
                before.mtime(),
                before.mtime_nsec()
            )
        );
    }

    #[test]
    fn trace_diagnostics_do_not_disclose_loaded_values_when_conditions_execute() {
        let f = Fixture::new(json!({"name":"diagnostics", "steps":[
            {"id":"truthy", "condition":"secret", "command":"printf ok"},
            {"id":"method", "condition":"secret.strip() != ''", "command":"printf ok"},
            {"id":"structured", "condition":"structured", "command":"printf ok"}
        ]}));
        f.write(&json!({"secret":SENTINEL, "structured":{"leaf":"NESTED_PRIVATE_211"}}));
        let out = f
            .file_command()
            .arg("--set")
            .arg(format!("same_value={SENTINEL}"))
            .env("RUST_LOG", "trace")
            .output()
            .unwrap();
        let diagnostics = String::from_utf8_lossy(&out.stderr).into_owned();
        let actual = result(out);
        assert!(
            diagnostics.contains("TRACE"),
            "trace logging must actually be active"
        );
        assert!(!diagnostics.contains(SENTINEL));
        assert!(!diagnostics.contains("NESTED_PRIVATE_211"));
        for step in actual["step_results"].as_array().unwrap() {
            assert_eq!(step["status"], "completed");
        }
        // Intentional JSON result output still retains context values.
        assert_eq!(actual["context"]["secret"], SENTINEL);
    }

    #[test]
    fn trace_diagnostics_do_not_disclose_member_names_inside_loaded_objects() {
        let f = Fixture::new(json!({"name":"private-object-members", "steps":[
            {"id":"one", "condition":"structured",
             "command":"printf '%s' \"$RECIPE_VAR_structured__NESTED_MEMBER_PRIVATE_211__leaf\""}
        ]}));
        for leaf in ["ordinary".to_string(), "x".repeat(130 * 1024)] {
            let structured = json!({"NESTED_MEMBER_PRIVATE_211":{"leaf":leaf}});
            f.write(&json!({"structured":structured}));
            let out = f.file_command().env("RUST_LOG", "trace").output().unwrap();
            let diagnostics = String::from_utf8_lossy(&out.stderr);
            assert!(diagnostics.contains("TRACE"));
            assert!(!diagnostics.contains("NESTED_MEMBER_PRIVATE_211"));
            if leaf.len() > 130_000 {
                assert!(diagnostics.contains("Spilled"));
            }
            let actual = result(out);
            assert_eq!(actual["step_results"][0]["output"], leaf);
            assert_eq!(actual["context"]["structured"], structured);
        }
    }

    #[test]
    fn spill_rejection_and_drop_diagnostics_do_not_disclose_nested_member_names() {
        let f = Fixture::simple();
        let mut nul_leaf = "x".repeat(130 * 1024);
        nul_leaf.push('\0');
        f.write(&json!({"structured":{"NESTED_MEMBER_PRIVATE_211":{"leaf":nul_leaf}}}));
        let out = f.file_command().env("RUST_LOG", "trace").output().unwrap();
        assert_private_diagnostics(&out, "NESTED_MEMBER_PRIVATE_211", false);
        failed_step(&out, "contains a NUL byte");

        let structured = json!({"NESTED_MEMBER_PRIVATE_211?":{"leaf":"x".repeat(130 * 1024)}});
        f.write(&json!({"structured":structured}));
        let out = f.file_command().env("RUST_LOG", "trace").output().unwrap();
        assert_private_diagnostics(&out, "NESTED_MEMBER_PRIVATE_211", false);
        assert!(String::from_utf8_lossy(&out.stderr).contains("Dropping env var"));
        assert_eq!(result(out)["context"]["structured"], structured);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn aggregate_env_budget_failure_keeps_nested_member_names_private() {
        let f = Fixture::simple();
        let mut structured = serde_json::Map::new();
        for index in 0..4 {
            // Individually under the per-value cap, but the invalid names
            // cannot be spilled when the combined environment exceeds budget.
            structured.insert(
                format!("NESTED_MEMBER_PRIVATE_211_{index}?"),
                Value::String("x".repeat(100_000)),
            );
        }
        f.write(&json!({"structured":structured}));
        let mut cmd = f.file_command();
        cmd.env("RUST_LOG", "trace");
        // SAFETY: only async-signal-safe resource-limit calls run after fork.
        // Bound this child's env budget independently of the host stack limit.
        unsafe {
            cmd.pre_exec(|| {
                let mut limit = std::mem::zeroed::<libc::rlimit>();
                if libc::getrlimit(libc::RLIMIT_STACK, &mut limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                limit.rlim_cur = limit.rlim_cur.min(1024 * 1024);
                if libc::setrlimit(libc::RLIMIT_STACK, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let out = cmd.output().unwrap();
        assert_private_diagnostics(&out, "NESTED_MEMBER_PRIVATE_211", false);
        failed_step(&out, "child environment exceeds");
    }

    fn assert_private_diagnostics(out: &Output, sentinel: &str, debug: bool) {
        let diagnostics = String::from_utf8_lossy(&out.stderr);
        assert!(!diagnostics.contains(sentinel), "context value disclosed");
        if debug {
            assert!(
                diagnostics.contains("DEBUG"),
                "debug logging must be active"
            );
        }
    }

    fn failed_step(out: &Output, category: &str) -> Value {
        assert_eq!(out.status.code(), Some(1));
        let diagnostics = String::from_utf8_lossy(&out.stderr);
        assert!(
            diagnostics.contains(category),
            "failure must remain visible: {diagnostics}"
        );
        let actual: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(actual["success"], false);
        assert_eq!(actual["step_results"][0]["status"], "failed");
        assert!(
            actual["step_results"][0]["error"]
                .as_str()
                .unwrap()
                .contains(category)
        );
        actual
    }

    #[test]
    fn rendered_bash_working_directory_is_used_without_disclosing_it_in_debug_logs() {
        let f = Fixture::new(json!({"name":"bash-directory", "steps":[
            {"id":"one", "working_dir":"{{private_dir}}", "command":"pwd -P; printf ran > bash-marker"}
        ]}));
        let private_dir = f.dir.path().join(SENTINEL);
        fs::create_dir(&private_dir).unwrap();
        f.write(&json!({"private_dir":private_dir}));
        let out = f.file_command().env("RUST_LOG", "debug").output().unwrap();
        assert_private_diagnostics(&out, SENTINEL, true);
        let actual = result(out);
        assert_eq!(
            actual["step_results"][0]["output"].as_str().unwrap().trim(),
            private_dir.canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(
            fs::read_to_string(private_dir.join("bash-marker")).unwrap(),
            "ran"
        );
        assert!(!f.dir.path().join("bash-marker").exists());
    }

    #[test]
    fn missing_rendered_bash_working_directory_fails_without_disclosing_it() {
        let f = Fixture::new(json!({"name":"bash-directory", "steps":[
            {"id":"one", "working_dir":"{{private_dir}}", "command":"printf ran > bash-marker"}
        ]}));
        f.write(&json!({"private_dir":f.dir.path().join(SENTINEL)}));
        for debug in [false, true] {
            let mut cmd = f.file_command();
            if debug {
                cmd.env("RUST_LOG", "debug");
            }
            let out = cmd.output().unwrap();
            assert_private_diagnostics(&out, SENTINEL, debug);
            let actual = failed_step(&out, "bash step failed");
            assert!(
                !actual["step_results"][0]["error"]
                    .as_str()
                    .unwrap()
                    .contains(SENTINEL)
            );
            assert!(!f.dir.path().join("bash-marker").exists());
        }
    }

    // Exercises both the ordinary subprocess and Codex launch paths locally.
    // The marker records cwd independently of the runner's intentional output.
    fn directory_launcher(f: &Fixture) -> PathBuf {
        let launcher = f.dir.path().join("launcher");
        fs::write(&launcher, "#!/bin/sh\npwd -P > \"${0%/*}/agent-cwd\"\nfinal=\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --output-last-message ]; then shift; final=$1; fi\n  shift\ndone\nif [ -n \"$final\" ]; then\n  cat > /dev/null\n  printf launched > \"$final\"\nelse\n  printf launched\nfi\n").unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
        launcher
    }

    #[test]
    fn rendered_agent_working_directory_is_used_without_disclosing_it_in_debug_logs() {
        let f = Fixture::new(json!({"name":"agent-directory", "steps":[
            {"id":"one", "type":"agent", "working_dir":"{{private_dir}}", "prompt":"Run the controlled launcher"}
        ]}));
        let private_dir = f.dir.path().join(SENTINEL);
        fs::create_dir(&private_dir).unwrap();
        f.write(&json!({"private_dir":private_dir}));
        let launcher = directory_launcher(&f);
        let marker = f.dir.path().join("agent-cwd");
        for agent in ["claude", "codex"] {
            let out = f
                .file_command()
                .arg("--launcher")
                .arg(&launcher)
                .args(["--agent-binary", agent])
                .env("RUST_LOG", "debug")
                .output()
                .unwrap();
            assert_private_diagnostics(&out, SENTINEL, true);
            let actual = result(out);
            assert_eq!(actual["step_results"][0]["output"], "launched");
            assert_eq!(
                fs::read_to_string(&marker).unwrap().trim(),
                private_dir.canonicalize().unwrap().to_str().unwrap()
            );
            fs::remove_file(&marker).unwrap();
        }
    }

    #[test]
    fn missing_rendered_agent_working_directory_fails_privately_before_launch() {
        let f = Fixture::new(json!({"name":"agent-directory", "steps":[
            {"id":"one", "type":"agent", "working_dir":"{{private_dir}}", "prompt":"Must not launch"}
        ]}));
        f.write(&json!({"private_dir":f.dir.path().join(SENTINEL)}));
        let launcher = directory_launcher(&f);
        for agent in ["claude", "codex"] {
            for debug in [false, true] {
                let mut cmd = f.file_command();
                cmd.arg("--launcher")
                    .arg(&launcher)
                    .args(["--agent-binary", agent]);
                if debug {
                    cmd.env("RUST_LOG", "debug");
                }
                let out = cmd.output().unwrap();
                assert_private_diagnostics(&out, SENTINEL, debug);
                let actual = failed_step(&out, "working directory does not exist");
                assert!(
                    !actual["step_results"][0]["error"]
                        .as_str()
                        .unwrap()
                        .contains(SENTINEL)
                );
                assert!(!f.dir.path().join("agent-cwd").exists(), "agent launched");
            }
        }
    }

    const PRIVATE_RECIPE: &str = "CONTEXT_FILE_PRIVATE_RECIPE_211";

    fn sub_recipe_fixture() -> Fixture {
        let f = Fixture::new(json!({"name":"parent", "steps":[
            {"id":"dispatch", "recipe":"{{private_name}}"}
        ]}));
        f.write(&json!({"private_name":PRIVATE_RECIPE}));
        f
    }

    #[test]
    fn rendered_sub_recipe_lookup_succeeds_without_disclosing_its_name_in_debug_logs() {
        let f = sub_recipe_fixture();
        fs::write(f.dir.path().join(format!("{PRIVATE_RECIPE}.yaml")),
            "name: child\nsteps:\n  - id: child-step\n    command: 'printf child-ok > child-marker; printf child-ok'\n"
        ).unwrap();
        let out = f.file_command().env("RUST_LOG", "debug").output().unwrap();
        assert_private_diagnostics(&out, PRIVATE_RECIPE, true);
        assert!(String::from_utf8_lossy(&out.stderr).contains("find_recipe_path"));
        let actual = result(out);
        assert_eq!(actual["step_results"][0]["status"], "completed");
        assert_eq!(
            fs::read_to_string(f.dir.path().join("child-marker")).unwrap(),
            "child-ok"
        );
    }

    #[test]
    fn missing_rendered_sub_recipe_fails_without_disclosing_its_name() {
        let f = sub_recipe_fixture();
        for debug in [false, true] {
            let mut cmd = f.file_command();
            if debug {
                cmd.env("RUST_LOG", "debug");
            }
            let out = cmd.output().unwrap();
            assert_private_diagnostics(&out, PRIVATE_RECIPE, debug);
            let actual = failed_step(&out, "Sub-recipe could not be found");
            assert!(
                !actual["step_results"][0]["error"]
                    .as_str()
                    .unwrap()
                    .contains(PRIVATE_RECIPE)
            );
        }
    }

    #[test]
    fn invalid_rendered_sub_recipe_name_fails_without_disclosing_it() {
        let f = sub_recipe_fixture();
        f.write(&json!({"private_name":format!("../{PRIVATE_RECIPE}")}));
        let out = f.file_command().env("RUST_LOG", "debug").output().unwrap();
        assert_private_diagnostics(&out, PRIVATE_RECIPE, true);
        let actual = failed_step(&out, "Invalid resolved sub-recipe name");
        assert!(
            !actual["step_results"][0]["error"]
                .as_str()
                .unwrap()
                .contains(PRIVATE_RECIPE)
        );
    }

    #[test]
    fn rejected_sub_recipe_symlink_does_not_disclose_the_candidate_filename() {
        let f = sub_recipe_fixture();
        let outside = tempfile::tempdir().unwrap();
        let child = outside.path().join("child.yaml");
        fs::write(
            &child,
            "name: child\nsteps:\n  - id: one\n    command: 'printf ran > child-marker'\n",
        )
        .unwrap();
        symlink(&child, f.dir.path().join(format!("{PRIVATE_RECIPE}.yaml"))).unwrap();
        let out = f.file_command().env("RUST_LOG", "debug").output().unwrap();
        assert_private_diagnostics(&out, PRIVATE_RECIPE, true);
        let diagnostics = String::from_utf8_lossy(&out.stderr);
        assert!(diagnostics.contains("outside anchored search roots"));
        failed_step(&out, "Sub-recipe could not be found");
        assert!(!f.dir.path().join("child-marker").exists());
        assert!(!outside.path().join("child-marker").exists());
    }

    #[test]
    fn rendered_sub_recipe_parse_extends_and_execution_failures_keep_names_private() {
        let f = sub_recipe_fixture();
        for (yaml, category) in [
            (
                format!("name: child\nsteps: [{PRIVATE_RECIPE}"),
                "Failed to parse sub-recipe",
            ),
            (
                format!(
                    "name: child\nextends: {PRIVATE_RECIPE}-missing\nsteps:\n  - id: one\n    command: true\n"
                ),
                "Failed to resolve extends for sub-recipe",
            ),
            (
                "name: child\nsteps:\n  - id: one\n    command: 'exit 1'\n".to_string(),
                "Sub-recipe failed",
            ),
        ] {
            fs::write(f.dir.path().join(format!("{PRIVATE_RECIPE}.yaml")), yaml).unwrap();
            let out = f.file_command().env("RUST_LOG", "debug").output().unwrap();
            assert_private_diagnostics(&out, PRIVATE_RECIPE, true);
            let actual = failed_step(&out, category);
            assert!(
                !actual["step_results"][0]["error"]
                    .as_str()
                    .unwrap()
                    .contains(PRIVATE_RECIPE)
            );
        }
    }

    #[test]
    fn condition_type_errors_do_not_print_the_loaded_value_in_diagnostics() {
        for condition in ["structured.strip()", "int(secret)", "float(secret)"] {
            let f = Fixture::new(json!({"name":"condition-error", "steps":[
                {"id":"invalid-condition", "condition":condition, "command":"printf should-not-run"}
            ]}));
            f.write(&json!({"secret":SENTINEL, "structured":{"secret":SENTINEL}}));
            let out = f.file_command().env("RUST_LOG", "trace").output().unwrap();
            assert_eq!(out.status.code(), Some(1));
            let diagnostics = String::from_utf8_lossy(&out.stderr);
            assert!(!diagnostics.is_empty(), "trace diagnostics must be active");
            assert!(
                !diagnostics.contains(SENTINEL),
                "condition error disclosed context for {condition}"
            );
            let actual: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(actual["step_results"][0]["status"], "failed");
            assert_eq!(actual["step_results"][0]["output"], "");
        }
    }

    #[test]
    fn required_json_failure_does_not_copy_file_loaded_output_into_diagnostics() {
        let f = Fixture::new(json!({"name":"private-json-error", "steps":[
            {"id":"invalid-json", "command":"printf '%s' \"$RECIPE_VAR_secret\"",
             "parse_json":true, "parse_json_required":true}
        ]}));
        f.write(&json!({"secret":SENTINEL}));
        for level in ["error", "trace"] {
            let out = f.file_command().env("RUST_LOG", level).output().unwrap();
            assert_private_diagnostics(&out, SENTINEL, false);
            let actual = failed_step(&out, "parse_json failed");
            assert_eq!(actual["step_results"][0]["output"], "");
            // Intentional result context remains available to the caller.
            assert_eq!(actual["context"]["secret"], SENTINEL);
        }
    }

    #[test]
    fn malformed_set_warning_does_not_disclose_a_file_loaded_value() {
        let f = Fixture::simple();
        f.write(&json!({"secret":SENTINEL}));
        let out = f
            .file_command()
            .args(["--set", SENTINEL])
            .env("RUST_LOG", "trace")
            .output()
            .unwrap();
        let diagnostics = String::from_utf8_lossy(&out.stderr);
        assert!(diagnostics.contains("ignoring malformed context override"));
        assert!(!diagnostics.contains(SENTINEL));
        assert_eq!(result(out)["context"]["secret"], SENTINEL);
    }

    #[test]
    fn context_validation_diagnostics_do_not_disclose_loaded_paths() {
        for (rule, exists) in [("path", false), ("git_repo", false), ("git_repo", true)] {
            let f = Fixture::new(json!({
                "name":"validation-error", "context_validation":{"private_path":rule},
                "steps":[{"id":"one", "command":"printf should-not-run"}]
            }));
            let private_path = f.dir.path().join(SENTINEL);
            if exists {
                fs::create_dir(&private_path).unwrap();
            }
            f.write(&json!({"private_path":private_path}));
            let out = f.file_command().env("RUST_LOG", "trace").output().unwrap();
            assert_eq!(out.status.code(), Some(1));
            let diagnostics = String::from_utf8_lossy(&out.stderr);
            assert!(diagnostics.contains("private_path"));
            assert!(!diagnostics.contains(SENTINEL));
            let actual: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(actual["step_results"][0]["status"], "failed");
            assert_eq!(actual["step_results"][0]["output"], "");
        }
    }

    #[test]
    fn git_validation_uses_loaded_path_as_cwd_without_putting_it_on_argv() {
        let f = Fixture::new(json!({
            "name":"git-validation", "context_validation":{"private_path":"git_repo"},
            "steps":[{"id":"one", "command":"printf should-not-run"}]
        }));
        let private_path = f.dir.path().join(SENTINEL);
        fs::create_dir(&private_path).unwrap();
        f.write(&json!({"private_path":private_path}));
        let bin = f.dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let git = bin.join("git");
        fs::write(
            &git,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$GIT_ARGUMENT_LOG\"\npwd > \"$GIT_CWD_LOG\"\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
        let argv = f.dir.path().join("git.argv");
        let cwd = f.dir.path().join("git.cwd");
        let out = f
            .file_command()
            .env("PATH", bin)
            .env("GIT_ARGUMENT_LOG", &argv)
            .env("GIT_CWD_LOG", &cwd)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(fs::read_to_string(argv).unwrap(), "rev-parse\n--git-dir\n");
        assert_eq!(
            fs::read_to_string(cwd).unwrap().trim(),
            private_path.to_str().unwrap()
        );
        assert!(!String::from_utf8_lossy(&out.stderr).contains(SENTINEL));
    }

    /// A socket inherited by the runner and its bash child sends readiness and
    /// receives release. EOF reports a CLI startup failure instead of hanging.
    /// Output goes to files so diagnostics cannot fill a pipe before readiness.
    struct LiveRun {
        child: Option<Child>,
        control: UnixStream,
        stdout: PathBuf,
        stderr: PathBuf,
    }

    impl LiveRun {
        fn start(f: &Fixture, mut cmd: Command) -> Self {
            let (control, inherited) = UnixStream::pair().unwrap();
            let source = inherited.as_raw_fd();
            let stdout = f.dir.path().join("stdout");
            let stderr = f.dir.path().join("stderr");
            cmd.stdout(Stdio::from(File::create(&stdout).unwrap()))
                .stderr(Stdio::from(File::create(&stderr).unwrap()));
            // SAFETY: only async-signal-safe descriptor operations run in the
            // forked child. FD 198 is a private test synchronization endpoint.
            unsafe {
                cmd.pre_exec(move || {
                    if libc::dup2(source, 198) == -1 || libc::fcntl(198, libc::F_SETFD, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = cmd.spawn().unwrap();
            drop(inherited);
            Self {
                child: Some(child),
                control,
                stdout,
                stderr,
            }
        }

        fn ready(&mut self) -> u32 {
            let mut line = String::new();
            BufReader::new(&mut self.control)
                .read_line(&mut line)
                .unwrap();
            line.trim().parse().unwrap_or_else(|_| {
                panic!(
                    "runner exited before bash readiness: {}",
                    fs::read_to_string(&self.stderr).unwrap()
                )
            })
        }

        fn finish(mut self) -> Value {
            self.control.write_all(b"release\n").unwrap();
            let status = self.child.as_mut().unwrap().wait().unwrap();
            self.child.take();
            result(Output {
                status,
                stdout: fs::read(&self.stdout).unwrap(),
                stderr: fs::read(&self.stderr).unwrap(),
            })
        }
    }

    impl Drop for LiveRun {
        fn drop(&mut self) {
            if let Some(child) = self.child.as_mut() {
                // Release any bash step first, including on assertion failure.
                let _ = self.control.write_all(b"release\n");
                let _ = self.control.shutdown(std::net::Shutdown::Write);
                // Child::kill targets only the recorded runner PID we spawned.
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn live_fixture() -> Fixture {
        let f = Fixture::new(json!({"name":"startup-only", "steps":[
            {"id":"ready", "command":"printf '%s\\n' \"$BASHPID\" >&198; IFS= read -r release <&198; printf '%s' \"$RECIPE_VAR_secret\""},
            {"id":"later", "command":"printf '%s' {{secret}}"}
        ]}));
        f.write(&json!({"secret":SENTINEL}));
        f
    }

    #[test]
    fn changing_the_file_after_startup_does_not_change_later_steps_or_rewrite_it() {
        let f = live_fixture();
        // Exercise the handshake against the existing --set path first, so a
        // broken synchronization fixture cannot masquerade as a missing feature.
        let mut baseline_command = f.command();
        baseline_command
            .arg("--set")
            .arg(format!("secret={SENTINEL}"));
        let mut baseline = LiveRun::start(&f, baseline_command);
        baseline.ready();
        assert_eq!(baseline.finish()["step_results"][1]["output"], SENTINEL);

        let mut live = LiveRun::start(&f, f.file_command());
        live.ready();
        // Invalid replacement detects both a reread and any rewrite on exit.
        let replacement = b"caller replaced context after startup; not JSON";
        fs::write(&f.context, replacement).unwrap();
        let actual = live.finish();
        for step in actual["step_results"].as_array().unwrap() {
            assert_eq!(step["output"], SENTINEL);
        }
        assert_eq!(actual["context"]["secret"], SENTINEL);
        assert_eq!(fs::read(&f.context).unwrap(), replacement);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_runner_and_bash_cmdlines_contain_no_file_context_value() {
        let f = live_fixture();
        let mut live = LiveRun::start(&f, f.file_command());
        let bash_pid = live.ready();
        let runner_pid = live.child.as_ref().unwrap().id();
        let runner_argv = fs::read(format!("/proc/{runner_pid}/cmdline")).unwrap();
        assert!(
            runner_argv
                .split(|b| *b == 0)
                .any(|arg| arg == b"--context-file")
        );
        for pid in [runner_pid, bash_pid] {
            let cmdline = fs::read(format!("/proc/{pid}/cmdline")).unwrap();
            assert!(
                !String::from_utf8_lossy(&cmdline).contains(SENTINEL),
                "context appeared on argv for PID {pid}"
            );
        }
        // Successful startup also closes the caller-owned input descriptor.
        let descriptors = fs::read_dir(format!("/proc/{runner_pid}/fd")).unwrap();
        for descriptor in descriptors {
            if let Ok(target) = fs::read_link(descriptor.unwrap().path()) {
                assert_ne!(
                    target, f.context,
                    "context descriptor remains open after startup"
                );
            }
        }
        live.finish();
    }
}
