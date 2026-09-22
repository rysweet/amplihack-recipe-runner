//! Issue #143 — end-to-end coverage for bash interpreter resolution.
//!
//! These tests exercise the part the in-file unit tests deliberately cannot:
//! the env READ, through a real spawn, all the way past `env_clear()` +
//! `bounded_env`, at every one of the four bash execution arms.
//!
//! `AMPLIHACK_BASH` is set on a CHILD process (`Command::env`) and never on this
//! one. Under edition 2024 `std::env::set_var` is `unsafe` and unsound
//! alongside Cargo's multi-threaded test runner; setting it on the child is
//! sound by construction and is also a truer test — it is exactly how an
//! operator sets it.
//!
//! The probe is an executable wrapper that records the fact that it ran (by
//! writing a marker file) and then `exec`s the real bash, so a recipe step that
//! went through the wrapper both succeeds AND leaves proof. A step that
//! silently fell back to `/bin/bash` succeeds without the marker — which is the
//! failure mode #143 is about, and is what these assertions catch.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_recipe-runner-rs");

/// Write `body` to `path` and make it mode 0o755.
fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

/// A stand-in bash: touches `marker`, then hands off to the real `/bin/bash`
/// with the argv it was given. Works for both the `-c <cmd>` and the
/// `<script-file>` forms, so one wrapper covers all four arms.
fn write_wrapper_bash(path: &Path, marker: &Path) {
    write_executable(
        path,
        &format!(
            "#!/bin/sh\n\
             printf 'wrapper-ran' > '{}'\n\
             exec /bin/bash \"$@\"\n",
            marker.display()
        ),
    );
}

