//! Real publications for joint-signal identities and permanent saturation refusal.
use super::{INTERRUPT, TERMINATE};
use crate::adapters::codex_exec::{
    CleanupFailure, Interruption, cancellation::Cancellation, check_cancellation,
};
use std::sync::atomic::Ordering;

pub(in crate::adapters::codex_exec) fn worker(case: &str) {
    crate::adapters::codex_exec::tests::retirement_fixtures::arm(0, 0, 0, 0, 0);
    let signal = if case.ends_with("term") {
        libc::SIGTERM
    } else {
        libc::SIGINT
    };
    if case.starts_with("observation_saturation") {
        // Test-only initial capacity, in a fresh subprocess; publication is real raise.
        let counter = if signal == libc::SIGINT {
            &INTERRUPT
        } else {
            &TERMINATE
        };
        counter.store(usize::MAX - 1, Ordering::SeqCst);
        let owner = Cancellation::install().unwrap();
        assert_eq!(unsafe { libc::raise(signal) }, 0);
        let error = owner.close().unwrap_err();
        assert_eq!(error.downcast_ref::<Interruption>().unwrap().signal, signal);
        assert!(error.downcast_ref::<CleanupFailure>().is_some());
        let refused = Cancellation::install()
            .err()
            .expect("saturation was reset on independent admission");
        assert!(refused.downcast_ref::<CleanupFailure>().is_some());
        assert!(format!("{refused:#}").contains("saturated"));
        return;
    }
    let owner = Cancellation::install().unwrap();
    for signal in [libc::SIGTERM, libc::SIGINT] {
        assert_eq!(unsafe { libc::raise(signal) }, 0);
    }
    for error in [
        check_cancellation().unwrap_err(),
        owner.close().unwrap_err(),
    ] {
        assert_eq!(
            error.downcast_ref::<Interruption>().unwrap().signal,
            libc::SIGINT
        );
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("signal 2") && diagnostic.contains("signal 15"));
        assert!(!crate::adapters::codex_exec::retryable(&error));
    }
    Cancellation::install().unwrap().close().unwrap();
}
#[test]
fn sigint_saturation_permanently_refuses_new_epoch() {
    crate::adapters::codex_exec::tests::extended_fixtures::run("observation_saturation_int");
}
#[test]
fn sigterm_saturation_permanently_refuses_new_epoch() {
    crate::adapters::codex_exec::tests::extended_fixtures::run("observation_saturation_term");
}
#[test]
fn both_real_signals_retain_both_identities() {
    crate::adapters::codex_exec::tests::extended_fixtures::run("observation_both");
}
