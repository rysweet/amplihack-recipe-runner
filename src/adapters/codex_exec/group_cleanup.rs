//! Shared bounded group teardown policy; preserves the original closure seam.
use std::time::{Duration, Instant};
#[cfg(test)]
pub(super) fn shutdown(
    mut signal: impl FnMut(i32) -> anyhow::Result<()>,
    live: impl FnMut() -> anyhow::Result<bool>,
    reap: impl FnMut() -> anyhow::Result<()>,
    failures: &mut Vec<String>,
) {
    shutdown_bounded(|value, _| signal(value), live, reap, failures);
}
pub(super) fn shutdown_bounded(
    mut signal: impl FnMut(i32, Instant) -> anyhow::Result<()>,
    mut live: impl FnMut() -> anyhow::Result<bool>,
    mut reap: impl FnMut() -> anyhow::Result<()>,
    failures: &mut Vec<String>,
) {
    if let Err(error) = signal(libc::SIGTERM, Instant::now()) {
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
    let deadline = Instant::now() + Duration::from_secs(2);
    if let Err(error) = signal(libc::SIGKILL, deadline) {
        failures.push(format!("{error:#}"));
    }
    // A failed group signal must not leave a live launcher blocking wait forever.
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
