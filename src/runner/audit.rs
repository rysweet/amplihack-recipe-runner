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
/// A recipe name reduced to something usable as a single filename component.
///
/// A recipe `name:` is free text and is not length-bounded anywhere. Used raw,
/// a name over ~255 bytes exceeds NAME_MAX and `File::create` fails with
/// ENAMETOOLONG -- and because `open_audit_log` only warns and returns `None`,
/// the run then reports SUCCESS while silently writing no audit log at all.
/// Someone who passed `--audit-dir` gets nothing back and no error.
///
/// `RecipeLogListener::new` in listeners.rs already reduces the name exactly
/// this way for its own temp file; this is the same treatment, applied to the
/// consumer that was missed.
fn safe_file_stem(recipe_name: &str) -> String {
    recipe_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// be created. On Unix, the file is created with mode `0600` (owner-only).
pub fn open_audit_log(audit_dir: &Path, recipe_name: &str) -> Option<std::fs::File> {
    log::debug!(
        "open_audit_log: audit_dir={:?}, recipe_name={:?}",
        audit_dir,
        recipe_name
    );
    if let Err(e) = std::fs::create_dir_all(audit_dir) {
        warn!("Failed to create audit log directory: {}", e);
        return None;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let path = audit_dir.join(format!("{}-{}.jsonl", safe_file_stem(recipe_name), ts));
    match std::fs::File::create(&path) {
        Ok(f) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Err(e) = f.set_permissions(std::fs::Permissions::from_mode(0o600)) {
                    warn!("Failed to set audit log permissions to 0600: {}", e);
                }
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
    log::debug!(
        "write_audit_entry: step_id={:?}, status={:?}",
        result.step_id,
        result.status
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A recipe `name:` is free text with no length bound anywhere. Used raw as
    /// a filename component it exceeds NAME_MAX, `File::create` fails with
    /// ENAMETOOLONG, and `open_audit_log` only warns and returns `None` -- so
    /// the run reports SUCCESS while writing no audit log at all. Someone who
    /// asked for `--audit-dir` silently gets nothing.
    #[test]
    fn a_long_recipe_name_still_produces_an_audit_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let long_name = "n".repeat(300);

        let file = open_audit_log(dir.path(), &long_name);

        assert!(
            file.is_some(),
            "a 300-character recipe name must not silently disable the audit log"
        );
        let written = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(Result::ok)
            .count();
        assert_eq!(written, 1, "exactly one audit log should have been created");
    }

    #[test]
    fn every_filename_component_stays_within_name_max() {
        // NAME_MAX is 255 on Linux; the reduced stem is capped well under it so
        // the timestamp suffix cannot push the whole component over.
        let stem = safe_file_stem(&"x".repeat(10_000));
        assert!(
            stem.len() <= 64,
            "stem must be bounded, got {} chars",
            stem.len()
        );
    }

    #[test]
    fn a_short_name_is_left_recognisable() {
        // The bound must not mangle ordinary names -- the audit file still has
        // to be findable by the recipe it belongs to.
        assert_eq!(safe_file_stem("smart-orchestrator"), "smart-orchestrator");
        assert_eq!(safe_file_stem("build_thing-2"), "build_thing-2");
    }

    #[test]
    fn path_separators_cannot_escape_the_audit_directory() {
        // A name is free text, so it can contain '/' or "..". Those must not
        // become path structure.
        let stem = safe_file_stem("../../etc/passwd");
        assert!(!stem.contains('/'), "got {stem:?}");
        assert!(!stem.contains(".."), "got {stem:?}");
    }
}
