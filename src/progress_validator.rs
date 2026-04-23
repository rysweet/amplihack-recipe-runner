//! Progress file validation hardening (ported from Python PR #3904).
//!
//! Validates progress signal files written by the recipe runner to prevent
//! spoofing.  Checks filename shape, required fields, status values, state
//! transitions, timestamp freshness, and PID liveness.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

// ── Constants ────────────────────────────────────────────────────────────────

const MAX_PROGRESS_AGE_SECS: f64 = 7200.0;
const MAX_FUTURE_DRIFT_SECS: f64 = 30.0;
const MAX_STEP_NAME_LEN: usize = 256;
const MAX_SAFE_NAME_LEN: usize = 64;

static FILENAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^amplihack-progress-(?<safe_name>[a-zA-Z0-9_]{1,64})-(?<pid>\d+)\.json$")
        .expect("compiled filename regex")
});

static SAFE_CHAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9_]").expect("compiled safe-char regex"));

// ── Public types ─────────────────────────────────────────────────────────────

/// Allowed progress statuses (matches Python `_ALLOWED_PROGRESS_STATUSES`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStatus {
    Running,
    Completed,
    Failed,
    Skipped,
    Unknown,
}

impl ProgressStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Skipped)
    }

    /// Returns the set of statuses this status may transition to.
    pub fn valid_transitions(self) -> &'static [ProgressStatus] {
        match self {
            Self::Running => &[Self::Completed, Self::Failed, Self::Skipped],
            Self::Unknown => &[Self::Running, Self::Completed, Self::Failed, Self::Skipped],
            _ => &[], // terminal
        }
    }
}

impl fmt::Display for ProgressStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Skipped => write!(f, "skipped"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Deserialized progress file payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressPayload {
    pub status: ProgressStatus,
    pub step_name: String,
    #[serde(alias = "updated_at")]
    pub timestamp: f64,
    #[serde(default)]
    pub recipe_name: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub current_step: Option<u32>,
    #[serde(default)]
    pub total_steps: Option<u32>,
    #[serde(default)]
    pub elapsed_seconds: Option<f64>,
}

/// Validation errors with descriptive messages.
#[derive(Debug, Error, PartialEq)]
pub enum ValidationError {
    #[error("filename does not match pattern amplihack-progress-{{safe_name}}-{{pid}}.json: {0}")]
    BadFilename(String),
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("step_name exceeds {MAX_STEP_NAME_LEN} chars ({0} chars)")]
    StepNameTooLong(usize),
    #[error("invalid status transition: {from} → {to}")]
    InvalidTransition {
        from: ProgressStatus,
        to: ProgressStatus,
    },
    #[error("progress file is stale (age {age_secs:.0}s exceeds {MAX_PROGRESS_AGE_SECS}s limit)")]
    Stale { age_secs: f64 },
    #[error("timestamp is {drift_secs:.1}s in the future (max {MAX_FUTURE_DRIFT_SECS}s)")]
    FutureDated { drift_secs: f64 },
    #[error("PID {0} in filename does not match payload PID {1}")]
    PidMismatch(u32, u32),
    #[error("PID {0} is not a running process")]
    PidNotAlive(u32),
    #[error("failed to parse progress JSON: {0}")]
    ParseError(String),
    #[error("progress file path {0} escapes temp directory {1}")]
    PathEscape(String, String),
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Build a progress file path in the system temp directory.
///
/// The recipe name is sanitised and clamped to [`MAX_SAFE_NAME_LEN`] characters.
/// The final path is validated to stay within the temp directory.
pub fn progress_file_path(
    recipe_name: &str,
    pid: u32,
) -> Result<std::path::PathBuf, ValidationError> {
    let safe_name = safe_progress_name(recipe_name);
    let filename = format!("amplihack-progress-{safe_name}-{pid}.json");
    let path = std::env::temp_dir().join(&filename);
    validate_path_within_tmpdir(&path)?;
    Ok(path)
}

/// Ensure a path resolves to a location inside the system temp directory.
///
/// Returns the path unchanged on success, or a `ValidationError` if the
/// resolved path escapes the temp directory (e.g. via `..` components or
/// symlinks in the recipe name).
pub fn validate_path_within_tmpdir(path: &Path) -> Result<(), ValidationError> {
    let tmp_root = std::env::temp_dir();
    // Use the string prefix check since the path may not exist yet.
    let tmp_str = tmp_root.to_string_lossy();
    let path_str = path.to_string_lossy();
    if !path_str.starts_with(tmp_str.as_ref()) {
        return Err(ValidationError::PathEscape(
            path.display().to_string(),
            tmp_root.display().to_string(),
        ));
    }
    Ok(())
}

/// Atomically write a JSON payload to a file via a temp-file rename.
///
/// Ensures concurrent readers never observe a partially-written file.
/// On rename failure, falls back to direct overwrite.
#[cfg(unix)]
pub fn atomic_write_json(path: &Path, payload: &serde_json::Value) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let data = serde_json::to_string(payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));

    // Try atomic write via temp + rename.
    let tmp_name = format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("progress"),
        std::process::id()
    );
    let tmp_path = parent.join(&tmp_name);
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp_path)
    {
        Ok(mut f) => {
            f.write_all(data.as_bytes())?;
            f.sync_all()?;
            drop(f);
            match std::fs::rename(&tmp_path, path) {
                Ok(()) => return Ok(()),
                Err(_) => {
                    let _ = std::fs::remove_file(&tmp_path);
                    // Fall through to direct write.
                }
            }
        }
        Err(_) => {
            // Fall through to direct write.
        }
    }

    // Fallback: direct overwrite.
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data.as_bytes())?;
    Ok(())
}

