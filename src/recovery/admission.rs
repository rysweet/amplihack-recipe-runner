//! Launch registration. Call once in the executing process, after authorization.
use super::{
    model::*,
    private_fs::{Directory, lock, parse},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs::File, path::Path};

#[derive(Serialize, Deserialize)]
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
            let latest = match index.read("latest.json") {
                Ok(b) => Some(parse::<Latest>(&b)?),
                Err(e)
                    if e.downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    None
                }
                Err(e) => return Err(e.context("cannot verify latest recovery attempt")),
            };
            descriptor.reason = if fresh {
                "fresh_requested"
            } else {
                "no_history"
            }
            .into();
            if let Some(previous) = latest {
                ensure!(
                    previous.schema_version == 1,
                    "unsupported latest recovery marker"
                );
                super::private_fs::component(&previous.run_id)?;
                descriptor.sequence = previous
                    .sequence
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("recovery sequence exhausted"))?;
                if !fresh {
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
            }
            // Register a complete unknown/running attempt before publishing latest.
            let tree = super::store::empty_tree(&descriptor);
            recovery.publish("tree.json", &super::private_fs::serialize(&tree)?)?;
            recovery.publish(
                "descriptor.json",
                &super::private_fs::serialize(&descriptor)?,
            )?;
            index.publish(
                "latest.json",
                &super::private_fs::serialize(&Latest {
                    schema_version: 1,
                    run_id,
                    sequence: descriptor.sequence,
                })?,
            )?;
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
