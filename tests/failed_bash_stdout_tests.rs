//! Regression coverage for issue #190 through the compiled public CLI.
//! Child environments are synthetic; these tests never select real profiles.

#![cfg(unix)]

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_recipe-runner-rs");
const RETAINED: &str = "Failed bash stdout retained at ";
const RETENTION_FAILED: &str = "Failed to retain bash stdout: ";

struct Fixture {
    root: Option<tempfile::TempDir>,
    runs: Cell<usize>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("rr190-cli-")
            .tempdir()
            .unwrap();
        for name in ["home", "tmp", "config", "cache", "work"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }
        Self {
            root: Some(root),
            runs: Cell::new(0),
        }
    }

    fn path(&self) -> &Path {
        self.root.as_ref().unwrap().path()
    }

    fn run(&self, script: &str, log_dir: Option<&Path>) -> Output {
        self.run_with_stderr(script, log_dir, false)
    }

    fn run_with_stderr(&self, script: &str, log_dir: Option<&Path>, closed: bool) -> Output {
        let indented = script
            .lines()
            .map(|line| format!("      {line}\n"))
            .collect::<String>();
        self.run_document(&format!("name: failed-stdout-regression\nsteps:\n  - id: probe\n    type: bash\n    command: |\n{indented}"), log_dir, closed, "json")
    }

    fn run_document(
        &self,
        document: &str,
        log_dir: Option<&Path>,
        closed: bool,
        format: &str,
    ) -> Output {
        let run = self.runs.get();
        self.runs.set(run + 1);
        let recipe = self.path().join(format!("recipe-{run}.yaml"));
        std::fs::write(&recipe, document).unwrap();
        let mut command = Command::new(BIN);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.path().join("home"))
            .env("TMPDIR", self.path().join("tmp"))
            .env("XDG_CONFIG_HOME", self.path().join("config"))
            .env("XDG_CACHE_HOME", self.path().join("cache"))
            .env("AMPLIHACK_BASH", "/bin/bash")
            .env("RECIPE_RUNNER_NO_UPDATE_CHECK", "1")
            .current_dir(self.path())
            .arg(&recipe)
            .arg("-C")
            .arg(self.path().join("work"))
            .args([
                "--no-auto-stage",
                "--no-update-check",
                "--output-format",
                format,
            ]);
        if let Some(dir) = log_dir {
            command.arg("--log-dir").arg(dir);
        }
        if closed {
            use std::os::unix::process::CommandExt;
            // SAFETY: only the forked child closes its own diagnostic descriptor.
            unsafe {
                command.pre_exec(|| {
                    if libc::close(libc::STDERR_FILENO) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let output = command.output().expect("run compiled CLI");
        std::fs::write(
            self.path().join(format!("cli-{run}.stdout")),
            &output.stdout,
        )
        .unwrap();
        std::fs::write(
            self.path().join(format!("cli-{run}.stderr")),
            &output.stderr,
        )
        .unwrap();
        output
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let path = self.root.take().unwrap().keep();
            eprintln!("Preserved failed CLI fixture and complete streams at {path:?}");
        }
    }
}

fn failed_result(output: &Output, stderr: &str) -> serde_json::Value {
    assert_eq!(output.status.code(), Some(1), "CLI must remain failed");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("CLI JSON");
    assert_eq!(result["success"], false);
    let step = &result["step_results"][0];
    assert_eq!(step["status"], "failed");
    assert_eq!(step["output"], "");
    assert_eq!(
        step["error"],
        format!("Step 'probe' failed: bash step failed: Command failed (exit 7): {stderr}")
    );
    if !stderr.is_empty() {
        assert!(String::from_utf8_lossy(&output.stderr).contains(stderr));
    }
    result
}

fn metadata(output: &Output) -> serde_json::Value {
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("CLI JSON");
    let records = result["diagnostic_artifacts"]
        .as_array()
        .expect("top-level diagnostic metadata");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["step_id"], "probe");
    assert!(!String::from_utf8_lossy(&output.stderr).contains(RETAINED));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(RETENTION_FAILED));
    records[0].clone()
}

