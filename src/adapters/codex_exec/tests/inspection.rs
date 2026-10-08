//! Fallible Linux process observations; only disappearance races are absence.
use anyhow::Context;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn members(group: i32) -> anyhow::Result<Vec<i32>> {
    members_in(Path::new("/proc"), group)
}
fn members_in(root: &Path, group: i32) -> anyhow::Result<Vec<i32>> {
    let mut members = Vec::new();
    for entry in fs::read_dir(root).context("Failed to enumerate process metadata")? {
        let entry = entry.context("Failed to read process directory entry")?;
        let name = entry.file_name();
        let name = name.to_str().context("Invalid process directory name")?;
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let pid: i32 = name.parse().context("Invalid numeric process identity")?;
        let stat = match fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) => {
                continue;
            }
            Err(error) => return Err(error).context("Failed to read process metadata"),
        };
        let tail = stat
            .rsplit_once(')')
            .context("Malformed process metadata")?
            .1;
        let pgid: i32 = tail
            .split_whitespace()
            .nth(2)
            .context("Incomplete process metadata")?
            .parse()
            .context("Invalid process group")?;
        if pgid == group {
            members.push(pid);
        }
    }
    Ok(members)
}

/// The retained helper must still exist: even ENOENT fails ownership coverage.
pub(super) fn descriptors(pid: i32) -> anyhow::Result<Vec<PathBuf>> {
    descriptors_in(&PathBuf::from(format!("/proc/{pid}/fd")))
        .with_context(|| format!("Failed to inspect retained helper {pid} descriptors"))
}
fn descriptors_in(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    fs::read_dir(root)
        .context("Failed to inspect retained helper descriptors")?
        .map(|entry| {
            let entry = entry.context("Failed to enumerate retained helper descriptor")?;
            fs::read_link(entry.path()).context("Failed to inspect retained helper descriptor")
        })
        .collect()
}

#[test]
fn process_absence_races_do_not_hide_malformed_or_unreadable_metadata() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("self")).unwrap();
    let process = root.path().join("123");
    fs::create_dir(&process).unwrap();
    assert!(
        members_in(root.path(), 42).unwrap().is_empty(),
        "ENOENT race"
    );
    for metadata in [
        "missing delimiter",
        "123 (name) S",
        "123 (name) S 1 invalid",
    ] {
        fs::write(process.join("stat"), metadata).unwrap();
        assert!(members_in(root.path(), 42).is_err(), "ignored {metadata}");
    }
    fs::remove_file(process.join("stat")).unwrap();
    fs::create_dir(process.join("stat")).unwrap();
    assert!(
        members_in(root.path(), 42).is_err(),
        "unexpected read failure"
    );
    fs::remove_dir(process.join("stat")).unwrap();
    fs::write(process.join("stat"), "123 (name with spaces) S 1 42").unwrap();
    assert_eq!(members_in(root.path(), 42).unwrap(), [123]);
}

#[test]
fn missing_retained_helper_or_descriptor_is_an_inspection_failure() {
    let root = tempfile::tempdir().unwrap();
    assert!(descriptors_in(&root.path().join("absent")).is_err());
    fs::write(root.path().join("3"), "not a descriptor link").unwrap();
    assert!(descriptors_in(root.path()).is_err());
    fs::remove_file(root.path().join("3")).unwrap();
    std::os::unix::fs::symlink("pipe:[123]", root.path().join("3")).unwrap();
    assert_eq!(
        descriptors_in(root.path()).unwrap(),
        [PathBuf::from("pipe:[123]")]
    );
}
