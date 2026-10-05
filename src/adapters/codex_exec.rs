//! Owned noninteractive Codex attempts. Progress is diagnostic, never a result.
use anyhow::Context;
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

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
pub(super) struct Cancellation {
    active: bool,
}

#[cfg(unix)]
impl Cancellation {
    pub(super) fn install() -> anyhow::Result<Self> {
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

    pub(super) fn close(mut self) -> anyhow::Result<()> {
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
pub(super) fn check_cancellation() -> anyhow::Result<()> {
    let signal = CANCELLED.load(std::sync::atomic::Ordering::Acquire);
    anyhow::ensure!(signal == 0, "Codex exec cancelled by signal {signal}");
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("Codex exec failed with status {status}: {classification}")]
pub(super) struct ExitFailure {
    status: std::process::ExitStatus,
    pub rate_limited: bool,
    classification: &'static str,
}

#[cfg(unix)]
pub(super) fn execute(
    command: Command,
    resources: &Path,
    cwd: &Path,
    environment: &[(String, String)],
    envelope: &str,
    timeout: Option<u64>,
) -> anyhow::Result<String> {
    execute_with_readers(
        command,
        resources,
        cwd,
        environment,
        envelope,
        timeout,
        |stdout, stderr, stop| {
            (
                drain_diagnostics(stdout, stop.clone()),
                drain_diagnostics(stderr, stop),
            )
        },
    )
}

#[cfg(unix)]
type DiagnosticReader = std::thread::JoinHandle<std::io::Result<Vec<u8>>>;

#[cfg(unix)]
fn execute_with_readers(
    mut command: Command,
    resources: &Path,
    cwd: &Path,
    environment: &[(String, String)],
    envelope: &str,
    timeout: Option<u64>,
    readers: impl FnOnce(
        std::process::ChildStdout,
        std::process::ChildStderr,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> (DiagnosticReader, DiagnosticReader),
) -> anyhow::Result<String> {
    use std::io::Write;
    use std::os::unix::{fs::PermissionsExt, io::AsRawFd, process::CommandExt};
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
    let mut child = command
        .spawn()
        .context("Failed to spawn Codex exec launcher")?;
    let group = child.id() as i32;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (stdout, stderr) = readers(
        child.stdout.take().unwrap(),
        child.stderr.take().unwrap(),
        stop.clone(),
    );
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
            check_cancellation()?;
            if stop.load(std::sync::atomic::Ordering::Acquire) {
                anyhow::bail!("Codex diagnostic reader failed");
            }
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
    let mut failures = Vec::new();
    shutdown(
        |value| signal(value).context("Failed to signal Codex process group"),
        || group_live(group),
        || reap_launcher(&mut child, |child| child.kill(), Duration::from_secs(2)),
        &mut failures,
    );
    stop.store(true, std::sync::atomic::Ordering::Release);
    let mut diagnostic = Vec::new();
    for (name, reader) in [("stdout", stdout), ("stderr", stderr)] {
        match reader.join() {
            Ok(Ok(bytes)) => {
                if name == "stderr" {
                    diagnostic = bytes;
                }
            }
            _ => failures.push(format!("Failed to drain Codex {name} diagnostics")),
        }
    }
    let status = match execution {
        Ok(status) if failures.is_empty() => status,
        result => {
            match result {
                Err(error) => failures.insert(0, format!("{error:#}")),
                Ok(status) if !status.success() => {
                    failures.insert(0, format!("Codex exec failed with status {status}"));
                }
                Ok(_) => {}
            }
            anyhow::bail!("{}", failures.join("; cleanup: "));
        }
    };
    check_cancellation()?;
    if !status.success() {
        let diagnostic = String::from_utf8_lossy(&diagnostic);
        // Diagnostics classify retries but are not echoed: they can contain secrets.
        let rate_limited = super::cli_subprocess::is_rate_limit(&diagnostic);
        return Err(ExitFailure {
            status,
            rate_limited,
            classification: classify_failure(&diagnostic, rate_limited),
        }
        .into());
    }
    read_final_output(&resources.join("final-message"))
}

// Nonblocking readers also finish when detached descendants retain a pipe.
#[cfg(unix)]
fn drain_diagnostics<R: std::io::Read + std::os::fd::AsRawFd + Send + 'static>(
    mut pipe: R,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let result = (|| {
            let fd = pipe.as_raw_fd();
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let mut stored = Vec::with_capacity(64 * 1024);
            let mut buffer = [0; 8192];
            let mut completion = None;
            let mut completion_bytes = 0;
            loop {
                if stop.load(std::sync::atomic::Ordering::Acquire) {
                    let deadline = completion
                        .get_or_insert_with(|| Instant::now() + Duration::from_millis(50));
                    // Preserve queued terminal messages even if this reader was
                    // first scheduled after shutdown. Detached writers cannot
                    // extend completion indefinitely, even with continuous data.
                    if Instant::now() >= *deadline || completion_bytes >= 1024 * 1024 {
                        return Ok(stored);
                    }
                }
                match pipe.read(&mut buffer) {
                    Ok(0) => return Ok(stored),
                    Ok(count) => {
                        if completion.is_some() {
                            completion_bytes += count;
                        }
                        let excess = (stored.len() + count).saturating_sub(64 * 1024);
                        stored.drain(..excess);
                        stored.extend_from_slice(&buffer[..count]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stop.load(std::sync::atomic::Ordering::Acquire) {
                            return Ok(stored);
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => return Err(error),
                }
            }
        })();
        if result.is_err() {
            stop.store(true, std::sync::atomic::Ordering::Release);
        }
        result
    })
}

pub(super) fn finish_resources(
    result: anyhow::Result<String>,
    cleanup: anyhow::Result<()>,
) -> anyhow::Result<String> {
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(anyhow::anyhow!("{error:#}; cleanup: {cleanup:#}")),
    }
}

/// Reap the direct launcher without an unbounded wait after a failed signal.
#[cfg(unix)]
fn reap_launcher(
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
fn shutdown(
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
fn group_live(group: i32) -> anyhow::Result<bool> {
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
fn group_live(group: i32) -> anyhow::Result<bool> {
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

/// Read only the owned regular final file, preserving UTF-8 within the runner limit.
#[cfg(unix)]
fn read_final_output(path: &Path) -> anyhow::Result<String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
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
    // The metadata bound above makes this allocation safe. The capped read and
    // post-read check still reject a file that grows after metadata inspection.
    read_final_bytes(file, metadata.len() as usize, limit)
}

#[cfg(unix)]
fn read_final_bytes(
    reader: impl std::io::Read,
    capacity: usize,
    limit: usize,
) -> anyhow::Result<String> {
    use std::io::Read;
    let mut bytes = Vec::with_capacity(capacity);
    reader
        .take(limit as u64 + 1)
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn launcher_kill_failure_has_bounded_reaping() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let started = Instant::now();
        let result = reap_launcher(
            &mut child,
            |_| Err(std::io::Error::from_raw_os_error(libc::EPERM)),
            Duration::from_millis(30),
        );
        let elapsed = started.elapsed();
        // Always clean up the fixture before asserting the injected failure.
        let still_live = child.try_wait().unwrap().is_none();
        child.kill().unwrap();
        child.wait().unwrap();
        let error = result.unwrap_err().to_string();
        assert!(still_live);
        assert!(elapsed < Duration::from_secs(1));
        assert!(error.contains("Failed to kill Codex launcher"));
        assert!(error.contains("Timed out reaping Codex launcher"));
    }

    #[test]
    fn already_exited_launcher_does_not_require_kill() {
        let mut child = Command::new("true").spawn().unwrap();
        child.wait().unwrap();
        reap_launcher(
            &mut child,
            |_| panic!("must not kill reaped child"),
            Duration::ZERO,
        )
        .unwrap();
    }

    #[test]
    fn signal_and_observation_errors_do_not_skip_cleanup() {
        let operations = RefCell::new(Vec::new());
        let mut failures = vec!["original execution failure".into()];
        shutdown(
            |signal| {
                operations.borrow_mut().push(signal);
                anyhow::bail!("injected signal failure")
            },
            || {
                operations.borrow_mut().push(0);
                anyhow::bail!("injected observation failure")
            },
            || {
                operations.borrow_mut().push(-1);
                anyhow::bail!("injected reap failure")
            },
            &mut failures,
        );
        assert_eq!(
            *operations.borrow(),
            [libc::SIGTERM, 0, libc::SIGKILL, 0, -1, 0]
        );
        assert_eq!(failures.len(), 7);
        assert!(failures.join("; ").contains("original execution failure"));
    }

    #[test]
    fn readers_cap_stored_bytes_and_report_failure() {
        use std::io::Write;
        use std::os::fd::FromRawFd;
        fn pair() -> (std::fs::File, std::fs::File) {
            let mut fds = [0; 2];
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
            unsafe {
                (
                    std::fs::File::from_raw_fd(fds[0]),
                    std::fs::File::from_raw_fd(fds[1]),
                )
            }
        }
        let (reader, mut writer) = pair();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = drain_diagnostics(reader, stop.clone());
        writer.write_all(&vec![b'x'; 1024 * 1024]).unwrap();
        writer
            .write_all(b"authentication failed: SECRET_CANARY")
            .unwrap();
        drop(writer);
        let tail = handle.join().unwrap().unwrap();
        assert_eq!(tail.len(), 64 * 1024);
        assert!(tail.ends_with(b"authentication failed: SECRET_CANARY"));
        assert!(classify_failure(&String::from_utf8_lossy(&tail), false).contains("auth"));

        struct Broken(std::fs::File);
        impl std::os::fd::AsRawFd for Broken {
            fn as_raw_fd(&self) -> i32 {
                self.0.as_raw_fd()
            }
        }
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected reader failure"))
            }
        }
        let (reader, _) = pair();
        assert!(
            drain_diagnostics(Broken(reader), stop)
                .join()
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn continuously_readable_diagnostics_observe_stop() {
        struct Continuous {
            file: std::fs::File,
            stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
            reads: usize,
        }
        impl std::os::fd::AsRawFd for Continuous {
            fn as_raw_fd(&self) -> i32 {
                self.file.as_raw_fd()
            }
        }
        impl std::io::Read for Continuous {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                assert!(self.reads <= 228, "reader ignored bounded completion");
                buffer.fill(b'x');
                if self.reads == 100 {
                    self.stop.store(true, std::sync::atomic::Ordering::Release);
                }
                Ok(buffer.len())
            }
        }
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pipe = Continuous {
            file: tempfile::tempfile().unwrap(),
            stop: stop.clone(),
            reads: 0,
        };
        assert_eq!(
            drain_diagnostics(pipe, stop).join().unwrap().unwrap().len(),
            64 * 1024
        );
    }

    #[test]
    fn delayed_stderr_reader_preserves_terminal_rate_limit() {
        let resources = tempfile::tempdir().unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "cat >/dev/null; echo 'rate limit' >&2; exit 1"]);
        let error = execute_with_readers(
            command,
            resources.path(),
            resources.path(),
            &[("PATH".into(), "/usr/bin:/bin".into())],
            "task",
            Some(5),
            |stdout, stderr, stop| {
                let stdout = drain_diagnostics(stdout, stop.clone());
                let stderr = std::thread::spawn(move || {
                    // Force the scheduling order that previously lost queued bytes.
                    while !stop.load(std::sync::atomic::Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    drain_diagnostics(stderr, stop).join().unwrap()
                });
                (stdout, stderr)
            },
        )
        .unwrap_err();
        let failure = error.downcast_ref::<ExitFailure>().unwrap();
        assert!(
            failure.rate_limited,
            "terminal diagnostic must enable retries"
        );
        assert!(failure.classification.contains("rate limit"));
    }

    #[test]
    fn reader_failure_triggers_cleanup_without_execution_timeout() {
        let resources = tempfile::tempdir().unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo $$ > launcher.pid; exec sleep 30"]);
        let started = Instant::now();
        let error = execute_with_readers(
            command,
            resources.path(),
            resources.path(),
            &[],
            "task",
            None,
            |stdout, stderr, stop| {
                let failed = stop.clone();
                let reader = std::thread::spawn(move || {
                    drop(stdout);
                    failed.store(true, std::sync::atomic::Ordering::Release);
                    Err(std::io::Error::other("injected reader error"))
                });
                let stderr = drain_diagnostics(stderr, stop);
                (reader, stderr)
            },
        )
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(format!("{error:#}").contains("diagnostic reader failed"));
        if let Ok(pid) = std::fs::read_to_string(resources.path().join("launcher.pid")) {
            assert!(!group_live(pid.trim().parse().unwrap()).unwrap());
        }
    }

    #[test]
    fn deletion_failure_preserves_execution_failure() {
        let error = finish_resources(
            Err(anyhow::anyhow!("execution failure")),
            Err(anyhow::anyhow!("resource deletion failure")),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "execution failure; cleanup: resource deletion failure"
        );
        assert!(finish_resources(Ok("output".into()), Err(anyhow::anyhow!("delete"))).is_err());
    }

    #[test]
    fn final_read_error_is_reported() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected file read failure"))
            }
        }
        let error = read_final_bytes(Broken, 0, 100).unwrap_err();
        assert!(format!("{error:#}").contains("Failed to read Codex final output"));
    }

    #[test]
    fn spawn_and_final_read_failures_are_contextual() {
        let resources = tempfile::tempdir().unwrap();
        let error = execute(
            Command::new("/nonexistent/codex-launcher"),
            resources.path(),
            resources.path(),
            &[],
            "task",
            None,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("Failed to spawn"));
        assert!(read_final_output(&resources.path().join("missing")).is_err());
    }
}
