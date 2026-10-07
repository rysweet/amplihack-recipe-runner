//! Actual unblocked signals while product dispositions are owned; no live model.
use super::super::{
    CleanupFailure, Interruption, cancellation::Cancellation, check_cancellation, finish_resources,
};
use crate::adapters::{Adapter, cli_subprocess::CLISubprocessAdapter};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicUsize, Ordering},
};
static PRIOR_CALLS: AtomicUsize = AtomicUsize::new(0);
extern "C" fn previous(_: i32) {
    PRIOR_CALLS.fetch_add(1, Ordering::SeqCst);
}

fn actions() -> [libc::sigaction; 2] {
    [libc::SIGINT, libc::SIGTERM].map(|signal| unsafe {
        let mut action = std::mem::zeroed();
        assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut action), 0);
        action
    })
}
fn install_prior() -> [libc::sigaction; 2] {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = previous as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaddset(&mut action.sa_mask, libc::SIGUSR1);
            assert_eq!(libc::sigaction(signal, &action, std::ptr::null_mut()), 0);
            let mut unblock = std::mem::zeroed();
            libc::sigemptyset(&mut unblock);
            libc::sigaddset(&mut unblock, signal);
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &unblock, std::ptr::null_mut()),
                0
            );
        }
    }
    actions()
}
fn assert_prior(prior: &[libc::sigaction; 2]) {
    for (expected, actual) in prior.iter().zip(actions()) {
        assert_eq!(actual.sa_sigaction, expected.sa_sigaction);
        assert_eq!(actual.sa_flags, expected.sa_flags);
        for signal in 1..=64 {
            assert_eq!(
                unsafe { libc::sigismember(&actual.sa_mask, signal) },
                unsafe { libc::sigismember(&expected.sa_mask, signal) }
            );
        }
    }
}
fn interrupted(result: anyhow::Result<()>, signal: i32) -> anyhow::Error {
    let error = result.expect_err("owned retirement must retain the real delivered signal");
    assert_eq!(
        error
            .downcast_ref::<Interruption>()
            .expect("typed Interruption was lost")
            .signal,
        signal
    );
    error
}
fn adapter() -> anyhow::Result<String> {
    let root = std::env::var("LIFECYCLE_ROOT").unwrap();
    let launcher = std::path::Path::new(&root).join("launcher");
    fs::write(&launcher, "#!/bin/sh\nwhile [ $# -gt 0 ]; do\n if [ \"$1\" = --output-last-message ]; then shift; final=$1; fi\n shift\ndone\ncat >/dev/null\nprintf 'fixture final' > \"$final\"\n").unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
    use sha2::Digest;
    println!(
        "launcher path={} sha256={:x}",
        launcher.display(),
        sha2::Sha256::digest(fs::read(&launcher).unwrap())
    );
    // Isolated worker only: no environment mutation in the outer test process.
    unsafe {
        std::env::set_var("AMPLIHACK_LAUNCHER_BINARY", launcher);
        std::env::set_var("AMPLIHACK_SESSION_DEPTH", "0");
        std::env::set_var("AMPLIHACK_RATELIMIT_MAX_RETRIES", "0");
    }
    CLISubprocessAdapter::new()
        .with_binary("codex")
        .execute_agent_step("safe fixture", None, None, None, &root, None, Some(5))
}

