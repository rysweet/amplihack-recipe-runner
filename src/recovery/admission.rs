//! Launch registration. Call once in the executing process, after authorization.
use super::{
    model::*,
    private_fs::{Directory, lock, parse},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs::File, path::Path};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Latest {
    schema_version: u32,
    run_id: String,
    sequence: u64,
}
/// The lock is held for the lifetime of this launch, including usage-limit waits.
pub struct Admission {
    pub descriptor: Descriptor,
    _owner: File,
}
impl Admission {
    pub fn begin(
        work_root: &Path,
        run_dir: &Path,
        identity: Option<&str>,
        fresh: bool,
        settings: String,
    ) -> Result<Self> {
        let root = Directory::open(work_root, true)?;
        let run = Directory::open(run_dir, false)?;
        let run_id = run_dir
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid run id"))?
            .to_owned();
        super::private_fs::component(&run_id)?;
        let recovery = run.child("recovery", true)?;
        let owner = recovery.file("owner.lock", true)?;
        lock(&owner, true)?;
        let mut descriptor = Descriptor {
            schema_version: 1,
            run_id: run_id.clone(),
            sequence: 1,
            identity: identity.unwrap_or("").into(),
            directory: recovery.path.clone(),
            source: None,
            reason: if fresh {
                "fresh_requested"
            } else {
                "identity_unavailable"
            }
            .into(),
            settings,
            fresh,
        };
        if let Some(identity) = identity {
            let index = root
                .child("recovery-index", true)?
                .child(&digest(identity.as_bytes()), true)?;
            let guard = index.file("admission.lock", true)?;
            lock(&guard, false)?;
            // Unsafe files/directories still fail admission. Only untrusted
            // historical *contents* may turn replay into a fresh decision.
            let latest_bytes = read_optional(&index, "latest.json")?;
            let latest = latest_bytes
                .as_deref()
                .and_then(|bytes| valid_latest(bytes).ok());
            let ordering = match read_optional(&index, "ordering.json")? {
                Some(bytes) => Some(valid_latest(&bytes)?),
                None => bootstrap_ordering(&root, identity, latest.clone())?,
            };
            ensure!(
                latest_bytes.is_none() || ordering.is_some(),
                "cannot establish admission ordering from historical registrations"
            );
            descriptor.sequence = ordering.as_ref().map_or(Ok(1), |previous| {
                previous
                    .sequence
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("recovery sequence exhausted"))
            })?;
            let verified_latest = latest.filter(|previous| {
                ordering.as_ref().is_some_and(|order| {
                    previous.sequence == order.sequence && previous.run_id == order.run_id
                })
            });
            descriptor.reason = if fresh {
                "fresh_requested"
            } else if latest_bytes.is_some() || ordering.is_some() {
                "state_unavailable"
            } else {
                "no_history"
            }
            .into();
            if latest_bytes.is_some() && verified_latest.is_none() {
                eprintln!(
                    "recipe recovery: fresh reason=state_unavailable (unverifiable latest marker)"
                );
            }
            if let Some(previous) = verified_latest
                && !fresh
            {
                let previous_dir = root
                    .child("runs", false)
                    .and_then(|d| d.child(&previous.run_id, false))
                    .and_then(|d| d.child("recovery", false));
                descriptor.reason = "state_unavailable".into();
                if let Ok(previous_dir) = previous_dir {
                    let status = (|| -> Result<Tree> {
                        let owner = previous_dir.file("owner.lock", false)?;
                        lock(&owner, true)?;
                        let writer = previous_dir.file("writer.lock", false)?;
                        lock(&writer, true)?;
                        let tree: Tree = parse(&previous_dir.read("tree.json")?)?;
                        super::store::validate_tree(&tree)?;
                        ensure!(
                            tree.run_id == previous.run_id
                                && tree.sequence == previous.sequence
                                && tree.identity == identity,
                            "recovery source identity mismatch"
                        );
                        Ok(tree)
                    })();
                    if status
                        .as_ref()
                        .err()
                        .and_then(|e| e.downcast_ref::<std::io::Error>())
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock)
                    {
                        descriptor.reason = "owner_active".into();
                    }
                    if let Ok(tree) = status {
                        if tree.outcome == Outcome::UsageLimit {
                            descriptor.source = Some(Source {
                                run_id: previous.run_id.clone(),
                                sequence: previous.sequence,
                                directory: previous_dir.path.clone(),
                            });
                            descriptor.reason = "usage_limit".into();
                        } else {
                            descriptor.reason = "latest_not_usage_limit".into();
                        }
                    }
                }
            }
            // Register a complete unknown/running attempt before publishing latest.
            let tree = super::store::empty_tree(&descriptor);
            recovery.publish("tree.json", &super::private_fs::serialize(&tree)?)?;
            recovery.publish(
                "descriptor.json",
                &super::private_fs::serialize(&descriptor)?,
            )?;
            let marker = super::private_fs::serialize(&Latest {
                schema_version: 1,
                run_id,
                sequence: descriptor.sequence,
            })?;
            // The high-water registration is durable before latest publication.
            // A crash between them leaves a barrier, never an older replay source.
            index.publish("ordering.json", &marker)?;
            index.publish("latest.json", &marker)?;
        } else {
            recovery.publish(
                "tree.json",
                &super::private_fs::serialize(&super::store::empty_tree(&descriptor))?,
            )?;
            recovery.publish(
                "descriptor.json",
                &super::private_fs::serialize(&descriptor)?,
            )?;
        }
        Ok(Self {
            descriptor,
            _owner: owner,
        })
    }
    /// Finalize through the host's trusted classifier after the runner writer
    /// has exited. This never races a live writer or changes an older attempt.
    pub fn finish(&self, outcome: Outcome) -> Result<()> {
        let directory = Directory::open(&self.descriptor.directory, false)?;
        let mut tree: Tree = parse(&directory.read("tree.json")?)?;
        super::store::validate_tree(&tree)?;
        ensure!(
            tree.run_id == self.descriptor.run_id
                && tree.sequence == self.descriptor.sequence
                && tree.identity == self.descriptor.identity,
            "current recovery identity mismatch"
        );
        tree.outcome = outcome;
        tree.revision = tree
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("recovery revision exhausted"))?;
        directory.publish("tree.json", &super::private_fs::serialize(&tree)?)
    }
    pub fn descriptor_path(&self) -> std::path::PathBuf {
        self.descriptor.directory.join("descriptor.json")
    }
}

