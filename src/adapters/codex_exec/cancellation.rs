//! Reference-counted scoped ownership and immutable, locked retirement observations.
#[cfg(unix)]
use super::{
    finish_resources,
    signal_observations::{Snapshot, publish},
};
#[cfg(unix)]
use anyhow::Context;

#[cfg(unix)]
struct Epoch {
    owners: usize,
    baseline: Snapshot,
    reported: Snapshot,
    saved: [libc::sigaction; 2],
    pending: [bool; 2],
}
#[cfg(unix)]
static SIGNALS: std::sync::Mutex<Option<Epoch>> = std::sync::Mutex::new(None);

#[cfg(unix)]
impl Epoch {
    fn restore(&mut self) -> anyhow::Result<()> {
        let mut errors = Vec::new();
        for (index, signal) in [libc::SIGINT, libc::SIGTERM].into_iter().enumerate() {
            if self.pending[index] {
                if unsafe { libc::sigaction(signal, &self.saved[index], std::ptr::null_mut()) } == 0
                {
                    self.pending[index] = false;
                } else {
                    errors.push(format!(
                        "Failed to restore Codex signal {signal}: {}",
                        std::io::Error::last_os_error()
                    ));
                }
            }
        }
        anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
        Ok(())
    }
    fn observed(&self) -> anyhow::Result<()> {
        Snapshot::capture().since(self.baseline)
    }
}

#[cfg(unix)]
pub(in crate::adapters) struct Cancellation {
    active: bool,
}
#[cfg(unix)]
impl Cancellation {
    pub(in crate::adapters) fn install() -> anyhow::Result<Self> {
        let mut state = SIGNALS.lock().map_err(|_| {
            anyhow::anyhow!("Codex signal ownership mutex poisoned").context(super::CleanupFailure(
                "Codex signal ownership poisoned".into(),
            ))
        })?;
        if let Some(epoch) = state.as_mut() {
            if epoch.owners != 0 {
                Snapshot::capture().healthy()?;
                epoch.owners = epoch.owners.checked_add(1).context(super::CleanupFailure(
                    "Codex signal owner count overflow".into(),
                ))?;
                return Ok(Self { active: true });
            }
            // Retain actions after a failed rollback/retirement. Reconcile once, without
            // saving leaked product handlers as the next epoch's original dispositions.
            let restored = epoch.restore();
            let snapshot = Snapshot::capture();
            let observed = snapshot.since(epoch.reported);
            epoch.reported = snapshot;
            if restored.is_err() {
                return super::combine_result(observed, restored).map(|_| Self { active: false });
            }
            // A prior close already returned its immutable observations. They must not
            // contaminate the independent epoch once all dispositions are reconciled.
            *state = None;
            observed?;
        }
        let baseline = Snapshot::capture();
        baseline.healthy()?;
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = publish as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            let mut epoch = Epoch {
                owners: 0,
                baseline,
                reported: baseline,
                saved: [std::mem::zeroed(), std::mem::zeroed()],
                pending: [false, false],
            };
            for (index, signal) in [libc::SIGINT, libc::SIGTERM].into_iter().enumerate() {
                if libc::sigaction(signal, &action, &mut epoch.saved[index]) != 0 {
                    let label = if index == 0 { "SIGINT" } else { "SIGTERM" };
                    let registration = Err(std::io::Error::last_os_error())
                        .context(format!("Failed to handle Codex {label}"));
                    let rollback = epoch.restore();
                    let snapshot = Snapshot::capture();
                    let observations = snapshot.since(epoch.baseline);
                    epoch.reported = snapshot;
                    if epoch.pending.contains(&true) {
                        *state = Some(epoch);
                    }
                    return finish_resources(
                        super::combine_result(registration, observations),
                        rollback,
                    )
                    .map(|_| Self { active: false });
                }
                epoch.pending[index] = true;
            }
            epoch.owners = 1;
            *state = Some(epoch);
        }
        Ok(Self { active: true })
    }
    pub(in crate::adapters) fn close(mut self) -> anyhow::Result<()> {
        self.release()
    }
    fn release(&mut self) -> anyhow::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let mut state = SIGNALS.lock().map_err(|_| {
            anyhow::anyhow!("Codex signal ownership mutex poisoned").context(super::CleanupFailure(
                "Codex signal ownership poisoned".into(),
            ))
        })?;
        let epoch = state.as_mut().context(super::CleanupFailure(
            "Codex signal ownership missing during retirement".into(),
        ))?;
        epoch.owners = epoch.owners.checked_sub(1).context(super::CleanupFailure(
            "Codex signal owner count underflow".into(),
        ))?;
        let restoration = if epoch.owners == 0 {
            epoch.restore()
        } else {
            Ok(())
        };
        // Snapshot and typed composition happen BEFORE unlocking or independent admission.
        let snapshot = Snapshot::capture();
        let result = super::combine_result(snapshot.since(epoch.baseline), restoration);
        if epoch.owners == 0 {
            epoch.reported = snapshot;
        }
        if epoch.owners == 0 && !epoch.pending.contains(&true) {
            *state = None;
        }
        result
    }
}
#[cfg(unix)]
impl Drop for Cancellation {
    fn drop(&mut self) {
        if let Err(error) = self.release() {
            log::error!("{error:#}");
        }
    }
}
#[cfg(unix)]
pub(in crate::adapters) fn check_cancellation() -> anyhow::Result<()> {
    let state = SIGNALS.lock().map_err(|_| {
        anyhow::anyhow!("Codex signal ownership mutex poisoned").context(super::CleanupFailure(
            "Codex signal ownership poisoned".into(),
        ))
    })?;
    match state.as_ref() {
        Some(epoch) => epoch.observed(),
        None => Snapshot::capture().healthy(),
    }
}

/// Terminal user interruption, retained through contextual cleanup errors.
#[derive(Debug, thiserror::Error)]
#[error("Codex exec cancelled by signal {signal}")]
pub(crate) struct Interruption {
    pub(crate) signal: i32,
}
