//! Direct-child retirement retaining genuine kill errors after successful reaping.
use anyhow::Context;
use std::{
    process::Child,
    time::{Duration, Instant},
};

/// Consume a late launcher exit without signaling or renewing the cleanup budget.
pub(super) fn observe_launcher(child: &mut Child, deadline: Instant) -> anyhow::Result<()> {
    loop {
        // Even an exhausted deadline permits one consuming nonblocking observation.
        if child
            .try_wait()
            .context("Failed to reap Codex launcher")?
            .is_some()
        {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        anyhow::ensure!(!remaining.is_zero(), "Timed out reaping Codex launcher");
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}
pub(super) fn reap_launcher(
    child: &mut Child,
    kill: impl FnOnce(&mut Child) -> std::io::Result<()>,
    timeout: Duration,
) -> anyhow::Result<()> {
    reap_until(child, kill, Instant::now() + timeout)
}
pub(super) fn reap_until(
    child: &mut Child,
    kill: impl FnOnce(&mut Child) -> std::io::Result<()>,
    deadline: Instant,
) -> anyhow::Result<()> {
    if child
        .try_wait()
        .context("Failed to inspect Codex launcher before reaping")?
        .is_some()
    {
        return Ok(());
    }
    let kill_error = kill(child).err();
    let failure = loop {
        match child.try_wait() {
            Ok(Some(_)) => break None,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => break Some("Timed out reaping Codex launcher".to_owned()),
            Err(error) => break Some(format!("Failed to reap Codex launcher: {error}")),
        }
    };
    match (kill_error, failure) {
        (None, None) => Ok(()),
        (Some(error), None) => Err(anyhow::anyhow!("Failed to kill Codex launcher: {error}")),
        (None, Some(failure)) => Err(anyhow::anyhow!(failure)),
        (Some(error), Some(failure)) => Err(anyhow::anyhow!(
            "Failed to kill Codex launcher: {error}; {failure}"
        )),
    }
}

/// Direct TERM/grace/KILL retirement shares one absolute budget.
pub(super) fn retire_launcher(child: &mut Child, deadline: Instant) -> anyhow::Result<()> {
    retire_with(
        child,
        |child| {
            let pid = i32::try_from(child.id())
                .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
            if pid <= 1 {
                return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
            }
            if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        },
        |child| child.kill(),
        deadline,
    )
}
pub(super) fn retire_with(
    child: &mut Child,
    term: impl FnOnce(&mut Child) -> std::io::Result<()>,
    kill: impl FnOnce(&mut Child) -> std::io::Result<()>,
    deadline: Instant,
) -> anyhow::Result<()> {
    super::group_anchor::validate_reaper()?;
    if child
        .try_wait()
        .context("Failed to inspect Codex launcher before retirement")?
        .is_some()
    {
        return Ok(());
    }
    let term_error = term(child)
        .err()
        .map(|error| anyhow::anyhow!("Failed to TERM Codex launcher: {error}"));
    let grace = deadline.min(Instant::now() + Duration::from_millis(100));
    let retirement = loop {
        match child.try_wait() {
            Ok(Some(_)) => break Ok(()),
            Ok(None) if Instant::now() < grace => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => break reap_until(child, kill, deadline),
            Err(error) => {
                break Err(error).context("Failed to reap Codex launcher during TERM grace");
            }
        }
    };
    super::combine_result(retirement, term_error.map_or(Ok(()), Err))
}