/// A one-step bash recipe. `command` is emitted as a YAML block scalar so an
/// arbitrarily large script needs no escaping.
fn write_recipe(dir: &Path, command: &str, timeout: Option<u64>) -> PathBuf {
    let indented: String = command
        .lines()
        .map(|l| format!("      {l}\n"))
        .collect::<String>();
    let timeout_line = match timeout {
        Some(s) => format!("    timeout: {s}\n"),
        None => String::new(),
    };
    let yaml = format!(
        "name: bashprobe\n\
         description: which bash runs recipe bash steps\n\
         context: {{}}\n\
         steps:\n\
         \x20 - id: probe\n\
         \x20   type: bash\n\
         {timeout_line}    command: |\n{indented}"
    );
    let path = dir.join("bashprobe.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

/// Run the recipe runner with `AMPLIHACK_BASH` set to `bash` on the child.
fn run_recipe(recipe: &Path, work_dir: &Path, amplihack_bash: Option<&Path>) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.arg(recipe)
        .arg("-C")
        .arg(work_dir)
        .arg("--no-auto-stage");
    match amplihack_bash {
        Some(p) => {
            cmd.env("AMPLIHACK_BASH", p);
        }
        None => {
            cmd.env_remove("AMPLIHACK_BASH");
        }
    }
    cmd.output().expect("failed to run recipe-runner binary")
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// ~96 KiB of padding, forcing the step past `BASH_INLINE_LIMIT` (64 KiB) and
/// onto the tempfile-backed arms.
fn large_script(tail: &str) -> String {
    let mut s = String::with_capacity(100 * 1024);
    for i in 0..1300 {
        s.push_str(&format!(
            "# padding line {i:04} aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
        ));
    }
    s.push_str(tail);
    s.push('\n');
    assert!(s.len() > 64 * 1024);
    s
}

// ══════════════════════════════════════════════════════════════════════
// AMPLIHACK_BASH reaches every one of the four arms
// ══════════════════════════════════════════════════════════════════════

/// Shared body: run one arm with the wrapper as `AMPLIHACK_BASH` and assert the
/// wrapper actually executed the step.
fn assert_override_reaches_arm(command: &str, timeout: Option<u64>, arm: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("wrapper.marker");
    let wrapper = tmp.path().join("fake-bash");
    write_wrapper_bash(&wrapper, &marker);

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let recipe = write_recipe(tmp.path(), command, timeout);

    let out = run_recipe(&recipe, &work, Some(&wrapper));

    assert!(
        out.status.success(),
        "[{arm}] recipe must succeed through the overridden interpreter:\n{}",
        combined(&out)
    );
    assert!(
        marker.exists(),
        "[{arm}] AMPLIHACK_BASH was ignored — the step ran through some other \
         interpreter, which is exactly the #143 defect. Output:\n{}",
        combined(&out)
    );
    assert!(
        work.join("step.ran").exists(),
        "[{arm}] the step body did not execute:\n{}",
        combined(&out)
    );
}

/// Arm 4: inline `-c`, no timeout.
#[test]
fn test_amplihack_bash_override_used_for_inline_step() {
    assert_override_reaches_arm("touch step.ran", None, "inline");
}

/// Arm 3: inline `-c` under `timeout`. Today the interpreter here is resolved
/// by `timeout`'s own `execvp` against the CHILD's PATH — a different mechanism
/// and a different PATH from arm 4. After the fix both receive one absolute
/// path, so this must behave identically to the test above.
#[test]
fn test_amplihack_bash_override_used_for_timed_inline_step() {
    assert_override_reaches_arm("touch step.ran", Some(60), "inline+timeout");
}

/// Arm 2: tempfile-backed script (> 64 KiB), no timeout.
#[test]
fn test_amplihack_bash_override_used_for_large_script_step() {
    assert_override_reaches_arm(&large_script("touch step.ran"), None, "file");
}

/// Arm 1: tempfile-backed script under `timeout` — the other `execvp` site.
#[test]
fn test_amplihack_bash_override_used_for_large_script_with_timeout() {
    assert_override_reaches_arm(&large_script("touch step.ran"), Some(60), "file+timeout");
}

/// The override must survive `env_clear()` + `bounded_env` and still be visible
/// to the step itself: `AMPLIHACK_*` is protected, so the child sees it too.
#[test]
fn test_amplihack_bash_is_propagated_to_the_step_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("wrapper.marker");
    let wrapper = tmp.path().join("fake-bash");
    write_wrapper_bash(&wrapper, &marker);

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let recipe = write_recipe(
        tmp.path(),
        "printf '%s' \"$AMPLIHACK_BASH\" > seen.txt",
        None,
    );

    let out = run_recipe(&recipe, &work, Some(&wrapper));
    assert!(out.status.success(), "{}", combined(&out));

    let seen = std::fs::read_to_string(work.join("seen.txt")).unwrap();
    assert_eq!(
        seen,
        wrapper.display().to_string(),
        "AMPLIHACK_BASH must survive env_clear + bounded_env (it is protected)"
    );
}

// ══════════════════════════════════════════════════════════════════════
// Fail loud — never a silent fallback
// ══════════════════════════════════════════════════════════════════════

/// Shared body for every rejection: the run fails, the message names both the
/// variable and the rejected value, and the step's side effect did NOT happen.
/// That last assertion is the one that matters: a silent fall back to
/// `/bin/bash` would leave the run green and the operator's choice ignored.
fn assert_fails_loud(bad: &Path, reason: &str, expect_in_message: &[&str]) {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let recipe = write_recipe(tmp.path(), "touch step.ran", None);

    let out = run_recipe(&recipe, &work, Some(bad));
    let text = combined(&out);

    assert!(
        !out.status.success(),
        "[{reason}] an unusable AMPLIHACK_BASH must fail the run, not be ignored:\n{text}"
    );
    assert!(
        !work.join("step.ran").exists(),
        "[{reason}] the step ran anyway — AMPLIHACK_BASH was silently ignored \
         and some other interpreter was used:\n{text}"
    );
    let lower = text.to_lowercase();
    for needle in expect_in_message {
        assert!(
            lower.contains(&needle.to_lowercase()),
            "[{reason}] message must mention {needle:?}:\n{text}"
        );
    }
}

#[test]
fn test_missing_amplihack_bash_fails_loud() {
    assert_fails_loud(
        Path::new("/nonexistent/definitely/not/here/bash"),
        "missing",
        &["AMPLIHACK_BASH", "/nonexistent/definitely/not/here/bash"],
    );
}

#[test]
fn test_non_executable_amplihack_bash_fails_loud() {
    let tmp = tempfile::tempdir().unwrap();
    let not_exec = tmp.path().join("bash");
    std::fs::write(&not_exec, "#!/bin/sh\nexec /bin/bash \"$@\"\n").unwrap();
    let mut perms = std::fs::metadata(&not_exec).unwrap().permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&not_exec, perms).unwrap();

    assert_fails_loud(
        &not_exec,
        "not executable",
        &["AMPLIHACK_BASH", "not executable"],
    );
}

