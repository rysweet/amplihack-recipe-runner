//! Validated, bounded final-message descriptor reads.
use anyhow::Context;
use std::path::Path;
/// Read only the owned regular final file, preserving UTF-8 within the runner limit.
#[cfg(unix)]
pub(super) fn read_final_output(path: &Path) -> anyhow::Result<String> {
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
pub(super) fn read_final_bytes(
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
