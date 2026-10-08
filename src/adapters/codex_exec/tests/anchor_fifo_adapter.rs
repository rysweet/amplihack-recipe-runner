//! Portable endpoints retain terminal causes through the full controlled adapter.
use super::super::anchor_fifo::tests::masks::run;
use super::super::{
    CleanupFailure, Interruption, anchor_io::TEST_FIFO, cancellation::Cancellation,
};
use crate::adapters::{Adapter, cli_subprocess::CLISubprocessAdapter};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicUsize, Ordering},
};

static PRIOR_CALLS: AtomicUsize = AtomicUsize::new(0);
extern "C" fn prior(_: i32) {
    PRIOR_CALLS.fetch_add(1, Ordering::SeqCst);
}
fn actions() -> [libc::sigaction; 2] {
    [libc::SIGINT, libc::SIGTERM].map(|signal| unsafe {
        let mut action = std::mem::zeroed();
        assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut action), 0);
        action
    })
}

#[test]
fn adapter_worker() {
    if std::env::var("FIFO_MASK_CHILD").as_deref() != Ok("1") {
        return;
    }
    TEST_FIFO.set(true);
    for signal in [libc::SIGINT, libc::SIGTERM] {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = prior as *const () as usize;
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
    let saved = actions();
    let case = std::env::var("LIFECYCLE_CASE").unwrap();
    let signal = if case.contains("term") {
        libc::SIGTERM
    } else {
        libc::SIGINT
    };
    let combined = case.contains("combined");
    let root = std::path::PathBuf::from(std::env::var_os("LIFECYCLE_ROOT").unwrap());
    let launcher = root.join("launcher");
    fs::write(&launcher, "#!/bin/sh\nwhile [ $# -gt 0 ]; do\n if [ \"$1\" = --output-last-message ]; then shift; final=$1; fi\n shift\ndone\ncat >/dev/null\nprintf 'fixture final' > \"$final\"\n").unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
    use sha2::Digest;
    println!(
        "launcher path={} sha256={:x}",
        launcher.display(),
        sha2::Sha256::digest(fs::read(&launcher).unwrap())
    );
    unsafe {
        std::env::set_var("AMPLIHACK_LAUNCHER_BINARY", launcher);
        std::env::set_var("AMPLIHACK_SESSION_DEPTH", "0");
        std::env::set_var("AMPLIHACK_RATELIMIT_MAX_RETRIES", "0");
    }
    super::retirement_fixtures::arm(
        signal,
        i32::from(signal == libc::SIGTERM),
        if combined { libc::SIGTERM } else { 0 },
        0,
        0,
    );
    let result = CLISubprocessAdapter::new()
        .with_binary("codex")
        .execute_agent_step(
            "safe fixture",
            None,
            None,
            None,
            root.to_str().unwrap(),
            None,
            Some(5),
        );
    let error = result.expect_err("portable full adapter swallowed terminal causes");
    assert_eq!(
        error
            .downcast_ref::<Interruption>()
            .expect("typed Interruption lost")
            .signal,
        signal
    );
    assert!(!super::super::retryable(&error));
    if combined {
        assert!(
            error.downcast_ref::<CleanupFailure>().is_some(),
            "joint cleanup lost: {error:#}"
        );
        let diagnostic = format!("{error:#}");
        for text in ["Failed to restore Codex signal 15", "Input/output error"] {
            assert!(diagnostic.contains(text), "missing {text}: {diagnostic}");
        }
        Cancellation::install().unwrap().close().unwrap();
    } else {
        assert!(
            error.downcast_ref::<CleanupFailure>().is_none(),
            "false cleanup failure: {error:#}"
        );
    }
    for (expected, actual) in saved.iter().zip(actions()) {
        assert_eq!(expected.sa_sigaction, actual.sa_sigaction);
        assert_eq!(expected.sa_flags, actual.sa_flags);
        for sig in 1..=64 {
            assert_eq!(
                unsafe { libc::sigismember(&expected.sa_mask, sig) },
                unsafe { libc::sigismember(&actual.sa_mask, sig) }
            );
        }
    }
    assert_eq!(
        PRIOR_CALLS.load(Ordering::SeqCst),
        0,
        "owned signal used prior handler"
    );
    let events = fs::read_to_string(root.join("events")).unwrap();
    assert!(events.contains("verified_owned_unblocked"));
    assert!(events.contains("real_raise_return value=0"));
    if signal == libc::SIGTERM {
        assert!(events.contains("verified_owned_unblocked value=15 int_restored=1"));
    }
    if combined {
        assert_eq!(events.matches("restoration_fault").count(), 1);
    }
    // The signal oracle must reach the portable factory, not succeed after early refusal.
    let observations = fs::read_to_string(std::env::var_os("FIFO_CREATION_LOG").unwrap()).unwrap();
    assert!(
        observations
            .lines()
            .filter(|line| line.starts_with("fifo\t"))
            .count()
            >= 2
    );
    TEST_FIFO.set(false);
}
macro_rules! masks {
    ($int:ident, $term:ident, $joint_int:ident, $joint_term:ident, $mask:expr) => {
        #[test]
        fn $int() {
            run(
                $mask,
                "adapters::codex_exec::tests::anchor_fifo_adapter::adapter_worker",
                "adapter_int",
            );
        }
        #[test]
        fn $term() {
            run(
                $mask,
                "adapters::codex_exec::tests::anchor_fifo_adapter::adapter_worker",
                "adapter_term_after_int",
            );
        }
        #[test]
        fn $joint_int() {
            run(
                $mask,
                "adapters::codex_exec::tests::anchor_fifo_adapter::adapter_worker",
                "adapter_int_combined",
            );
        }
        #[test]
        fn $joint_term() {
            run(
                $mask,
                "adapters::codex_exec::tests::anchor_fifo_adapter::adapter_worker",
                "adapter_term_combined",
            );
        }
    };
}
masks!(
    int_000,
    term_000,
    combined_int_000,
    combined_term_000,
    0o000
);
masks!(
    int_002,
    term_002,
    combined_int_002,
    combined_term_002,
    0o002
);
masks!(
    int_022,
    term_022,
    combined_int_022,
    combined_term_022,
    0o022
);
masks!(
    int_077,
    term_077,
    combined_int_077,
    combined_term_077,
    0o077
);