#[test]
fn test_relative_amplihack_bash_fails_loud() {
    assert_fails_loud(
        Path::new("bash"),
        "relative",
        &["AMPLIHACK_BASH", "absolute"],
    );
}

#[test]
fn test_directory_amplihack_bash_fails_loud() {
    let tmp = tempfile::tempdir().unwrap();
    assert_fails_loud(
        tmp.path(),
        "directory",
        &["AMPLIHACK_BASH", "not a regular file"],
    );
}

/// A pin that is SET but not valid UTF-8 must be rejected, not treated as
/// absent. `env::var(..).ok()` conflates `NotUnicode` with `NotPresent`, which
/// downgrades an explicit operator choice into a silent PATH search — the run
/// stays green while a different interpreter executes. This is the one
/// rejection that cannot be unit-tested through the pure function: the defect
/// lives entirely in the env READ, so it only exists once a real process reads
/// a real variable.
#[test]
fn test_non_utf8_amplihack_bash_fails_loud() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    // Lone 0xFF/0xFE: valid in a POSIX path, never valid UTF-8.
    let bad = OsStr::from_bytes(b"/nonexistent/\xff\xfe/bash");

    assert_fails_loud(
        Path::new(bad),
        "not valid UTF-8",
        &["AMPLIHACK_BASH", "not valid UTF-8"],
    );
}

/// An executable, regular bash under a parent directory the runner cannot
/// traverse must be reported as inaccessible — not as "not a regular file",
/// which sends the operator hunting for a file that is sitting right there.
#[test]
fn test_unreadable_parent_amplihack_bash_reports_permission_denied() {
    let tmp = tempfile::tempdir().unwrap();
    let locked = tmp.path().join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    let bash = locked.join("bash");
    write_executable(&bash, "#!/bin/sh\nexec /bin/bash \"$@\"\n");

    let restore = std::fs::metadata(&locked).unwrap().permissions();
    let mut sealed = restore.clone();
    sealed.set_mode(0o000);
    std::fs::set_permissions(&locked, sealed).unwrap();

    // root ignores the mode bits, so the EACCES this test is about never
    // happens. Restore and skip rather than assert something untrue.
    let traversable = std::fs::metadata(&bash).is_ok();

    let outcome = if traversable {
        None
    } else {
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let recipe = write_recipe(tmp.path(), "touch step.ran", None);
        let out = run_recipe(&recipe, &work, Some(&bash));
        Some((
            combined(&out),
            out.status.success(),
            work.join("step.ran").exists(),
        ))
    };

    // Restore BEFORE asserting so a failure cannot leave an undeletable tempdir.
    std::fs::set_permissions(&locked, restore).unwrap();

    let Some((text, success, step_ran)) = outcome else {
        return;
    };
    assert!(
        !success,
        "an inaccessible AMPLIHACK_BASH must fail the run:\n{text}"
    );
    assert!(
        !step_ran,
        "the step ran anyway — the pin was ignored:\n{text}"
    );
    let lower = text.to_lowercase();
    assert!(lower.contains("amplihack_bash"), "{text}");
    assert!(
        lower.contains("permission denied"),
        "message must name the real cause, not a missing file:\n{text}"
    );
}

