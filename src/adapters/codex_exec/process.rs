//! Owned launcher/group lifecycle and bounded stdin delivery.
#[cfg(test)]
pub(super) use super::group_cleanup::shutdown;
use super::group_cleanup::shutdown_bounded;
#[cfg(test)]
pub(super) use super::launcher_cleanup::reap_launcher;
use super::{
    check_cancellation,
    group_anchor::{GroupAnchor, validate_reaper},
};
use anyhow::Context;
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(super) struct OwnedProcess {
    pub(super) child: std::process::Child,
    anchor: GroupAnchor,
    started: Instant,
    deadline: Option<Instant>,
    closed: bool,
}
impl OwnedProcess {
    #[cfg(test)]
    pub(super) fn spawn(
        command: Command,
        resources: &Path,
        cwd: &Path,
        environment: &[(String, String)],
    ) -> anyhow::Result<Self> {
        Self::spawn_with_timeout(command, resources, cwd, environment, None)
    }
    pub(super) fn spawn_with_timeout(
        mut command: Command,
        resources: &Path,
        cwd: &Path,
        environment: &[(String, String)],
        timeout: Option<u64>,
    ) -> anyhow::Result<Self> {
        use std::os::unix::{fs::PermissionsExt, process::CommandExt};
        anyhow::ensure!(
            timeout != Some(0),
            "Codex exec timed out before launcher setup"
        );
        // Reject unrepresentable budgets before any launcher can create descendants.
        let lifetime = timeout
            .map(|seconds| {
                seconds
                    .checked_mul(1000)
                    .and_then(|ms| ms.checked_add(4100))
                    .context("Codex anchor lifetime overflow")
            })
            .transpose()?;
        timeout
            .map(|seconds| {
                Instant::now()
                    .checked_add(Duration::from_secs(seconds))
                    .context("Codex attempt deadline overflow")
            })
            .transpose()?;
        std::fs::set_permissions(resources, std::fs::Permissions::from_mode(0o700))
            .context("Failed to secure Codex resources")?;
        command
            .current_dir(cwd)
            .env_clear()
            .envs(environment.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Only async-signal-safe calls are made between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::umask(0o077);
                Ok(())
            });
        }
        super::combine_result(Ok(()), validate_reaper())?;
        check_cancellation()?;
        let mut child = command
            .spawn()
            .context("Failed to spawn Codex exec launcher")?;
        let started = Instant::now();
        let deadline = timeout
            .map(|seconds| {
                started
                    .checked_add(Duration::from_secs(seconds))
                    .context("Codex attempt deadline overflow")
            })
            .transpose();
        let deadline = match deadline {
            Ok(deadline) => deadline,
            Err(error) => {
                return Err(super::group_anchor::startup_failure(&mut child, error));
            }
        };
        let startup = deadline.map_or(started + Duration::from_millis(100), |d| {
            d.min(started + Duration::from_millis(100))
        });
        let anchor = GroupAnchor::establish(&mut child, startup, lifetime)?;
        Ok(Self {
            child,
            anchor,
            started,
            deadline,
            closed: false,
        })
    }
    pub(super) fn deliver_and_wait(
        &mut self,
        envelope: &str,
        timeout: Option<u64>,
        stop: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<std::process::ExitStatus> {
        use std::{io::Write, os::fd::AsRawFd};
        let started = self.started;
        if self.deadline.is_none() {
            self.deadline = timeout
                .map(|seconds| {
                    started
                        .checked_add(Duration::from_secs(seconds))
                        .context("Codex attempt deadline overflow")
                })
                .transpose()?;
        }
        let group = self.child.id() as i32;
        let mut stdin = self.child.stdin.take();
        let fd = stdin.as_ref().context("Missing Codex stdin")?.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("Failed to configure Codex stdin");
            }
        }
        let mut offset = 0;
        let mut heartbeat = Instant::now();
        loop {
            check_cancellation()?;
            // Owned inspection faults are terminal even if later shutdown succeeds.
            super::combine_result(Ok(()), self.anchor.inspect())?;
            if stop.load(std::sync::atomic::Ordering::Acquire) {
                anyhow::bail!("Codex diagnostic reader failed");
            }
            if self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                anyhow::bail!(
                    "Codex exec timed out while delivering stdin or waiting for completion"
                );
            }
            if let Some(status) = self
                .child
                .try_wait()
                .context("Failed to wait for Codex exec")
                .context(super::CleanupFailure(
                    "Codex owned launcher inspection failed".into(),
                ))?
            {
                if offset != envelope.len() {
                    anyhow::bail!(
                        "Codex stdin delivery incomplete: {offset}/{} bytes",
                        envelope.len()
                    );
                }
                return Ok(status);
            }
            let mut stdin_progress = false;
            if let Some(writer) = stdin.as_mut() {
                match writer.write(&envelope.as_bytes()[offset..]) {
                    Ok(0) => anyhow::bail!("Codex stdin write returned zero before completion"),
                    Ok(count) => {
                        offset += count;
                        stdin_progress = true;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => {
                        return Err(error).context("Failed to deliver Codex stdin instructions");
                    }
                }
                if offset == envelope.len() {
                    stdin.take();
                }
            }
            if heartbeat.elapsed() >= Duration::from_secs(30) {
                eprintln!(
                    "Codex exec still running ({} seconds, PID {group})",
                    started.elapsed().as_secs()
                );
                heartbeat = Instant::now();
            }
            // Keep delivering while the pipe accepts bytes. Back off only when
            // blocked or waiting for exit; every iteration still checks timeout.
            if !stdin_progress {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    pub(super) fn shutdown(&mut self, failures: &mut Vec<String>) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.child.stdin.take();
        if let Err(error) = super::launcher_cleanup::retire_launcher(
            &mut self.child,
            Instant::now() + Duration::from_secs(2),
        ) {
            failures.push(format!("{error:#}"));
        }
        let group = self.child.id() as i32;
        let anchor = &mut self.anchor;
        let child = &mut self.child;
        shutdown_bounded(
            |signal, deadline| anchor.signal(signal, deadline),
            || group_live(group),
            |deadline| super::launcher_cleanup::observe_launcher(child, deadline),
            failures,
        );
    }
}
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if !self.closed {
            let mut failures = Vec::new();
            self.shutdown(&mut failures);
            for failure in failures {
                log::error!("{failure}");
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn group_live(group: i32) -> anyhow::Result<bool> {
    for entry in std::fs::read_dir("/proc").context("Failed to inspect process groups")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            // A process can disappear after read_dir or after opening stat.
            // Linux reports either ENOENT or ESRCH for those races.
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ESRCH) =>
            {
                continue;
            }
            Err(error) => return Err(error).context("Failed to inspect process state"),
        };
        // comm is parenthesized and may itself contain spaces or parentheses.
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .context("Invalid process metadata")?
            .1
            .split_whitespace()
            .collect();
        anyhow::ensure!(fields.len() >= 3, "Incomplete process metadata");
        if fields[2].parse::<i32>()? == group && !matches!(fields[0], "Z" | "X") {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(super) fn group_live(group: i32) -> anyhow::Result<bool> {
    if unsafe { libc::kill(-group, 0) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(error).context("Failed to confirm process group absence")
    }
}
