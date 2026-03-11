//! JSONL audit logging for recipe execution.
//!
//! Creates timestamped `.jsonl` files in the configured audit directory,
//! writing one JSON object per line for each completed step.

use crate::models::StepResult;
use log::warn;
use serde_json;
use std::io::Write;
use std::path::Path;

/// Open a new JSONL audit log file for the given recipe.
///
/// Returns `None` if no audit directory is configured or if the file cannot
/// be created. On Unix, the file is created with mode `0600` (owner-only).
pub fn open_audit_log(audit_dir: &Path, recipe_name: &str) -> Option<std::fs::File> {
    log::debug!("open_audit_log: audit_dir={:?}, recipe_name={:?}", audit_dir, recipe_name);
    if let Err(e) = std::fs::create_dir_all(audit_dir) {
        warn!("Failed to create audit log directory: {}", e);
        return None;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let path = audit_dir.join(format!("{}-{}.jsonl", recipe_name, ts));
    match std::fs::File::create(&path) {
        Ok(f) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
            Some(f)
        }
        Err(e) => {
            warn!("Failed to create audit log file: {}", e);
            None
        }
    }
}

/// Write a step result as a single JSONL line to the audit log.
pub fn write_audit_entry(file: &Option<std::fs::File>, result: &StepResult) {
    log::debug!("write_audit_entry: step_id={:?}, status={:?}", result.step_id, result.status);
    if let Some(mut f) = file.as_ref().and_then(|f| f.try_clone().ok()) {
        let entry = serde_json::json!({
            "step_id": result.step_id,
            "status": format!("{}", result.status),
            "duration_ms": result.duration.map(|d| d.as_millis()),
            "error": if result.error.is_empty() { None } else { Some(&result.error) },
            "output_len": result.output.len(),
        });
        if let Err(e) = writeln!(f, "{}", entry) {
            warn!("Failed to write audit log entry: {}", e);
        }
    }
}
