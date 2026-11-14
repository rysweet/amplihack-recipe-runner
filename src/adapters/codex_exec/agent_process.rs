//! Wrapper-to-backend retirement for file-backed agent transports.
use super::{group_anchor::GroupAnchor, group_cleanup::shutdown_bounded, process::group_live};
use crate::adapters::prompt_transport::PromptTransport;
use anyhow::Context;
use std::{
    os::unix::process::CommandExt,
    process::{Child, Command},
    time::{Duration, Instant},
};

/// The anchor retains authority over this invocation's dedicated process group,
/// even after a wrapper exits and its backend becomes orphaned.
pub(in crate::adapters) struct AgentProcess {
    pub(in crate::adapters) child: Child,
    anchor: GroupAnchor,
    closed: bool,
}

impl AgentProcess {
    pub(in crate::adapters) fn spawn(
        command: &mut Command,
        transport: &mut PromptTransport,
    ) -> anyhow::Result<Self> {
        super::group_anchor::validate_reaper()?;
        command.process_group(0);
        let mut child = command.spawn().context("Failed to spawn agent wrapper")?;
        transport.consumer_started();
        let anchor = GroupAnchor::establish(
            &mut child,
            Some(Instant::now() + Duration::from_secs(2)),
            None,
        )?;
        Ok(Self {
            child,
            anchor,
            closed: false,
        })
    }

    /// Request the wrapper's supported SIGTERM shutdown first. It gets a bounded
    /// grace period to terminate/reap its backend. Then sweep the owned group,
    /// escalating to SIGKILL and confirming no backend remains live before return.
    pub(in crate::adapters) fn retire(&mut self) -> anyhow::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.child.stdin.take();
        let mut failures = Vec::new();
        if let Err(error) = self.request_wrapper_shutdown() {
            failures.push(format!("{error:#}"));
        }
        let group = self.child.id() as i32;
        shutdown_bounded(
            |signal, deadline| self.anchor.signal(signal, deadline),
            || group_live(group),
            |deadline| super::launcher_cleanup::observe_launcher(&mut self.child, deadline),
            &mut failures,
        );
        anyhow::ensure!(
            failures.is_empty(),
            "Agent shutdown failed: {}",
            failures.join("; ")
        );
        Ok(())
    }

    fn request_wrapper_shutdown(&mut self) -> anyhow::Result<()> {
        if self
            .child
            .try_wait()
            .context("Failed to inspect agent wrapper")?
            .is_some()
        {
            return Ok(());
        }
        // The unreaped direct child owns this PID. Never signal the parent's group.
        if unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error).context("Failed to request agent wrapper shutdown");
            }
        }
        // ManagedChild's supported shutdown allows its backend three seconds.
        // Leave room for that escalation and the wrapper's own finalization.
        let grace = Instant::now() + Duration::from_secs(4);
        while self
            .child
            .try_wait()
            .context("Failed to observe agent wrapper shutdown")?
            .is_none()
            && Instant::now() < grace
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        if let Err(error) = self.retire() {
            log::error!("{error:#}");
        }
    }
}