fn retained_path(output: &Output) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let record = metadata(output);
    assert!(record.get("retention_error").is_none());
    let path = if let Some(text) = record["path"].as_str() {
        assert!(record.get("path_bytes").is_none());
        PathBuf::from(text)
    } else {
        let raw: Vec<u8> =
            serde_json::from_value(record["path_bytes"].clone()).expect("lossless Unix path bytes");
        PathBuf::from(std::ffi::OsString::from_vec(raw))
    };
    assert!(path.is_absolute());
    path
}

#[test]
fn failed_bash_stdout_default_location_survives_cli_exit() {
    let fixture = Fixture::new();
    let output = fixture.run(
        "printf 'RR190_STDOUT_DIAGNOSTIC\\n'; printf 'RR190_STDERR_DIAGNOSTIC\\n' >&2; exit 7",
        None,
    );
    // These assertions must pass before the missing-retention regression fires.
    failed_result(&output, "RR190_STDERR_DIAGNOSTIC");
    let path = retained_path(&output);
    assert_eq!(path.parent().unwrap(), fixture.path().join("tmp"));
    assert_eq!(std::fs::read(path).unwrap(), b"RR190_STDOUT_DIAGNOSTIC\n");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("RR190_STDOUT_DIAGNOSTIC"));
}

#[test]
fn configured_locations_retain_byte_exact_stdout_only_failures() {
    for relative in [false, true] {
        let fixture = Fixture::new();
        let relative_dir = Path::new("logs with spaces/nested");
        let absolute_dir = fixture.path().join(relative_dir);
        let configured = if relative {
            relative_dir
        } else {
            absolute_dir.as_path()
        };
        let mut bytes = b" \t\n\0\xff\xfeRR190_PRIVATE_PAYLOAD\n".to_vec();
        bytes.extend((0..150_000).map(|i| (i % 256) as u8));
        bytes.extend_from_slice(b"\n\n \t");
        std::fs::write(fixture.path().join("work/payload.bin"), &bytes).unwrap();
        let output = fixture.run("cat payload.bin; exit 7", Some(configured));
        failed_result(&output, "");
        let path = retained_path(&output);
        assert_eq!(path.parent().unwrap(), absolute_dir);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("RR190_PRIVATE_PAYLOAD"));
        assert_eq!(
            std::fs::read_dir(fixture.path().join("tmp"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn unusable_configured_location_reports_retention_failure_without_fallback() {
    let fixture = Fixture::new();
    let log_dir = fixture.path().join("authentication failed\n\u{1b}[31m");
    std::fs::write(&log_dir, b"existing file").unwrap();
    let output = fixture.run(
        "printf 'RR190_PRIVATE_PAYLOAD'; printf 'original diagnostic' >&2; exit 7",
        Some(&log_dir),
    );
    failed_result(&output, "original diagnostic");
    let record = metadata(&output);
    assert!(
        record["retention_error"]
            .as_str()
            .is_some_and(|problem| !problem.is_empty())
    );
    let quoted_directory = serde_json::to_string(log_dir.to_str().unwrap()).unwrap();
    assert!(
        record["retention_error"]
            .as_str()
            .unwrap()
            .contains(&quoted_directory),
        "retention problems must identify the configured directory in metadata"
    );
    assert!(record.get("path").is_none());
    assert!(record.get("path_bytes").is_none());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("RR190_PRIVATE_PAYLOAD"));
    assert!(!output.stdout.contains(&0x1b));
    assert_eq!(std::fs::read(&log_dir).unwrap(), b"existing file");
    assert_eq!(
        std::fs::read_dir(fixture.path().join("tmp"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn retention_problem_identifies_non_utf8_directory_losslessly() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let fixture = Fixture::new();
    let logs = fixture.path().join(std::ffi::OsString::from_vec(
        b"raw-\xff-\xfe-\n-\x1b-api error: 401".to_vec(),
    ));
    std::fs::write(&logs, b"untouched").unwrap();
    let output = fixture.run("printf 'private bytes'; exit 7", Some(&logs));
    failed_result(&output, "");
    let record = metadata(&output);
    let quoted_directory = format!(
        "b\"{}\"",
        logs.as_os_str()
            .as_bytes()
            .iter()
            .map(|byte| format!("\\x{byte:02x}"))
            .collect::<String>()
    );
    assert!(
        record["retention_error"]
            .as_str()
            .unwrap()
            .contains(&quoted_directory)
    );
    assert!(record.get("path").is_none());
    assert!(record.get("path_bytes").is_none());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private bytes"));
    assert!(!output.stdout.contains(&0x1b));
    assert_eq!(std::fs::read(&logs).unwrap(), b"untouched");
    assert_eq!(
        std::fs::read_dir(fixture.path().join("tmp"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn empty_failures_and_successful_output_create_no_failure_artifacts() {
    for (script, success, expected) in [
        ("printf 'original diagnostic' >&2; exit 7", false, ""),
        (
            "printf ' \\n\\377ok\\n\\t'; printf 'ignored stderr' >&2",
            true,
            "\u{fffd}ok",
        ),
    ] {
        let fixture = Fixture::new();
        let logs = fixture.path().join("unused-logs");
        let output = fixture.run(script, Some(&logs));
        if success {
            assert_eq!(output.status.code(), Some(0));
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["success"], true);
            assert_eq!(result["step_results"][0]["status"], "completed");
            assert_eq!(result["step_results"][0]["output"], expected);
        } else {
            failed_result(&output, "original diagnostic");
        }
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(result.get("diagnostic_artifacts").is_none());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains(RETAINED));
        assert!(!stderr.contains(RETENTION_FAILED));
        assert!(!logs.exists());
        assert_eq!(
            std::fs::read_dir(fixture.path().join("tmp"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn artifact_pointer_quotes_and_escapes_control_characters() {
    let fixture = Fixture::new();
    let logs = fixture
        .path()
        .join("logs-é-🦀-connection reset by peer\n\r\t\u{1b}[31m\"\\");
    let output = fixture.run("printf 'private bytes'; exit 7", Some(&logs));
    failed_result(&output, "");
    let record = metadata(&output);
    assert!(
        record["path"]
            .as_str()
            .unwrap()
            .contains("connection reset by peer")
    );
    assert!(!output.stdout.contains(&0x1b));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private bytes"));
    let path = retained_path(&output);
    assert_eq!(path.parent().unwrap(), logs);
    assert_eq!(std::fs::read(&path).unwrap(), b"private bytes");
    let paths = std::fs::read_dir(&logs)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(std::fs::read(paths[0].path()).unwrap(), b"private bytes");
}

#[test]
fn non_utf8_pointer_round_trips_every_original_path_byte() {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let logs = fixture.path().join(std::ffi::OsString::from_vec(
        b"raw-\xff-\xfe-\\xFF-\"-\n-\x1b-api error: 401".to_vec(),
    ));
    let payload = b" \0\xff\xfeprivate bytes\n\n";
    std::fs::write(fixture.path().join("work/payload.bin"), payload).unwrap();
    let output = fixture.run("cat payload.bin; exit 7", Some(&logs));
    failed_result(&output, "");
    let path = retained_path(&output);
    assert_eq!(path.parent().unwrap(), logs);
    assert_eq!(std::fs::read(&path).unwrap(), payload);
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let record = metadata(&output);
    assert!(record.get("path").is_none());
    assert!(record["path_bytes"].is_array());
    assert!(!output.stdout.contains(&0x1b));
    let text = fixture.run_document("name: text-bytes\nsteps:\n  - id: probe\n    type: bash\n    command: cat payload.bin; exit 7\n", Some(&logs), false, "text");
    assert_eq!(text.status.code(), Some(1));
    let stdout = String::from_utf8(text.stdout).unwrap();
    let pointer = stdout
        .lines()
        .find_map(|line| line.strip_prefix(RETAINED))
        .unwrap();
    let escaped = pointer
        .strip_prefix("b\"")
        .unwrap()
        .strip_suffix('"')
        .unwrap();
    assert_eq!(escaped.len() % 4, 0);
    let raw = escaped
        .as_bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|unit| {
            assert_eq!(&unit[..2], b"\\x");
            u8::from_str_radix(std::str::from_utf8(&unit[2..]).unwrap(), 16).unwrap()
        })
        .collect::<Vec<_>>();
    let path = PathBuf::from(std::ffi::OsString::from_vec(raw));
    assert_eq!(path.parent().unwrap(), logs);
    assert_eq!(std::fs::read(path).unwrap(), payload);
    assert!(!String::from_utf8_lossy(&text.stderr).contains(RETAINED));
}

#[test]
fn unavailable_diagnostic_stderr_preserves_failure_and_retained_stdout() {
    for unusable in [false, true] {
        let fixture = Fixture::new();
        let logs = fixture.path().join("logs");
        if unusable {
            std::fs::write(&logs, b"existing file").unwrap();
        }
        let output = fixture.run_with_stderr("printf 'private bytes'; exit 7", Some(&logs), true);
        failed_result(&output, "");
        assert!(output.stderr.is_empty());
        if unusable {
            assert!(metadata(&output)["retention_error"].is_string());
            assert_eq!(std::fs::read(&logs).unwrap(), b"existing file");
        } else {
            assert_eq!(
                std::fs::read(retained_path(&output)).unwrap(),
                b"private bytes"
            );
            let paths = std::fs::read_dir(&logs)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(paths.len(), 1);
            assert_eq!(std::fs::read(paths[0].path()).unwrap(), b"private bytes");
        }
    }
}

#[test]
fn failed_stdout_metadata_is_separate_from_original_failure_channels() {
    let fixture = Fixture::new();
    let logs = fixture.path().join("connection reset by peer");
    let baseline = fixture.run("printf 'original diagnostic' >&2; exit 7", None);
    let candidate = fixture.run(
        "printf 'private stdout'; printf 'original diagnostic' >&2; exit 7",
        Some(&logs),
    );
    let before = failed_result(&baseline, "original diagnostic");
    let after = failed_result(&candidate, "original diagnostic");
    assert_eq!(
        after["step_results"][0]["error"],
        before["step_results"][0]["error"]
    );
    assert_eq!(after["step_results"][0]["output"], "");
    let records = after["diagnostic_artifacts"]
        .as_array()
        .expect("nonempty failed stdout requires top-level typed diagnostic metadata");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["step_id"], "probe");
    assert_eq!(
        std::fs::read(records[0]["path"].as_str().unwrap()).unwrap(),
        b"private stdout"
    );
    // Only the existing timestamp can vary between otherwise identical failures.
    let normalise = |bytes: &[u8]| {
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .lines()
            .map(|line| {
                line.split_once(" ERROR ")
                    .map_or(line, |(_, tail)| tail)
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(normalise(&candidate.stderr), normalise(&baseline.stderr));
    assert!(!String::from_utf8_lossy(&candidate.stdout).contains("private stdout"));
}

#[test]
fn default_text_reports_references_and_problems_on_stdout_only() {
    let fixture = Fixture::new();
    let document = "name: text\nsteps:\n  - id: probe\n    type: bash\n    command: printf 'private bytes'; exit 7\n";
    let logs = fixture.path().join("logs with spaces\n\u{1b}");
    let output = fixture.run_document(document, Some(&logs), false, "text");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let pointer = stdout
        .lines()
        .find_map(|line| line.strip_prefix(RETAINED))
        .expect("text stdout reference");
    let path: String = serde_json::from_str(pointer).expect("quoted usable path");
    assert_eq!(std::fs::read(path).unwrap(), b"private bytes");
    assert!(!stdout.contains("private bytes"));
    assert!(!stdout.contains('\u{1b}'));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(RETAINED));
    let blocker = fixture.path().join("api error: 401");
    std::fs::write(&blocker, b"untouched").unwrap();
    let output = fixture.run_document(document, Some(&blocker), false, "text");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains(RETENTION_FAILED));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(RETENTION_FAILED));
}

#[test]
fn parallel_nonfatal_and_nested_diagnostics_reach_only_top_level_metadata() {
    let fixture = Fixture::new();
    let logs = fixture.path().join("logs");
    std::fs::write(fixture.path().join("child.yaml"), "name: child\nsteps:\n  - id: child-failure\n    type: bash\n    command: printf child-private; exit 7\n    fatal: false\n  - id: child-ok\n    type: bash\n    command: printf child-success\n").unwrap();
    let output = fixture.run_document("name: parent\ncontext:\n  sentinel: unchanged\nsteps:\n  - id: first\n    type: bash\n    command: printf first-private; exit 7\n    fatal: false\n  - id: second\n    type: bash\n    command: printf second-private; exit 7\n    parallel_group: pair\n    fatal: false\n  - id: third\n    type: bash\n    command: printf third-private; exit 7\n    parallel_group: pair\n    fatal: false\n  - id: nested\n    type: recipe\n    recipe: child\n  - id: after\n    type: bash\n    command: env | sort\n", Some(&logs), false, "json");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let records = result["diagnostic_artifacts"].as_array().unwrap();
    let mut ids = records
        .iter()
        .map(|r| r["step_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, ["child-failure", "first", "second", "third"]);
    for record in records {
        let bytes = std::fs::read(record["path"].as_str().unwrap()).unwrap();
        assert!(bytes.ends_with(b"-private"));
    }
    let steps = result["step_results"].as_array().unwrap();
    for step in steps {
        assert!(
            !step["output"]
                .as_str()
                .unwrap()
                .contains("amplihack-failed-bash-stdout")
        );
        assert!(
            !step["error"]
                .as_str()
                .unwrap()
                .contains("amplihack-failed-bash-stdout")
        );
    }
    let env = steps.last().unwrap()["output"].as_str().unwrap();
    assert!(env.contains("RECIPE_VAR_sentinel=unchanged"));
    assert!(!env.to_ascii_lowercase().contains("diagnostic_artifacts"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(RETAINED));
}

#[test]
fn fatal_parallel_and_nested_failures_preserve_all_metadata_and_original_errors() {
    for nested in [false, true] {
        let fixture = Fixture::new();
        let logs = fixture.path().join("connection reset by peer");
        let child = "name: child\nsteps:\n  - id: first\n    type: bash\n    command: PAYLOAD; printf original >&2; exit 7\n    parallel_group: pair\n  - id: second\n    type: bash\n    command: PAYLOAD; printf original >&2; exit 7\n    parallel_group: pair\n  - id: after\n    type: bash\n    command: printf must-not-run\n";
        let parent = "name: parent\nsteps:\n  - id: nested\n    type: recipe\n    recipe: child\n  - id: after\n    type: bash\n    command: printf must-not-run\n";
        let mut results = Vec::new();
        for payload in ["true", "printf private-bytes"] {
            let document = child.replace("PAYLOAD", payload);
            let document = if nested {
                std::fs::write(fixture.path().join("child.yaml"), document).unwrap();
                parent
            } else {
                &document
            };
            let output = fixture.run_document(document, Some(&logs), false, "json");
            assert_eq!(output.status.code(), Some(1));
            let mut result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["success"], false);
            if payload != "true" {
                let records = result["diagnostic_artifacts"].as_array().unwrap();
                assert_eq!(records.len(), 2);
                let mut ids = records
                    .iter()
                    .map(|r| r["step_id"].as_str().unwrap())
                    .collect::<Vec<_>>();
                ids.sort();
                assert_eq!(ids, ["first", "second"]);
                for record in records {
                    let path = Path::new(record["path"].as_str().unwrap());
                    assert_eq!(path.parent().unwrap(), logs);
                    assert_eq!(std::fs::read(path).unwrap(), b"private-bytes");
                }
            } else {
                assert!(result.get("diagnostic_artifacts").is_none());
            }
            assert!(!String::from_utf8_lossy(&output.stderr).contains(RETAINED));
            assert!(!String::from_utf8_lossy(&output.stderr).contains(RETENTION_FAILED));
            assert!(!String::from_utf8_lossy(&output.stderr).contains("private-bytes"));
            result
                .as_object_mut()
                .unwrap()
                .remove("diagnostic_artifacts");
            result.as_object_mut().unwrap().remove("duration");
            for step in result["step_results"].as_array_mut().unwrap() {
                step.as_object_mut().unwrap().remove("duration");
                assert_eq!(step["status"], "failed");
                assert_eq!(step["output"], "");
                assert!(
                    step["error"]
                        .as_str()
                        .unwrap()
                        .contains("Command failed (exit 7): original")
                );
                assert_ne!(step["step_id"], "after");
            }
            results.push(result);
        }
        assert_eq!(
            results[0], results[1],
            "metadata must not alter the ordinary failed result"
        );
    }
}
