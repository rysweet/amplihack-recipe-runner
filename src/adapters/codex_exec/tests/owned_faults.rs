//! Real owned probe faults and late group termination through public execution.
use super::super::{CleanupFailure, process::OwnedProcess};
use crate::{
    adapters::{Adapter, cli_subprocess::CLISubprocessAdapter},
    runner::RecipeRunner,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

fn run(case: &str) {
    super::retirement_fixtures::run_case(
        case,
        "adapters::codex_exec::tests::owned_faults::owned_fault_worker",
    );
}
fn events() -> String {
    fs::read_to_string(std::env::var("LIFECYCLE_EVENTS").unwrap()).unwrap()
}
fn launcher() -> i32 {
    events()
        .lines()
        .find(|s| s.starts_with("owned_launcher "))
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .strip_prefix("value=")
        .unwrap()
        .parse()
        .unwrap()
}
// Observe before worker exit under default SIGCHLD; consume leftovers on RED.
fn reaped(pid: i32) {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    let errno = std::io::Error::last_os_error().raw_os_error();
    if rc != -1 || errno != Some(libc::ECHILD) {
        unsafe {
            libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG);
        }
    }
    println!(
        "after_return_launcher_waitid rc={rc} errno={errno:?} pid={}",
        unsafe { info.si_pid() }
    );
    assert_eq!(
        (rc, errno),
        (-1, Some(libc::ECHILD)),
        "launcher remained owned/waitable"
    );
}
#[test]
fn owned_fault_worker() {
    let Ok(case) = std::env::var("LIFECYCLE_CASE") else {
        return;
    };
    let mode = if case.starts_with("probe_") {
        14
    } else if case.starts_with("wait_") {
        15
    } else if case.starts_with("late_") {
        16
    } else {
        17
    };
    super::retirement_fixtures::arm(0, 0, 0, 0, mode);
    let root = tempfile::tempdir().unwrap();
    let started = Instant::now();
    if case.ends_with("owned") {
        let mut command = Command::new("/bin/cat");
        command.arg("-");
        let mut process = OwnedProcess::spawn(command, root.path(), root.path(), &[]).unwrap();
        let result = process.deliver_and_wait("safe task", Some(3), &AtomicBool::new(false));
        let mut failures = Vec::new();
        process.shutdown(&mut failures);
        assert!(
            failures.is_empty(),
            "later explicit cleanup failed: {failures:?}"
        );
        reaped(process.child.id() as i32);
        let error = result.unwrap_err();
        assert!(
            error.downcast_ref::<CleanupFailure>().is_some(),
            "{error:#}"
        );
        assert!(
            error.downcast_ref::<std::io::Error>().is_some(),
            "original error lost"
        );
    } else {
        let binary = root.path().join("launcher");
        fs::write(&binary, format!("#!/bin/sh\nprintf launch >> launches\nwhile [ $# -gt 0 ]; do\n if [ \"$1\" = --output-last-message ]; then shift; final=$1; fi\n shift\ndone\n{}\ncat >/dev/null\nprintf '{{\"ok\":true}}' > \"$final\"\n",
            if mode == 16 { "exec /bin/sleep 30" } else { "" })).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        unsafe {
            std::env::set_var("AMPLIHACK_LAUNCHER_BINARY", &binary);
            std::env::set_var("AMPLIHACK_SESSION_DEPTH", "0");
            std::env::set_var("AMPLIHACK_RATELIMIT_MAX_RETRIES", "0");
        }
        let adapter = CLISubprocessAdapter::new().with_binary("codex");
        if case.ends_with("adapter") {
            let result = adapter.execute_agent_step(
                "safe fixture",
                None,
                None,
                None,
                root.path().to_str().unwrap(),
                None,
                Some(1),
            );
            reaped(launcher());
            if mode == 17 {
                assert_eq!(result.unwrap(), "{\"ok\":true}");
            } else {
                let error = result.unwrap_err();
                assert!(
                    error.downcast_ref::<CleanupFailure>().is_some(),
                    "{error:#}"
                );
                let text = format!("{error:#}");
                if mode == 16 {
                    for detail in [
                        "TERM Codex launcher",
                        "kill Codex launcher",
                        "Timed out reaping",
                        "Operation not permitted",
                    ] {
                        assert!(text.contains(detail), "lost {detail}: {text}");
                    }
                } else {
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    assert!(!text.contains("cleanup:"), "later cleanup failed: {text}");
                }
            }
        } else {
            let mut recipe = crate::parse_recipe("name: owned-fault\nsteps:\n  - id: owned\n    type: agent\n    prompt: safe fixture\n    parse_json: true\n    timeout: 1\n    fatal: false\n  - id: pending-bash\n    type: bash\n    command: 'printf admitted > pending'\n  - id: pending-agent\n    prompt: pending\n").unwrap();
            if case.ends_with("nested") {
                let child = root.path().join("child.yaml");
                fs::write(&child, serde_yaml::to_string(&recipe).unwrap()).unwrap();
                recipe = crate::parse_recipe("name: nested-fault\nsteps:\n  - id: child\n    type: recipe\n    recipe: child\n    recovery_on_failure: true\n    fatal: false\n  - id: outer-pending\n    type: bash\n    command: 'printf admitted > outer-pending'\n").unwrap();
            }
            let result = RecipeRunner::new(adapter)
                .with_working_dir(root.path().to_str().unwrap())
                .with_auto_stage(false)
                .execute(&recipe, None);
            // Controls run additional launchers; fault cases must admit only owned.
            reaped(launcher());
            if mode == 17 {
                assert!(result.success, "{result:?}");
                assert!(root.path().join("pending").exists());
            } else {
                assert!(!result.success, "{result:?}");
                assert!(!root.path().join("outer-pending").exists());
                assert!(
                    !root.path().join("pending").exists(),
                    "pending Bash admitted"
                );
                assert_eq!(
                    result.step_results.len(),
                    1,
                    "pending agent admitted: {result:?}"
                );
                let detail = format!("{result:?}");
                assert!(
                    detail.contains(if mode == 16 {
                        "Operation not permitted"
                    } else {
                        "Input/output error"
                    }),
                    "lost primary: {detail}"
                );
                assert_eq!(
                    events().matches("owned_launcher ").count(),
                    1,
                    "repair/retry/admission"
                );
            }
        }
    }
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "cleanup budget renewed"
    );
    let log = events();
    if mode == 14 {
        assert_eq!(log.matches("delivery_anchor_EIO").count(), 1);
    }
    if mode == 15 {
        assert_eq!(log.matches("delivery_launcher_EIO").count(), 1);
    }
    if mode == 16 {
        assert_eq!(log.matches("direct_signal_denied").count(), 2);
        assert!(log.contains("launcher_reaped"));
    }
    assert!(
        log.matches("anchor_wait_forwarded").count() >= 3,
        "later waits missing"
    );
    assert_eq!(
        log.matches("group_signal_forwarded").count(),
        if mode == 17 && case.ends_with("runner") {
            4
        } else {
            2
        }
    );
}
macro_rules! contract {
    ($name:ident, $case:literal) => {
        #[test]
        fn $name() {
            run($case);
        }
    };
}
contract!(
    anchor_probe_failure_survives_successful_cleanup,
    "probe_owned"
);
contract!(
    launcher_wait_failure_survives_successful_cleanup,
    "wait_owned"
);
contract!(public_adapter_retains_owned_probe_failure, "probe_adapter");
contract!(public_adapter_retains_owned_wait_failure, "wait_adapter");
contract!(owned_probe_failure_blocks_real_pending_work, "probe_runner");
contract!(owned_wait_failure_blocks_real_pending_work, "wait_runner");
contract!(late_group_termination_reaps_public_launcher, "late_adapter");
contract!(
    late_group_termination_blocks_real_pending_work,
    "late_runner"
);
contract!(ordinary_adapter_success_control, "control_adapter");
contract!(ordinary_runner_admission_control, "control_runner");
contract!(
    owned_probe_failure_blocks_real_nested_recovery,
    "probe_nested"
);
contract!(
    owned_wait_failure_blocks_real_nested_recovery,
    "wait_nested"
);
contract!(
    late_group_termination_blocks_real_nested_recovery,
    "late_nested"
);