/// An unusable `AMPLIHACK_BASH` must be rejected on the timed arms too, not
/// just deferred into a confusing exit-127 from `timeout`'s `execvp`.
#[test]
fn test_missing_amplihack_bash_fails_loud_on_timed_step() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let recipe = write_recipe(tmp.path(), "touch step.ran", Some(60));

    let out = run_recipe(&recipe, &work, Some(Path::new("/nonexistent/bash")));
    let text = combined(&out);

    assert!(!out.status.success(), "{text}");
    assert!(!work.join("step.ran").exists(), "{text}");
    assert!(
        text.to_lowercase().contains("amplihack_bash"),
        "message must name AMPLIHACK_BASH:\n{text}"
    );
}

/// The rejection is diagnosed BEFORE the step's environment or its tempfile are
/// built, so a bad interpreter cannot be masked by an unrelated env-budget
/// failure and cannot leave an orphan tempfile behind.
#[test]
fn test_bad_amplihack_bash_is_rejected_before_env_budget_work() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    // Over BASH_INLINE_LIMIT: the tempfile arm, which must never be reached.
    let recipe = write_recipe(tmp.path(), &large_script("touch step.ran"), None);

    let out = run_recipe(&recipe, &work, Some(Path::new("/nonexistent/bash")));
    let text = combined(&out);

    assert!(!out.status.success(), "{text}");
    assert!(
        text.to_lowercase().contains("amplihack_bash"),
        "the interpreter failure must be the reported cause, not an env/tempfile \
         error downstream of it:\n{text}"
    );
    assert!(!work.join("step.ran").exists(), "{text}");
}

// ══════════════════════════════════════════════════════════════════════
// Unset AMPLIHACK_BASH: existing behaviour is preserved
// ══════════════════════════════════════════════════════════════════════

/// With no override, bash steps keep working exactly as before. Guards against
/// the resolution change breaking the default path on ordinary hosts.
#[test]
fn test_unset_amplihack_bash_still_runs_bash_steps() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let recipe = write_recipe(tmp.path(), "echo BASHPROBE_OK", None);

    let out = run_recipe(&recipe, &work, None);
    assert!(
        out.status.success(),
        "default resolution must keep bash steps working:\n{}",
        combined(&out)
    );
}

/// `AMPLIHACK_BASH=/bin/bash` restores the pre-#143 behaviour exactly. This is
/// the documented escape hatch for the one real behaviour change: after the
/// fix, a bash earlier on PATH wins by default.
#[test]
fn test_amplihack_bash_can_pin_legacy_bin_bash() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let recipe = write_recipe(tmp.path(), "touch step.ran", None);

    let out = run_recipe(&recipe, &work, Some(Path::new("/bin/bash")));
    assert!(out.status.success(), "{}", combined(&out));
    assert!(work.join("step.ran").exists(), "{}", combined(&out));
}

// ══════════════════════════════════════════════════════════════════════
// The resolved interpreter is recorded
// ══════════════════════════════════════════════════════════════════════

/// #143 asks for the resolved interpreter in the run log. This test injects
/// `RUST_LOG=debug` itself, so what it pins is the line's CONTENT and FORMAT —
/// the path and the deciding rule — not that an operator sees it by default.
#[test]
fn test_resolved_interpreter_is_logged() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("wrapper.marker");
    let wrapper = tmp.path().join("fake-bash");
    write_wrapper_bash(&wrapper, &marker);

    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let recipe = write_recipe(tmp.path(), "echo ok", None);

    let out = Command::new(BIN)
        .arg(&recipe)
        .arg("-C")
        .arg(&work)
        .arg("--no-auto-stage")
        .env("AMPLIHACK_BASH", &wrapper)
        .env("RUST_LOG", "debug")
        .output()
        .expect("failed to run recipe-runner binary");

    let text = combined(&out);
    assert!(
        text.contains(&wrapper.display().to_string()),
        "the run log must record the resolved interpreter path:\n{text}"
    );
    assert!(
        text.contains("AMPLIHACK_BASH"),
        "the log must also record WHICH rule decided, so an operator can tell \
         an override from a PATH hit:\n{text}"
    );
}