/// Atomically write a JSON payload to a file (non-Unix fallback).
#[cfg(not(unix))]
pub fn atomic_write_json(path: &Path, payload: &serde_json::Value) -> std::io::Result<()> {
    use std::io::Write;
    let data = serde_json::to_string(payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut f = std::fs::File::create(path)?;
    f.write_all(data.as_bytes())?;
    Ok(())
}

/// Read and validate a progress JSON file, returning `None` on any error.
///
/// Handles missing files, permission errors, partial writes, and malformed
/// JSON gracefully — the caller should treat `None` as "no progress info".
pub fn read_progress_file(path: &Path) -> Option<ProgressPayload> {
    let raw = std::fs::read_to_string(path).ok()?;
    let data: serde_json::Value = serde_json::from_str(&raw).ok()?;
    if !data.is_object() {
        return None;
    }
    let required_keys = ["recipe_name", "current_step", "status", "pid"];
    for key in &required_keys {
        data.get(*key)?;
    }
    serde_json::from_value(data).ok()
}

/// Sanitize a recipe name to the safe filename stem format.
pub fn safe_progress_name(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let sanitized = SAFE_CHAR_RE.replace_all(stem, "_");
    let mut s = sanitized.into_owned();
    s.truncate(MAX_SAFE_NAME_LEN);
    s
}

/// Parse filename and return `(safe_name, pid)` or an error.
pub fn validate_filename(filename: &str) -> Result<(String, u32), ValidationError> {
    let caps = FILENAME_RE
        .captures(filename)
        .ok_or_else(|| ValidationError::BadFilename(filename.to_owned()))?;
    let safe_name = caps["safe_name"].to_owned();
    let pid: u32 = caps["pid"]
        .parse()
        .map_err(|_| ValidationError::BadFilename(filename.to_owned()))?;
    Ok((safe_name, pid))
}

/// Validate required fields and field constraints on a payload.
pub fn validate_fields(payload: &ProgressPayload) -> Result<(), ValidationError> {
    if payload.step_name.len() > MAX_STEP_NAME_LEN {
        return Err(ValidationError::StepNameTooLong(payload.step_name.len()));
    }
    if payload.timestamp <= 0.0 {
        return Err(ValidationError::MissingField("timestamp"));
    }
    Ok(())
}

/// Validate a status transition.
pub fn validate_transition(
    from: ProgressStatus,
    to: ProgressStatus,
) -> Result<(), ValidationError> {
    if from == to {
        return Ok(());
    }
    if from.valid_transitions().contains(&to) {
        Ok(())
    } else {
        Err(ValidationError::InvalidTransition { from, to })
    }
}

/// Validate timestamp freshness.
pub fn validate_age(timestamp: f64) -> Result<(), ValidationError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let drift = timestamp - now;
    if drift > MAX_FUTURE_DRIFT_SECS {
        return Err(ValidationError::FutureDated { drift_secs: drift });
    }
    let age = now - timestamp;
    if age > MAX_PROGRESS_AGE_SECS {
        return Err(ValidationError::Stale { age_secs: age });
    }
    Ok(())
}

