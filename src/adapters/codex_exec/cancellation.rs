//! Reference-counted scoped terminal signal ownership.
#[cfg(unix)]
use super::finish_resources;
#[cfg(unix)]
use anyhow::Context;
// Process dispositions are shared by concurrent Codex calls. The first owner
// installs handlers; the last restores them. Handlers only set an atomic flag.
#[cfg(unix)]
static CANCELLED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
#[cfg(unix)]
static SIGNALS: std::sync::Mutex<Option<(usize, libc::sigaction, libc::sigaction)>> =
    std::sync::Mutex::new(None);

#[cfg(unix)]
extern "C" fn cancel(signal: libc::c_int) {
    CANCELLED.store(signal, std::sync::atomic::Ordering::Release);
}

#[cfg(unix)]
pub(in crate::adapters) struct Cancellation {
    active: bool,
}

#[cfg(unix)]
impl Cancellation {
    pub(in crate::adapters) fn install() -> anyhow::Result<Self> {
        let mut state = SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((owners, _, _)) = state.as_mut() {
            *owners += 1;
            return Ok(Self { active: true });
        }
        CANCELLED.store(0, std::sync::atomic::Ordering::Release);
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = cancel as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            let mut interrupt = std::mem::zeroed();
            let mut terminate = std::mem::zeroed();
            if libc::sigaction(libc::SIGINT, &action, &mut interrupt) != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("Failed to handle Codex SIGINT");
            }
            if libc::sigaction(libc::SIGTERM, &action, &mut terminate) != 0 {
                let error = std::io::Error::last_os_error();
                let registration = Err(error).context("Failed to handle Codex SIGTERM");
                let rollback =
                    if libc::sigaction(libc::SIGINT, &interrupt, std::ptr::null_mut()) == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::last_os_error())
                            .context("Failed to roll back Codex SIGINT handler")
                    };
                return finish_resources(registration, rollback).map(|_| Self { active: true });
            }
            *state = Some((1, interrupt, terminate));
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
        let mut state = SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        let mut errors = Vec::new();
        if let Some((owners, interrupt, terminate)) = state.as_mut() {
            *owners -= 1;
            if *owners == 0 {
                for (signal, action) in [(libc::SIGINT, interrupt), (libc::SIGTERM, terminate)] {
                    if unsafe { libc::sigaction(signal, action, std::ptr::null_mut()) } != 0 {
                        errors.push(format!(
                            "Failed to restore Codex signal {signal}: {}",
                            std::io::Error::last_os_error()
                        ));
                    }
                }
                *state = None;
            }
        }
        anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
        Ok(())
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
    let signal = CANCELLED.load(std::sync::atomic::Ordering::Acquire);
    if signal != 0 {
        return Err(Interruption { signal }.into());
    }
    Ok(())
}

/// Terminal user interruption, retained through contextual cleanup errors.
#[derive(Debug, thiserror::Error)]
#[error("Codex exec cancelled by signal {signal}")]
pub(crate) struct Interruption {
    pub(crate) signal: i32,
}
