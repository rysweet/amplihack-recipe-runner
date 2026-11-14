//! Wrapper-to-backend retirement for file-backed agent transports.
use super::{group_anchor::GroupAnchor, group_cleanup::shutdown_bounded, process::group_live};
use crate::adapters::prompt_transport::PromptTransport;
use anyhow::Context;
use std::{
    os::unix::process::CommandExt,
    process::{Child, Command},
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
        let anchor = match GroupAnchor::establish_with_retirement(&mut child, None, None) {
            Ok(anchor) => anchor,
            Err(failure) => {
                if failure.retirement_confirmed {
                    transport.consumer_retired();
                }
                return Err(failure.error);
            }
        };
        Ok(Self {
            child,
            anchor,
            closed: false,
        })
    }

    /// Use the existing group TERM/grace/KILL policy and confirm that no backend
    /// remains live before returning. Prompt ownership lasts through retirement.
    pub(in crate::adapters) fn retire(&mut self) -> anyhow::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.child.stdin.take();
        let mut failures = Vec::new();
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
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        if let Err(error) = self.retire() {
            log::error!("{error:#}");
        }
    }
}