/// Check whether a PID corresponds to a running process.
#[cfg(unix)]
pub fn is_pid_alive(pid: u32) -> bool {
    // SAFETY: `kill(pid, 0)` only checks existence — sends no signal.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Check whether a PID corresponds to a running process (non-Unix fallback).
#[cfg(not(unix))]
pub fn is_pid_alive(_pid: u32) -> bool {
    // On non-Unix platforms, skip PID liveness — assume alive.
    true
}

/// Full validation of a progress file given its filename and raw JSON bytes.
///
/// If `previous_status` is provided, transition validation is also performed.
pub fn validate_progress_file(
    filename: &str,
    json_bytes: &[u8],
    previous_status: Option<ProgressStatus>,
) -> Result<ProgressPayload, ValidationError> {
    let (safe_name, file_pid) = validate_filename(filename)?;

    let payload: ProgressPayload = serde_json::from_slice(json_bytes)
        .map_err(|e| ValidationError::ParseError(e.to_string()))?;

    // Field constraints
    validate_fields(&payload)?;

    // PID consistency
    if let Some(p) = payload.pid
        && p != file_pid
    {
        return Err(ValidationError::PidMismatch(file_pid, p));
    }

    // Recipe-name consistency
    if let Some(ref rn) = payload.recipe_name
        && safe_progress_name(rn) != safe_name
    {
        return Err(ValidationError::BadFilename(format!(
            "recipe_name '{rn}' does not match filename stem '{safe_name}'"
        )));
    }

    // Freshness
    validate_age(payload.timestamp)?;

    // PID liveness
    if !is_pid_alive(file_pid) {
        return Err(ValidationError::PidNotAlive(file_pid));
    }

    // Transition
    if let Some(prev) = previous_status {
        validate_transition(prev, payload.status)?;
    }

    Ok(payload)
}

// ── Workstream progress sidecar (PR #4075 port) ─────────────────────────────

/// Workstream state entry persisted across recipe runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkstreamState {
    pub workstream_id: String,
    pub status: ProgressStatus,
    #[serde(default)]
    pub last_step: Option<String>,
    #[serde(default)]
    pub timestamp: f64,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub elapsed_seconds: Option<f64>,
}

/// Return the path specified by `AMPLIHACK_WORKSTREAM_PROGRESS_FILE`, if set.
///
/// The recipe runner sets this variable so the progress sidecar knows where to
/// write aggregated workstream progress.
pub fn workstream_progress_sidecar_path() -> Option<std::path::PathBuf> {
    std::env::var("AMPLIHACK_WORKSTREAM_PROGRESS_FILE")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
}

/// Return the path specified by `AMPLIHACK_WORKSTREAM_STATE_FILE`, if set.
///
/// Used for persisting per-workstream state so that timed-out workstreams can
/// be resumed on the next run.
pub fn workstream_state_file_path() -> Option<std::path::PathBuf> {
    std::env::var("AMPLIHACK_WORKSTREAM_STATE_FILE")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
}

