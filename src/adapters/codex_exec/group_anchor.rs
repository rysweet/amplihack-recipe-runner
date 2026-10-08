//! Non-cloneable original-group authority, held unreaped until destructive access seals.
use super::{
    anchor_child,
    anchor_io::{Pipe, acknowledge},
};
use anyhow::Context;
use std::{
    os::fd::{AsRawFd, OwnedFd},
    process::Child,
    time::{Duration, Instant},
};

pub(super) fn validate_reaper() -> anyhow::Result<()> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("Failed to inspect Codex anchor reaping policy");
    }
    anyhow::ensure!(
        action.sa_sigaction == libc::SIG_DFL && action.sa_flags & libc::SA_NOCLDWAIT == 0,
        "Codex anchor requires default SIGCHLD and exclusive PID-specific child reaping"
    );
    Ok(())
}
pub(super) fn signal_group(group: i32, signal: i32) -> anyhow::Result<()> {
    anyhow::ensure!(
        group > 1 && group != unsafe { libc::getpgrp() },
        "Invalid Codex owned group identity"
    );
    if unsafe { libc::kill(-group, signal) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error).context("Failed to signal Codex process group");
        }
    }
    Ok(())
}

pub(super) struct GroupAnchor {
    pid: i32,
    group: i32,
    control: Option<OwnedFd>,
    sealed: bool,
    retired: bool,
}
impl GroupAnchor {
    pub(super) fn establish(
        launcher: &mut Child,
        deadline: Instant,
        lifetime_ms: Option<u64>,
    ) -> anyhow::Result<Self> {
        let group = i32::try_from(launcher.id()).context("Invalid Codex launcher identity")?;
        let result = Self::start(group, deadline, lifetime_ms);
        match result {
            Ok(anchor) => Ok(anchor),
            Err(error) => Err(startup_failure(launcher, error)),
        }
    }
    fn start(group: i32, deadline: Instant, lifetime_ms: Option<u64>) -> anyhow::Result<Self> {
        validate_reaper()?;
        anyhow::ensure!(
            group > 1 && group != unsafe { libc::getpgrp() },
            "Invalid Codex anchor group"
        );
        let control = Pipe::new()?;
        let ready = Pipe::new()?;
        // All child inputs, including platform descriptor storage, are prepared here.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(std::io::Error::last_os_error()).context("Failed to fork Codex anchor");
        }
        if pid == 0 {
            unsafe {
                anchor_child::run(
                    group,
                    control.read.as_raw_fd(),
                    ready.write.as_raw_fd(),
                    lifetime_ms,
                )
            }
        }
        drop(control.read);
        drop(ready.write);
        let mut anchor = Self {
            pid,
            group,
            control: Some(control.write),
            sealed: false,
            retired: false,
        };
        let result = acknowledge(&ready.read, deadline).and_then(|_| anchor.inspect());
        if let Err(error) = result {
            // Do not reap the helper before the still-unreaped launcher is available
            // to the caller's group unwind. No launcher operation occurred here.
            anchor.sealed = true;
            let retirement = anchor.retire(Instant::now() + Duration::from_secs(2));
            return super::combine_result(Err(error), retirement);
        }
        Ok(anchor)
    }
    /// WNOWAIT inspects the owned child without releasing its group membership.
    fn state(&self) -> anyhow::Result<libc::siginfo_t> {
        validate_reaper()?;
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::waitid(
                libc::P_PID,
                self.pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("Codex anchor child authority lost");
        }
        Ok(info)
    }
    pub(super) fn inspect(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.sealed, "Codex anchor destructive access sealed");
        let info = self.state()?;
        anyhow::ensure!(
            unsafe { info.si_pid() } == 0,
            "Codex anchor exited unexpectedly before sealing"
        );
        Ok(())
    }
    pub(super) fn signal(&mut self, signal: i32, deadline: Instant) -> anyhow::Result<()> {
        anyhow::ensure!(!self.sealed, "Codex anchor destructive access sealed");
        let state = self.state();
        // An unreaped zombie still retains group identity. Report unexpected death
        // while safely cleaning descendants; ECHILD forbids uncertain group access.
        let result = match state {
            Err(error) => Err(error),
            Ok(info) => {
                let unexpected = unsafe { info.si_pid() } != 0;
                let signaled = signal_group(self.group, signal);
                if unexpected {
                    super::combine_result(
                        Err(anyhow::anyhow!(
                            "Codex anchor exited unexpectedly before sealing"
                        )),
                        signaled,
                    )
                } else {
                    signaled
                }
            }
        };
        if signal == libc::SIGKILL {
            self.sealed = true;
            let retired = self.retire(deadline);
            super::combine_result(result, retired)
        } else {
            result
        }
    }
    fn retire(&mut self, deadline: Instant) -> anyhow::Result<()> {
        if self.retired {
            return Ok(());
        }
        self.retired = true; // Drop must never renew a failed retirement budget.
        self.control.take(); // EOF also releases a live helper after a failed group KILL.
        let mut failures = Vec::new();
        let mut sent_fallback = false;
        loop {
            // Retain the helper identity until the LAST direct fallback operation.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            } != 0
            {
                failures.push(format!(
                    "Failed to inspect Codex anchor retirement: {}",
                    std::io::Error::last_os_error()
                ));
                break;
            }
            if unsafe { info.si_pid() } != 0 {
                let mut status = 0;
                let rc = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
                if rc == self.pid {
                    if !(libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGKILL
                        || libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0)
                    {
                        failures.push(format!("Codex anchor exited unexpectedly during retirement (wait status {status})"));
                    }
                    break;
                }
                if rc < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    failures.push(format!(
                        "Failed to reap Codex anchor: {}",
                        std::io::Error::last_os_error()
                    ));
                    break;
                }
            } else if !sent_fallback {
                sent_fallback = true;
                if let Err(error) = validate_reaper() {
                    failures.push(format!("Codex anchor fallback authority lost: {error:#}"));
                } else if unsafe { libc::kill(self.pid, libc::SIGKILL) } != 0 {
                    failures.push(format!(
                        "Failed to kill Codex anchor: {}",
                        std::io::Error::last_os_error()
                    ));
                }
            }
            if Instant::now() >= deadline {
                failures.push("Timed out reaping Codex anchor".into());
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        anyhow::ensure!(failures.is_empty(), "{}", failures.join("; "));
        Ok(())
    }
}
impl Drop for GroupAnchor {
    fn drop(&mut self) {
        self.sealed = true;
        if !self.retired
            && let Err(error) = self.retire(Instant::now() + Duration::from_secs(2))
        {
            log::error!("{error:#}");
        }
    }
}
fn retained_launcher(pid: i32) -> anyhow::Result<()> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .context("Codex launcher startup authority lost");
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
#[path = "tests/anchor_state.rs"]
pub(super) mod tests;

/// Startup unwind while the unreaped launcher still anchors the original group.
pub(super) fn startup_failure(launcher: &mut Child, error: anyhow::Error) -> anyhow::Error {
    let group = launcher.id() as i32;
    let mut failures = vec![format!("Codex anchor establishment: {error:#}")];
    let authority = validate_reaper().and_then(|_| retained_launcher(group));
    let held = authority.is_ok();
    if let Err(error) = authority {
        failures.push(format!("Codex anchor startup authority lost: {error:#}"));
    } else {
        for signal in [libc::SIGTERM, libc::SIGKILL] {
            if let Err(error) = signal_group(group, signal) {
                failures.push(format!("{error:#}"));
            }
        }
    }
    if let Err(error) = super::launcher_cleanup::reap_launcher(
        launcher,
        |child| {
            if !held {
                return Err(std::io::Error::other(
                    "Codex launcher startup authority unavailable",
                ));
            }
            validate_reaper().map_err(|error| std::io::Error::other(format!("{error:#}")))?;
            child.kill()
        },
        Duration::from_secs(2),
    ) {
        failures.push(format!("{error:#}"));
    }
    anyhow::anyhow!(failures.join("; "))
        .context(super::CleanupFailure("Codex anchor startup failed".into()))
}