fn read_optional(directory: &Directory, name: &str) -> Result<Option<Vec<u8>>> {
    match directory.read(name) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
fn valid_latest(bytes: &[u8]) -> Result<Latest> {
    let latest: Latest = parse(bytes)?;
    ensure!(
        latest.schema_version == 1 && latest.sequence > 0,
        "unsupported admission ordering"
    );
    super::private_fs::component(&latest.run_id)?;
    Ok(latest)
}
/// One-time migration of pre-ordering-file history. Registrations supply order,
/// never replay eligibility: only the verified latest marker can select a source.
fn bootstrap_ordering(
    root: &Directory,
    identity: &str,
    mut newest: Option<Latest>,
) -> Result<Option<Latest>> {
    let runs = match root.child("runs", false) {
        Ok(runs) => runs,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(newest);
        }
        Err(error) => return Err(error),
    };
    for entry in std::fs::read_dir(&runs.path)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let registration = (|| -> Result<Descriptor> {
            let recovery = runs.child(&name, false)?.child("recovery", false)?;
            let descriptor: Descriptor = parse(&recovery.read("descriptor.json")?)?;
            ensure!(
                descriptor.schema_version == 1
                    && descriptor.run_id == name
                    && descriptor.sequence > 0,
                "unverifiable admission registration"
            );
            Ok(descriptor)
        })();
        if let Ok(registration) = registration
            && registration.identity == identity
            && newest
                .as_ref()
                .is_none_or(|n| registration.sequence > n.sequence)
        {
            newest = Some(Latest {
                schema_version: 1,
                run_id: name,
                sequence: registration.sequence,
            });
        }
    }
    Ok(newest)
}