pub(super) fn worker(case: &str) {
    let prior = install_prior();
    let signal = if case.contains("term") {
        libc::SIGTERM
    } else {
        libc::SIGINT
    };
    let boundary = if case.starts_with("inflight_") {
        2
    } else {
        i32::from(case.contains("after_int"))
    };
    let fault = if case.contains("restore_fault") {
        libc::SIGTERM
    } else {
        0
    };
    let registration = i32::from(case == "rollback" || case == "rollback_fault");
    let fault = if case == "rollback_fault" {
        libc::SIGINT
    } else {
        fault
    };
    super::retirement_fixtures::arm(
        if case.starts_with("adapter_")
            || case.starts_with("inflight_")
            || case.starts_with("reset_")
            || case.contains("restore_fault")
        {
            signal
        } else {
            0
        },
        boundary,
        fault,
        registration,
        0,
    );
    if case.starts_with("adapter_") {
        let result = adapter();
        assert_prior(&prior);
        let error = result.expect_err("full adapter swallowed active restoration-boundary signal");
        assert_eq!(
            error
                .downcast_ref::<Interruption>()
                .expect("full adapter lost typed interruption")
                .signal,
            signal
        );
        assert!(!super::super::retryable(&error));
        let events = fs::read_to_string(std::env::var("LIFECYCLE_EVENTS").unwrap()).unwrap();
        assert!(events.contains("verified_owned_unblocked"));
        assert!(events.contains("real_raise_return value=0"));
        if boundary == 1 {
            assert!(events.contains("verified_owned_unblocked value=15 int_restored=1"));
        }
        return;
    }
    if registration != 0 {
        let error = Cancellation::install()
            .err()
            .expect("registration failure must refuse ownership");
        assert!(format!("{error:#}").contains("SIGTERM"));
        if fault != 0 {
            assert!(error.downcast_ref::<CleanupFailure>().is_some());
            // A new install must reconcile the original dispositions, not save a leaked handler.
            Cancellation::install().unwrap().close().unwrap();
        }
        assert_prior(&prior);
        return;
    }
    let owner = Cancellation::install().unwrap();
    if case.starts_with("inflight_") {
        let retired = owner.close();
        super::retirement_fixtures::release_inflight();
        let next = Cancellation::install().unwrap();
        assert!(check_cancellation().is_ok());
        next.close().unwrap();
        assert_prior(&prior);
        interrupted(retired, signal);
    } else if case.starts_with("reset_") {
        let retired = owner.close();
        let next = Cancellation::install().unwrap();
        assert!(
            check_cancellation().is_ok(),
            "fresh epoch inherited cancellation"
        );
        next.close().unwrap();
        assert_prior(&prior);
        interrupted(retired, signal);
    } else if case.starts_with("concurrent_") {
        let second = Cancellation::install().unwrap();
        assert_eq!(unsafe { libc::raise(signal) }, 0);
        let nonlast = owner.close();
        assert_ne!(
            actions()[0].sa_sigaction,
            prior[0].sa_sigaction,
            "nonlast restored global handlers"
        );
        let observed = check_cancellation();
        let last = second.close();
        assert_prior(&prior);
        interrupted(observed, signal);
        interrupted(nonlast, signal);
        interrupted(last, signal);
    } else if case.contains("restore_fault") {
        let result = finish_resources(
            Err(anyhow::anyhow!("primary ordinary failure")),
            owner.close(),
        );
        // Failure metadata must survive and reconcile on the next owner.
        Cancellation::install().unwrap().close().unwrap();
        assert_prior(&prior);
        let error = result.unwrap_err();
        assert!(
            error.downcast_ref::<Interruption>().is_some(),
            "signal lost under restoration fault: {error:#}"
        );
        assert!(error.downcast_ref::<CleanupFailure>().is_some());
        for detail in ["primary ordinary failure", "restore Codex signal"] {
            assert!(format!("{error:#}").contains(detail));
        }
    } else if case.starts_with("entered_") {
        assert_eq!(unsafe { libc::raise(signal) }, 0);
        let observed = check_cancellation();
        let retired = owner.close();
        assert_prior(&prior);
        interrupted(observed, signal);
        interrupted(retired, signal);
    } else if case == "control" {
        owner.close().unwrap();
        assert_prior(&prior);
        for sig in [libc::SIGINT, libc::SIGTERM] {
            assert_eq!(unsafe { libc::raise(sig) }, 0);
        }
        assert_eq!(PRIOR_CALLS.load(Ordering::SeqCst), 2);
        Cancellation::install().unwrap().close().unwrap();
        assert_prior(&prior);
    } else {
        panic!("unknown signal case {case}");
    }
}
macro_rules! isolated {
    ($name:ident, $case:literal) => {
        #[test]
        fn $name() {
            super::retirement_fixtures::run($case);
        }
    };
}
isolated!(adapter_sigint_at_owned_restoration, "adapter_int");
isolated!(adapter_sigterm_at_owned_restoration, "adapter_term");
isolated!(
    adapter_sigterm_after_sigint_restoration,
    "adapter_term_after_int"
);
isolated!(
    sigint_retirement_result_survives_independent_reset,
    "reset_int"
);
isolated!(
    sigterm_retirement_result_survives_independent_reset,
    "reset_term"
);
isolated!(
    sigint_concurrent_nonlast_and_last_retirement,
    "concurrent_int"
);
isolated!(
    sigterm_concurrent_nonlast_and_last_retirement,
    "concurrent_term"
);
isolated!(sigint_entered_handler_retained_on_close, "entered_int");
isolated!(sigterm_entered_handler_retained_on_close, "entered_term");
isolated!(
    sigint_restoration_fault_retains_joint_causes,
    "int_restore_fault"
);
isolated!(
    sigterm_restoration_fault_retains_joint_causes,
    "term_restore_fault"
);
isolated!(
    registration_rollback_restores_custom_dispositions,
    "rollback"
);
isolated!(
    registration_rollback_fault_preserves_reconciliation_metadata,
    "rollback_fault"
);
isolated!(
    prior_custom_dispositions_and_clean_independent_epoch,
    "control"
);

isolated!(
    sigint_inflight_publication_survives_retirement_reset,
    "inflight_int"
);
isolated!(
    sigterm_inflight_publication_survives_retirement_reset,
    "inflight_term"
);
