//! Persistent observable handler-entry publications; independent epochs never reset them.
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(not(target_has_atomic = "ptr"))]
compile_error!("Codex signal ownership requires native pointer-width atomics");
static INTERRUPT: AtomicUsize = AtomicUsize::new(0);
static TERMINATE: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
pub(super) struct Snapshot([usize; 2]);
impl Snapshot {
    pub(super) fn capture() -> Self {
        Self([
            INTERRUPT.load(Ordering::SeqCst),
            TERMINATE.load(Ordering::SeqCst),
        ])
    }
    pub(super) fn healthy(self) -> anyhow::Result<()> {
        if self.0.contains(&usize::MAX) {
            return Err(
                anyhow::anyhow!("Codex signal observation saturated").context(
                    super::CleanupFailure("Permanent Codex signal observation fault".into()),
                ),
            );
        }
        Ok(())
    }
    pub(super) fn since(self, baseline: Self) -> anyhow::Result<()> {
        let detail = self.healthy();
        let mut result = Ok(());
        // Both identities survive in the diagnostic chain; INT has precedence.
        for (index, signal) in [(1, libc::SIGTERM), (0, libc::SIGINT)] {
            if self.0[index] != baseline.0[index] {
                let error = super::Interruption { signal };
                result = match result {
                    Ok(()) => Err(anyhow::Error::new(error)),
                    Err(previous) => Err(previous.context(error)),
                };
            }
        }
        super::combine_result(result, detail)
    }
}

pub(super) extern "C" fn publish(signal: libc::c_int) {
    // Native atomics require no runtime locks. No later handler write depends on an epoch.
    let counter = if signal == libc::SIGINT {
        &INTERRUPT
    } else {
        &TERMINATE
    };
    let _ = counter.try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
        Some(value.saturating_add(1))
    });
    #[cfg(all(test, target_os = "linux"))]
    super::tests::retirement_fixtures::observed_entry(signal);
}

#[cfg(all(test, target_os = "linux"))]
#[path = "tests/observation_faults.rs"]
pub(super) mod tests;
