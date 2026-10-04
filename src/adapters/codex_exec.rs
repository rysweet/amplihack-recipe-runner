//! Owned noninteractive Codex attempts. Progress is diagnostic, never a result.
use anyhow::Context;
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error)]
#[error("Codex exec failed with status {status}: {classification}")]
pub(super) struct ExitFailure {
    status: std::process::ExitStatus,
    pub rate_limited: bool,
    classification: &'static str,
}

#[cfg(unix)]
pub(super) fn execute(
    mut command: Command,
    resources: &Path,
    cwd: &Path,
    environment: &[(String, String)],
    envelope: &str,
    timeout: Option<u64>,
) -> anyhow::Result<String> {
    use std::io::{Read, Write};
    use std::os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
        process::CommandExt,
    };
    std::fs::set_permissions(resources, std::fs::Permissions::from_mode(0o700))
        .context("Failed to secure Codex resources")?;
    let progress = resources.join("progress");
    let diagnostics = resources.join("diagnostics");
    let private_file = |path: &Path| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    };
    command
        .current_dir(cwd)
        .env_clear()
        .envs(environment.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::piped())
        .stdout(private_file(&progress)?)
        .stderr(private_file(&diagnostics)?);
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
    let mut child = command
        .spawn()
        .context("Failed to spawn Codex exec launcher")?;
    let group = child.id() as i32;
    let started = Instant::now();
    let mut stdin = child.stdin.take();
    let execution = (|| -> anyhow::Result<std::process::ExitStatus> {
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
            if timeout.is_some_and(|seconds| started.elapsed() >= Duration::from_secs(seconds)) {
                anyhow::bail!(
                    "Codex exec timed out while delivering stdin or waiting for completion"
                );
            }
            if let Some(status) = child.try_wait().context("Failed to wait for Codex exec")? {
                if offset != envelope.len() {
                    anyhow::bail!(
                        "Codex stdin delivery incomplete: {offset}/{} bytes",
                        envelope.len()
                    );
                }
                return Ok(status);
            }
            if let Some(writer) = stdin.as_mut() {
                match writer.write(&envelope.as_bytes()[offset..]) {
                    Ok(0) => anyhow::bail!("Codex stdin write returned zero before completion"),
                    Ok(count) => offset += count,
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
                log::info!(
                    "Codex exec still running ({} seconds)",
                    started.elapsed().as_secs()
                );
                heartbeat = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    drop(stdin);
    // Terminate inherited descendants even when the launcher exited successfully.
    let signal = |value| -> std::io::Result<()> {
        if unsafe { libc::kill(-group, value) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    };
    let cleanup = (|| -> anyhow::Result<()> {
        signal(libc::SIGTERM).context("Failed to terminate Codex process group")?;
        std::thread::sleep(Duration::from_millis(50));
        signal(libc::SIGKILL).context("Failed to kill Codex process group")?;
        child.wait().context("Failed to reap Codex launcher")?;
        Ok(())
    })();
    let status = match (execution, cleanup) {
        (Ok(status), Ok(())) => status,
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
        (Err(error), Err(cleanup)) => anyhow::bail!("{error:#}; cleanup: {cleanup:#}"),
    };
    if !status.success() {
        let diagnostic = super::cli_subprocess::read_capped(&diagnostics, 64 * 1024)
            .context("Failed to read Codex failure diagnostics")?;
        // Diagnostics classify retries but are not echoed: they can contain secrets.
        let rate_limited = super::cli_subprocess::is_rate_limit(&diagnostic);
        return Err(ExitFailure {
            status,
            rate_limited,
            classification: classify_failure(&diagnostic, rate_limited),
        }
        .into());
    }
    let path = resources.join("final-message");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .context("Failed to open Codex final output")?;
    let metadata = file
        .metadata()
        .context("Failed to inspect Codex final output")?;
    anyhow::ensure!(
        metadata.is_file(),
        "Codex final output must be a regular file"
    );
    anyhow::ensure!(
        metadata.mode() & 0o077 == 0 && metadata.uid() == unsafe { libc::geteuid() },
        "Codex final output has unsafe permissions or ownership"
    );
    let limit = crate::runner::MAX_STEP_OUTPUT_BYTES;
    anyhow::ensure!(
        metadata.len() <= limit as u64,
        "Codex final output exceeds {limit}-byte limit"
    );
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .context("Failed to read Codex final output")?;
    anyhow::ensure!(
        bytes.len() <= limit,
        "Codex final output exceeds {limit}-byte limit"
    );
    String::from_utf8(bytes).context("Codex final output is not valid UTF-8")
}

#[cfg(not(unix))]
pub(super) fn execute(
    _: Command,
    _: &Path,
    _: &Path,
    _: &[(String, String)],
    _: &str,
    _: Option<u64>,
) -> anyhow::Result<String> {
    anyhow::bail!("Codex exec requires Unix process-group containment on this platform")
}

// Emit only fixed categories: arbitrary service output may contain credentials or prompts.
fn classify_failure(diagnostic: &str, rate_limited: bool) -> &'static str {
    let text = diagnostic.to_ascii_lowercase();
    if rate_limited {
        "service rate limit; retry later or check account quota"
    } else if [
        "unauthorized",
        "authentication",
        "401",
        "invalid api key",
        "not logged in",
    ]
    .iter()
    .any(|marker| text.contains(marker))
    {
        "authentication rejected; check Codex login and credentials"
    } else if ["connection", "network", "dns", "502", "503"]
        .iter()
        .any(|marker| text.contains(marker))
    {
        "service connection failed; check connectivity and service availability"
    } else if ["unexpected argument", "unrecognized", "invalid value"]
        .iter()
        .any(|marker| text.contains(marker))
    {
        "CLI invocation rejected; check launcher and Codex CLI compatibility"
    } else {
        "unclassified service or launcher failure; check Codex login, configuration and service availability"
    }
}