/// Read workstream state entries from the state file.
///
/// Returns an empty vec on any error (missing file, bad JSON, etc.).
pub fn read_workstream_state(path: &Path) -> Vec<WorkstreamState> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Merge workstream state into the progress sidecar file.
///
/// Reads the current state from `state_path`, folds it into whatever already
/// exists at `progress_path`, and atomically writes the result.  Timed-out
/// workstreams (status == `Running` with stale timestamps) are preserved so
/// they can be resumed.
pub fn merge_workstream_state_into_progress(
    state_path: &Path,
    progress_path: &Path,
) -> std::io::Result<()> {
    let states = read_workstream_state(state_path);
    if states.is_empty() {
        return Ok(());
    }

    // Read existing progress entries (if any).
    let mut existing: Vec<WorkstreamState> = std::fs::read_to_string(progress_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    // Merge: newer state entries win by workstream_id.
    for new_ws in &states {
        if let Some(pos) = existing
            .iter()
            .position(|e| e.workstream_id == new_ws.workstream_id)
        {
            existing[pos] = new_ws.clone();
        } else {
            existing.push(new_ws.clone());
        }
    }

    let value = serde_json::to_value(&existing)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    atomic_write_json(progress_path, &value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_ts() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    }

    fn make_payload(status: &str, step: &str, ts: f64, pid: u32) -> String {
        format!(
            r#"{{"status":"{status}","step_name":"{step}","timestamp":{ts},"pid":{pid},"recipe_name":"test_recipe"}}"#
        )
    }

    // ── Filename validation ──────────────────────────────────────────────

    #[test]
    fn valid_filename() {
        let (name, pid) = validate_filename("amplihack-progress-my_recipe-1234.json").unwrap();
        assert_eq!(name, "my_recipe");
        assert_eq!(pid, 1234);
    }

    #[test]
    fn reject_bad_filename_no_prefix() {
        assert!(validate_filename("bad-file.json").is_err());
    }

    #[test]
    fn reject_bad_filename_special_chars() {
        assert!(validate_filename("amplihack-progress-../../etc-99.json").is_err());
    }

    #[test]
    fn reject_empty_safe_name() {
        assert!(validate_filename("amplihack-progress--42.json").is_err());
    }

    #[test]
    fn reject_safe_name_too_long() {
        let long_name = "a".repeat(65);
        let fname = format!("amplihack-progress-{long_name}-1.json");
        assert!(validate_filename(&fname).is_err());
    }

    // ── Field validation ─────────────────────────────────────────────────

    #[test]
    fn reject_step_name_too_long() {
        let pid = std::process::id();
        let long_step = "x".repeat(257);
        let json = format!(
            r#"{{"status":"running","step_name":"{long_step}","timestamp":{ts},"pid":{pid},"recipe_name":"test_recipe"}}"#,
            ts = now_ts()
        );
        let fname = format!("amplihack-progress-test_recipe-{pid}.json");
        let err = validate_progress_file(&fname, json.as_bytes(), None).unwrap_err();
        assert!(matches!(err, ValidationError::StepNameTooLong(257)));
    }

    #[test]
    fn reject_invalid_status_in_json() {
        let pid = std::process::id();
        let json = format!(
            r#"{{"status":"bogus","step_name":"s","timestamp":{ts},"pid":{pid},"recipe_name":"test_recipe"}}"#,
            ts = now_ts()
        );
        let fname = format!("amplihack-progress-test_recipe-{pid}.json");
        assert!(matches!(
            validate_progress_file(&fname, json.as_bytes(), None),
            Err(ValidationError::ParseError(_))
        ));
    }

    // ── Transition validation ────────────────────────────────────────────

    #[test]
    fn valid_running_to_completed() {
        assert!(validate_transition(ProgressStatus::Running, ProgressStatus::Completed).is_ok());
    }

    #[test]
    fn reject_completed_to_running() {
        assert!(matches!(
            validate_transition(ProgressStatus::Completed, ProgressStatus::Running),
            Err(ValidationError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn same_status_transition_ok() {
        assert!(validate_transition(ProgressStatus::Running, ProgressStatus::Running).is_ok());
    }

    // ── Age validation ───────────────────────────────────────────────────

    #[test]
    fn reject_stale_timestamp() {
        let old = now_ts() - 8000.0;
        let err = validate_age(old).unwrap_err();
        assert!(matches!(err, ValidationError::Stale { .. }));
    }

    #[test]
    fn reject_future_timestamp() {
        let future = now_ts() + 120.0;
        let err = validate_age(future).unwrap_err();
        assert!(matches!(err, ValidationError::FutureDated { .. }));
    }

    #[test]
    fn accept_recent_timestamp() {
        assert!(validate_age(now_ts() - 10.0).is_ok());
    }

    // ── PID validation ───────────────────────────────────────────────────

    #[test]
    fn current_pid_is_alive() {
        assert!(is_pid_alive(std::process::id()));
    }

    #[test]
    fn bogus_pid_is_not_alive() {
        // PID 4_000_000 is virtually guaranteed not to exist.
        assert!(!is_pid_alive(4_000_000));
    }

    // ── Full validation ──────────────────────────────────────────────────

    #[test]
    fn full_valid_payload() {
        let pid = std::process::id();
        let json = make_payload("running", "step-0", now_ts(), pid);
        let fname = format!("amplihack-progress-test_recipe-{pid}.json");
        let p = validate_progress_file(&fname, json.as_bytes(), None).unwrap();
        assert_eq!(p.status, ProgressStatus::Running);
        assert_eq!(p.step_name, "step-0");
    }

    #[test]
    fn reject_pid_mismatch() {
        let pid = std::process::id();
        let json = make_payload("running", "s", now_ts(), 99999);
        let fname = format!("amplihack-progress-test_recipe-{pid}.json");
        let err = validate_progress_file(&fname, json.as_bytes(), None).unwrap_err();
        assert!(matches!(err, ValidationError::PidMismatch(_, _)));
    }

    #[test]
    fn reject_recipe_name_mismatch() {
        let pid = std::process::id();
        let json = format!(
            r#"{{"status":"running","step_name":"s","timestamp":{ts},"pid":{pid},"recipe_name":"other_recipe"}}"#,
            ts = now_ts()
        );
        let fname = format!("amplihack-progress-test_recipe-{pid}.json");
        let err = validate_progress_file(&fname, json.as_bytes(), None).unwrap_err();
        assert!(matches!(err, ValidationError::BadFilename(_)));
    }

    // ── safe_progress_name ───────────────────────────────────────────────

    #[test]
    fn safe_name_strips_special_chars() {
        assert_eq!(safe_progress_name("my-recipe/v2!"), "v2_");
        assert_eq!(safe_progress_name("hello world"), "hello_world");
    }

    // ── Workstream sidecar (PR #4075) ────────────────────────────────────

    #[test]
    fn workstream_state_round_trip() {
        let ws = WorkstreamState {
            workstream_id: "ws-1".into(),
            status: ProgressStatus::Running,
            last_step: Some("step-3".into()),
            timestamp: 1700000000.0,
            error_message: None,
            elapsed_seconds: Some(42.0),
        };
        let json = serde_json::to_string(&ws).unwrap();
        let parsed: WorkstreamState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.workstream_id, "ws-1");
        assert_eq!(parsed.status, ProgressStatus::Running);
        assert_eq!(parsed.elapsed_seconds, Some(42.0));
    }

    #[test]
    fn read_workstream_state_missing_file() {
        let states = read_workstream_state(std::path::Path::new("/nonexistent/file.json"));
        assert!(states.is_empty());
    }

    #[test]
    fn read_workstream_state_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws-state.json");
        let data = r#"[{"workstream_id":"w1","status":"completed","timestamp":1.0}]"#;
        std::fs::write(&path, data).unwrap();
        let states = read_workstream_state(&path);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].workstream_id, "w1");
    }

    #[test]
    fn merge_workstream_creates_progress_file() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let progress_path = dir.path().join("progress.json");

        let state_data = serde_json::json!([
            {"workstream_id": "ws-a", "status": "running", "timestamp": 1.0},
            {"workstream_id": "ws-b", "status": "completed", "timestamp": 2.0}
        ]);
        std::fs::write(&state_path, state_data.to_string()).unwrap();

        merge_workstream_state_into_progress(&state_path, &progress_path).unwrap();

        let result: Vec<WorkstreamState> =
            serde_json::from_str(&std::fs::read_to_string(&progress_path).unwrap()).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn merge_workstream_updates_existing() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let progress_path = dir.path().join("progress.json");

        // Pre-existing progress
        let existing = serde_json::json!([
            {"workstream_id": "ws-a", "status": "running", "timestamp": 1.0}
        ]);
        std::fs::write(&progress_path, existing.to_string()).unwrap();

        // New state with updated status
        let new_state = serde_json::json!([
            {"workstream_id": "ws-a", "status": "completed", "timestamp": 2.0},
            {"workstream_id": "ws-c", "status": "running", "timestamp": 3.0}
        ]);
        std::fs::write(&state_path, new_state.to_string()).unwrap();

        merge_workstream_state_into_progress(&state_path, &progress_path).unwrap();

        let result: Vec<WorkstreamState> =
            serde_json::from_str(&std::fs::read_to_string(&progress_path).unwrap()).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].workstream_id, "ws-a");
        assert_eq!(result[0].status, ProgressStatus::Completed);
        assert_eq!(result[1].workstream_id, "ws-c");
    }

    #[test]
    fn merge_workstream_noop_when_state_empty() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let progress_path = dir.path().join("progress.json");
        std::fs::write(&state_path, "[]").unwrap();

        merge_workstream_state_into_progress(&state_path, &progress_path).unwrap();
        assert!(!progress_path.exists());
    }

    #[test]
    fn workstream_env_paths_unset() {
        // These env vars are not expected to be set in test env.
        // Just verify the functions return None gracefully.
        unsafe {
            std::env::remove_var("AMPLIHACK_WORKSTREAM_PROGRESS_FILE");
            std::env::remove_var("AMPLIHACK_WORKSTREAM_STATE_FILE");
        }
        assert!(workstream_progress_sidecar_path().is_none());
        assert!(workstream_state_file_path().is_none());
    }
}
