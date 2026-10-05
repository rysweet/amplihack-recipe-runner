//! Bounded diagnostic tails and secret-safe failure categories.
#[cfg(unix)]
use std::time::{Duration, Instant};
#[derive(Debug, thiserror::Error)]
#[error("Codex exec failed with status {status}: {classification}")]
pub(in crate::adapters) struct ExitFailure {
    pub(super) status: std::process::ExitStatus,
    pub rate_limited: bool,
    pub(super) classification: &'static str,
}

#[cfg(unix)]
pub(super) type DiagnosticReader = std::thread::JoinHandle<std::io::Result<Vec<u8>>>;
// Nonblocking readers also finish when detached descendants retain a pipe.
#[cfg(unix)]
pub(super) fn drain_diagnostics<R: std::io::Read + std::os::fd::AsRawFd + Send + 'static>(
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

// Emit only fixed categories: arbitrary service output may contain credentials or prompts.
pub(super) fn classify_failure(diagnostic: &str, rate_limited: bool) -> &'static str {
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

#[cfg(unix)]
pub(super) fn join_readers(
    stdout: DiagnosticReader,
    stderr: DiagnosticReader,
    failures: &mut Vec<String>,
) -> Vec<u8> {
    let mut diagnostic = Vec::new();
    for (name, reader) in [("stdout", stdout), ("stderr", stderr)] {
        match reader.join() {
            Ok(Ok(bytes)) => {
                if name == "stderr" {
                    diagnostic = bytes;
                }
            }
            Ok(Err(error)) => {
                failures.push(format!("Failed to drain Codex {name} diagnostics: {error}"))
            }
            Err(_) => failures.push(format!(
                "Failed to drain Codex {name} diagnostics: reader panicked"
            )),
        }
    }
    diagnostic
}
