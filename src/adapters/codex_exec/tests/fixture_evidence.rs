//! Fixture reads distinguish explicit optional absence from corrupt/missing evidence.
use anyhow::Context;
use std::{fs, path::Path};

fn read(path: &Path, optional: bool) -> anyhow::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if optional && error.kind() == std::io::ErrorKind::NotFound => {
            println!("fixture path={} absent=optional", path.display());
            Ok(None)
        }
        Err(error) => Err(error).with_context(|| format!("Read fixture {}", path.display())),
    }
}
fn optional(case: &str, file: &str) -> bool {
    match file {
        // These workers do not require an event-producing gate: authority/state
        // observations and kernel ENOSYS are direct, mode-zero gates emit none,
        // and the zombie-only gate may not be reached after anchored cleanup.
        "events" => matches!(
            case,
            "authority_held"
                | "authority_fd_lowered_limit"
                | "authority_unrelated"
                | "authority_reaper_ignore"
                | "authority_reaper_no_cldwait"
                | "state_eof"
                | "state_expiry"
                | "state_stolen"
                | "state_policy_change"
                | "state_drop"
                | "state_sealed"
                | "fifo_isolation"
                | "fifo_partial"
                | "fifo_parent_loss"
                | "protocol_enosys"
                | "descriptors_concurrent"
                | "anchor_unexpected_exit"
                | "group_zombie"
        ),
        // Only adapter signal workers create this script in the outer fixture.
        "launcher" => !case.starts_with("adapter_"),
        _ => false,
    }
}

pub(super) fn observe(
    root: tempfile::TempDir,
    case: &str,
    status: impl std::fmt::Display,
    preserve: bool,
) -> anyhow::Result<String> {
    let path = root.path().to_owned();
    if preserve {
        println!("preserved_fixture={}", root.keep().display());
    }
    println!("case={case} worker_exit={status}");
    let mut failures = Vec::new();
    let mut stdout = String::new();
    for file in ["stdout", "stderr", "events", "launcher"] {
        let file_path = path.join(file);
        match read(&file_path, optional(case, file)) {
            Err(error) => failures.push(format!("{error:#}")),
            Ok(None) => (),
            Ok(Some(bytes)) => {
                if preserve {
                    use sha2::Digest;
                    println!(
                        "fixture path={} bytes={} sha256={:x}",
                        file_path.display(),
                        bytes.len(),
                        sha2::Sha256::digest(&bytes)
                    );
                }
                if file != "launcher" {
                    match String::from_utf8(bytes) {
                        Ok(text) => {
                            println!("{file}:\n{text}");
                            if file == "stdout" {
                                stdout = text;
                            }
                        }
                        Err(error) => failures.push(format!(
                            "Decode UTF-8 fixture {}: {error}",
                            file_path.display()
                        )),
                    }
                }
            }
        }
    }
    anyhow::ensure!(
        failures.is_empty(),
        "{case} worker_exit={status}; fixture directory={}; {}",
        path.display(),
        failures.join("; ")
    );
    Ok(stdout)
}

#[test]
fn optional_absence_is_distinct_and_required_absence_fails() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("events");
    assert!(read(&missing, true).unwrap().is_none());
    let error = read(&missing, false).unwrap_err();
    assert!(format!("{error:#}").contains(&missing.display().to_string()));
    fs::write(&missing, "").unwrap();
    assert_eq!(read(&missing, true).unwrap().unwrap(), b"");
}
#[test]
fn invalid_utf8_preserves_diagnostics_and_worker_failure() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().to_owned();
    fs::write(path.join("stdout"), "worker failed its assertion").unwrap();
    fs::write(path.join("stderr"), "primary worker diagnostic").unwrap();
    fs::write(path.join("events"), [0xff]).unwrap();
    let error = observe(root, "required_events", "exit101", true).unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("exit101") && text.contains("Decode UTF-8") && text.contains("events"));
    assert_eq!(
        fs::read(path.join("stderr")).unwrap(),
        b"primary worker diagnostic"
    );
    fs::remove_dir_all(path).unwrap();
}
#[test]
fn directory_read_error_cannot_be_hidden_as_optional_absence() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("events")).unwrap();
    let error = read(&root.path().join("events"), true).unwrap_err();
    assert!(format!("{error:#}").contains("Read fixture"));
    assert!(format!("{error:#}").contains("events"));
}
