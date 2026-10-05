//! Owned noninteractive Codex attempts. Progress is diagnostic, never a result.
use std::{path::Path, process::Command};
mod cancellation;
mod diagnostics;
#[cfg(unix)]
mod final_output;
#[cfg(unix)]
mod process;
pub(crate) use cancellation::Interruption;
#[cfg(unix)]
pub(super) use cancellation::{Cancellation, check_cancellation};
pub(super) use diagnostics::ExitFailure;
#[cfg(unix)]
use diagnostics::{DiagnosticReader, classify_failure, drain_diagnostics, join_readers};
#[cfg(unix)]
use final_output::read_final_output;
#[cfg(unix)]
use process::OwnedProcess;
#[cfg(all(test, unix))]
use std::time::{Duration, Instant};
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
fn execute_with_readers(
    command: Command,
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
    let mut process = OwnedProcess::spawn(command, resources, cwd, environment)?;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (stdout, stderr) = readers(
        process.child.stdout.take().unwrap(),
        process.child.stderr.take().unwrap(),
        stop.clone(),
    );
    let execution = process.deliver_and_wait(envelope, timeout, &stop);
    let mut failures = Vec::new();
    process.shutdown(&mut failures);
    stop.store(true, std::sync::atomic::Ordering::Release);
    let diagnostic = join_readers(stdout, stderr, &mut failures);
    // Cancellation dominates ordinary execution/cleanup failures, retaining its type.
    let execution = combine_result(execution, check_cancellation());
    let cleanup = if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(failures.join("; cleanup: ")))
    };
    let status = combine_result(execution, cleanup)?;
    if !status.success() {
        let diagnostic = String::from_utf8_lossy(&diagnostic);
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

fn combine_result<T>(result: anyhow::Result<T>, cleanup: anyhow::Result<()>) -> anyhow::Result<T> {
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            if cleanup.downcast_ref::<Interruption>().is_some() {
                Err(cleanup.context(format!("execution: {error:#}")))
            } else {
                let detail = format!("{error:#}; cleanup: {cleanup:#}");
                Err(error.context(detail))
            }
        }
    }
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

#[cfg(all(test, unix))]
mod tests;

pub(super) fn finish_resources(
    result: anyhow::Result<String>,
    cleanup: anyhow::Result<()>,
) -> anyhow::Result<String> {
    combine_result(result, cleanup)
}
