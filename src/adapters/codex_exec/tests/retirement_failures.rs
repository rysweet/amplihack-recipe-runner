//! Drop restores exactly once, logging faults while retaining reconciliation metadata.
use super::super::cancellation::Cancellation;
use std::fs;

pub(super) fn worker(case: &str) {
    super::retirement_fixtures::arm(
        if case == "fault_reconcile" {
            0
        } else {
            libc::SIGTERM
        },
        0,
        libc::SIGTERM,
        0,
        0,
    );
    for signal in [libc::SIGINT, libc::SIGTERM] {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = previous as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaddset(&mut action.sa_mask, libc::SIGUSR1);
            assert_eq!(libc::sigaction(signal, &action, std::ptr::null_mut()), 0);
        }
    }
    let original = actions();
    let owner = Cancellation::install().unwrap();
    if case == "fault_reconcile" {
        let fault = owner.close().unwrap_err();
        assert!(
            fault
                .downcast_ref::<super::super::CleanupFailure>()
                .is_some()
        );
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        let refused = Cancellation::install()
            .err()
            .expect("new publication during faulted restoration was erased");
        assert_eq!(
            refused
                .downcast_ref::<super::super::Interruption>()
                .unwrap()
                .signal,
            libc::SIGTERM
        );
        for (restored, saved) in actions().into_iter().zip(original) {
            assert_eq!(restored.sa_sigaction, saved.sa_sigaction);
            assert_eq!(restored.sa_flags, saved.sa_flags);
        }
        Cancellation::install().unwrap().close().unwrap();
        return;
    }
    drop(owner);
    let before = fs::read_to_string(std::env::var("LIFECYCLE_EVENTS").unwrap()).unwrap();
    assert_eq!(before.matches("restoration_fault").count(), 1);
    assert_eq!(before.matches("verified_owned_unblocked").count(), 1);
    // The failed disposition is still product-owned, and the complete original
    // actions must be reconciled before a new epoch saves dispositions.
    assert_ne!(actions()[1].sa_sigaction, original[1].sa_sigaction);
    Cancellation::install().unwrap().close().unwrap();
    for (restored, saved) in actions().into_iter().zip(original) {
        assert_eq!(restored.sa_sigaction, saved.sa_sigaction);
        assert_eq!(restored.sa_flags, saved.sa_flags);
        for signal in 1..=64 {
            assert_eq!(
                unsafe { libc::sigismember(&restored.sa_mask, signal) },
                unsafe { libc::sigismember(&saved.sa_mask, signal) }
            );
        }
    }
}
fn actions() -> [libc::sigaction; 2] {
    [libc::SIGINT, libc::SIGTERM].map(|signal| unsafe {
        let mut action = std::mem::zeroed();
        assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut action), 0);
        action
    })
}
#[test]
fn drop_fault_retains_complete_dispositions_for_reconciliation() {
    super::extended_fixtures::run("fault_drop");
}

extern "C" fn previous(_: i32) {}

#[test]
fn pending_restoration_publication_refuses_independent_admission() {
    super::extended_fixtures::run("fault_reconcile");
}

#[test]
fn primary_signal_and_joint_cleanup_survive_retirement_composition() {
    use super::super::{CleanupFailure, Interruption, finish_resources};
    let primary = Err(Interruption {
        signal: libc::SIGTERM,
    }
    .into());
    let retirement = Err(anyhow::Error::new(Interruption {
        signal: libc::SIGINT,
    })
    .context(CleanupFailure("restoration failure".into())));
    let error = finish_resources(primary, retirement).unwrap_err();
    assert_eq!(
        error.downcast_ref::<Interruption>().unwrap().signal,
        libc::SIGTERM
    );
    assert!(error.downcast_ref::<CleanupFailure>().is_some());
    let detail = format!("{error:#}");
    for text in ["signal 2", "signal 15", "restoration failure"] {
        assert!(detail.contains(text));
    }
}
