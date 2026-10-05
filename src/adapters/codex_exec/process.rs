//! Owned launcher/group lifecycle and bounded stdin delivery.
use super::check_cancellation;
use anyhow::Context;
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(super) struct OwnedProcess {
    pub(super) child: std::process::Child,
}
impl OwnedProcess {
    pub(super) fn spawn(
        mut command: Command,
        resources: &Path,
        cwd: &Path,
        environment: &[(String, String)],
    ) -> anyhow::Result<Self> {
        use std::os::unix::{fs::PermissionsExt, process::CommandExt};
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
        check_cancellation()?;
        let child = command
            .spawn()
            .context("Failed to spawn Codex exec launcher")?;
        Ok(Self { child })
    }
    pub(super) fn deliver_and_wait(
        &mut self,
        envelope: &str,
        timeout: Option<u64>,
        stop: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<std::process::ExitStatus> {
        use std::{io::Write, os::fd::AsRawFd};
        let started = Instant::now();
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
            if stop.load(std::sync::atomic::Ordering::Acquire) {
                anyhow::bail!("Codex diagnostic reader failed");
            }
            if timeout.is_some_and(|seconds| started.elapsed() >= Duration::from_secs(seconds)) {
                anyhow::bail!(
                    "Codex exec timed out while delivering stdin or waiting for completion"
                );
            }
            if let Some(status) = self
                .child
                .try_wait()
                .context("Failed to wait for Codex exec")?
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
        let group = self.child.id() as i32;
        shutdown(
            |value| {
                if unsafe { libc::kill(-group, value) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error).context("Failed to signal Codex process group");
                    }
                }
                Ok(())
            },
            || group_live(group),
            || {
                reap_launcher(
                    &mut self.child,
                    |child| child.kill(),
                    Duration::from_secs(2),
                )
            },
            failures,
        );
    }
}
/// Reap the direct launcher without an unbounded wait after a failed signal.
#[cfg(unix)]
pub(super) fn reap_launcher(
    child: &mut std::process::Child,
    kill: impl FnOnce(&mut std::process::Child) -> std::io::Result<()>,
    timeout: Duration,
) -> anyhow::Result<()> {
    if child
        .try_wait()
        .context("Failed to inspect Codex launcher before reaping")?
        .is_some()
    {
        return Ok(());
    }
    let kill_error = kill(child).err();
    let deadline = Instant::now() + timeout;
    loop {
        let failure = match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Ok(None) => "Timed out reaping Codex launcher".to_owned(),
            Err(error) => format!("Failed to reap Codex launcher: {error}"),
        };
        return Err(match kill_error {
            Some(error) => anyhow::anyhow!("Failed to kill Codex launcher: {error}; {failure}"),
            None => anyhow::anyhow!(failure),
        });
    }
}

#[cfg(unix)]
pub(super) fn shutdown(
    mut signal: impl FnMut(i32) -> anyhow::Result<()>,
    mut live: impl FnMut() -> anyhow::Result<bool>,
    mut reap: impl FnMut() -> anyhow::Result<()>,
    failures: &mut Vec<String>,
) {
    if let Err(error) = signal(libc::SIGTERM) {
        failures.push(format!("{error:#}"));
    }
    let grace = Instant::now() + Duration::from_millis(100);
    loop {
        match live() {
            Ok(false) => break,
            Ok(true) if Instant::now() < grace => std::thread::sleep(Duration::from_millis(5)),
            Ok(true) => break,
            Err(error) => {
                failures.push(format!("Failed to observe Codex process group: {error:#}"));
                break;
            }
        }
    }
    // Always attempt KILL even after a TERM or observation failure.
    if let Err(error) = signal(libc::SIGKILL) {
        failures.push(format!("{error:#}"));
    }
    // A failed group signal must not leave a live launcher blocking wait forever.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match live() {
            Ok(false) => break,
            Ok(true) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(true) => {
                failures.push("Codex process group termination was not confirmed".into());
                break;
            }
            Err(error) => {
                failures.push(format!("Failed to confirm Codex termination: {error:#}"));
                break;
            }
        }
    }
    if let Err(error) = reap() {
        failures.push(format!("{error:#}"));
    }
    match live() {
        Ok(false) => {}
        Ok(true) => failures.push("Codex process group remains live after launcher reaping".into()),
        Err(error) => failures.push(format!(
            "Failed to confirm Codex termination after reaping: {error:#}"
        )),
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
