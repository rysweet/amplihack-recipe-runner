//! One writer owns publication of the entire completion tree.
use super::{
    model::*,
    private_fs::{Directory, parse},
    resources,
};
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

pub fn empty_tree(d: &Descriptor) -> Tree {
    Tree {
        blockages: vec![],
        schema_version: 1,
        run_id: d.run_id.clone(),
        sequence: d.sequence,
        identity: d.identity.clone(),
        revision: 0,
        generation: 0,
        outcome: Outcome::Running,
        records: vec![],
        repositories: BTreeMap::new(),
        report: Report::fresh(&d.reason),
    }
}
pub fn validate_tree(tree: &Tree) -> Result<()> {
    ensure!(
        tree.schema_version == 1 && tree.records.len() <= 10_000,
        "unsupported or oversized recovery tree"
    );
    ensure!(tree.blockages.len() <= 10_000, "too many blockage records");
    for blockage in &tree.blockages {
        validate_location(blockage)?;
    }
    let mut seen = BTreeSet::new();
    for record in &tree.records {
        validate_location(&record.probe.location)?;
        ensure!(
            record.probe.definition.len() == 64
                && record.probe.settings.len() == 64
                && record.probe.permitted,
            "unverifiable recovery record"
        );
        ensure!(
            matches!(
                record.result.status,
                crate::models::StepStatus::Completed | crate::models::StepStatus::Skipped
            ) && record.result.step_id == record.probe.location.step_id,
            "incomplete recovery record"
        );
        ensure!(
            seen.insert((
                &record.probe.location.invocation_path,
                &record.probe.location.step_id
            )),
            "duplicate completion identity"
        );
    }
    let records: BTreeSet<_> = tree
        .records
        .iter()
        .map(|r| (&r.probe.location.invocation_path, &r.probe.location.step_id))
        .collect();
    for record in &tree.records {
        let prefix = format!(
            "{}/{}/",
            record.probe.location.invocation_path, record.probe.location.step_id
        );
        let mut children = BTreeSet::new();
        for child in &record.descendants {
            validate_location(child)?;
            ensure!(
                child.invocation_path.starts_with(&prefix)
                    && records.contains(&(&child.invocation_path, &child.step_id))
                    && children.insert((&child.invocation_path, &child.step_id)),
                "invalid recovery child relationship"
            );
        }
    }
    Ok(())
}
pub fn validate_location(l: &Location) -> Result<()> {
    ensure!(
        l.invocation_path.len() <= 4096
            && !l.invocation_path.is_empty()
            && l.invocation_path.split('/').all(|s| !s.is_empty()
                && s != "."
                && s != ".."
                && s.len() <= 255
                && !s.chars().any(char::is_control)),
        "invalid invocation path"
    );
    super::private_fs::component(&l.step_id)?;
    ensure!(!l.step_id.chars().any(char::is_control), "invalid step id");
    Ok(())
}
pub struct Store {
    _writer: std::fs::File,
    pub tree: Tree,
    directory: Directory,
    candidates: Vec<Record>,
    invalid: BTreeSet<(String, String)>,
    started: BTreeSet<(String, String)>,
    pub descriptor: Descriptor,
}
impl Store {
    pub fn open(d: Descriptor) -> Result<Self> {
        ensure!(d.schema_version == 1, "unsupported recovery descriptor");
        let directory = Directory::open(&d.directory, false)?;
        let writer = directory.file("writer.lock", true)?;
        super::private_fs::lock(&writer, true)?;
        let mut tree = empty_tree(&d);
        let mut candidates = vec![];
        if let Some(source) = &d.source {
            let imported = (|| -> Result<Tree> {
                let source_dir = Directory::open(&source.directory, false)?;
                let previous: Tree = parse(&source_dir.read("tree.json")?)?;
                validate_tree(&previous)?;
                ensure!(
                    previous.run_id == source.run_id
                        && previous.sequence == source.sequence
                        && previous.identity == d.identity
                        && previous.outcome == Outcome::UsageLimit,
                    "ineligible source attempt"
                );
                resources::import(&previous, &source_dir, &directory)?;
                Ok(previous)
            })();
            match imported {
                Ok(previous) => {
                    tree.records = previous.records.clone();
                    tree.repositories = previous.repositories;
                    candidates = previous.records;
                    tree.report.mode = "resumed".into();
                    tree.report.reason = "usage_limit".into();
                    tree.report.source_run_id = Some(source.run_id.clone());
                    tree.report.source_sequence = Some(source.sequence);
                }
                Err(_) => {
                    tree.report = Report::fresh("state_unavailable");
                }
            }
        }
        let mut result = Self {
            _writer: writer,
            tree,
            directory,
            candidates,
            invalid: BTreeSet::new(),
            started: BTreeSet::new(),
            descriptor: d,
        };
        result.publish()?;
        if result.tree.report.mode == "fresh" {
            eprintln!(
                "recipe recovery: fresh reason={}",
                result.tree.report.reason
            );
        }
        Ok(result)
    }
    fn publish(&mut self) -> Result<()> {
        self.tree.revision = self
            .tree
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("recovery revision exhausted"))?;
        validate_tree(&self.tree)?;
        self.directory
            .publish("tree.json", &super::private_fs::serialize(&self.tree)?)
    }
    pub fn begin(&mut self, probe: &Probe) -> Result<Option<Record>> {
        validate_location(&probe.location)?;
        self.clear_blockage(&format!(
            "{}/{}",
            probe.location.invocation_path, probe.location.step_id
        ));
        let key = (
            probe.location.invocation_path.clone(),
            probe.location.step_id.clone(),
        );
        ensure!(
            self.started.insert(key.clone()),
            "duplicate step invocation"
        );
        if probe.rebind {
            // Current authorization/session admission runs before semantic replay.
            // Later steps still compare every input they actually consume. A new
            // live receipt alone does not discard independent semantic progress.
            self.publish()?;
            return Ok(None);
        }
        let candidate = self
            .candidates
            .iter()
            .find(|r| r.probe.location == probe.location)
            .cloned();
        let reason = if self.invalid.contains(&key) {
            Some("dependent_input_changed")
        } else if let Some(old) = &candidate {
            if old.probe.index != probe.index
                || old.probe.group != probe.group
                || old.probe.definition != probe.definition
            {
                Some("definition_changed")
            } else if old.probe.settings != probe.settings
                || old.probe.inputs != probe.inputs
                || !probe.permitted
            {
                Some("input_changed")
            } else if old.child_definitions.iter().any(|(path, hash)| {
                !resources::read_file(std::path::Path::new(path)).is_ok_and(|b| digest(&b) == *hash)
            }) {
                Some("definition_changed")
            } else {
                self.compatibility(&old.probe.resources, &probe.resources)
                    .or_else(|| {
                        let current: Vec<_> = old
                            .child_resources
                            .iter()
                            .map(|r| {
                                let mut observed = r.clone();
                                observed.digest = if r.kind == "repository" {
                                    resources::repository(std::path::Path::new(&r.path))
                                        .unwrap_or_default()
                                } else {
                                    resources::read_file(std::path::Path::new(&r.path))
                                        .map(|b| digest(&b))
                                        .unwrap_or_default()
                                };
                                observed
                            })
                            .collect();
                        self.compatibility(&old.child_resources, &current)
                    })
            }
        } else {
            Some("incomplete")
        };
        if let Some(reason) = reason {
            self.invalidate(probe, reason);
            self.note_restart(&probe.location);
            self.publish()?;
            return Ok(None);
        }
        let mut record = candidate.expect("checked candidate");
        // Rebind only explicitly declared artifact context slots. Never guess paths
        // embedded in output strings or prompts.
        for resource in &record.probe.resources {
            if let (Some(blob), Some(key)) = (&resource.blob, &resource.context_key) {
                let value = Value::String(
                    self.directory
                        .path
                        .join("artifacts")
                        .join(blob)
                        .display()
                        .to_string(),
                );
                record.delta.insert(key.clone(), Some(value));
            }
        }
        if !self.tree.report.restored_steps.contains(&probe.location) {
            self.tree.report.restored_steps.push(probe.location.clone());
        }
        for child in &record.descendants {
            if !self.tree.report.restored_steps.contains(child) {
                self.tree.report.restored_steps.push(child.clone());
            }
        }
        // Inherited records already live in this attempt, independently of ancestors.
        if let Some(r) = self
            .tree
            .records
            .iter_mut()
            .find(|r| r.probe.location == probe.location)
        {
            *r = record.clone();
        }
        self.publish()?;
        Ok(Some(record))
    }
    fn compatibility(&self, old: &[Observation], new: &[Observation]) -> Option<&'static str> {
        if old.len() != new.len() {
            return Some("resource_unavailable");
        }
        for (old, new) in old.iter().zip(new) {
            if old.kind != new.kind || old.path != new.path || old.context_key != new.context_key {
                return Some("resource_unavailable");
            }
            match old.kind.as_str() {
                "live" => return Some("live_resource_requires_admission"),
                "repository" => {
                    if new.digest.is_empty() {
                        return Some("resource_unavailable");
                    }
                    if self.tree.repositories.get(&old.path) != Some(&new.digest) {
                        return Some("repository_changed");
                    }
                }
                "artifact" => {
                    if !new.digest.is_empty() && new.digest != old.digest {
                        return Some("resource_changed");
                    }
                    let Some(blob) = &old.blob else {
                        return Some("resource_unavailable");
                    };
                    if !self
                        .directory
                        .child("artifacts", false)
                        .and_then(|d| d.read(blob))
                        .is_ok_and(|b| digest(&b) == old.digest)
                    {
                        return Some("resource_unavailable");
                    }
                }
                "file" => {
                    if new.digest.is_empty() {
                        return Some("resource_unavailable");
                    }
                    if old.digest != new.digest {
                        return Some("resource_changed");
                    }
                }
                _ => return Some("resource_unavailable"),
            }
        }
        None
    }
    fn invalidate(&mut self, probe: &Probe, reason: &str) {
        let invocation = &probe.location.invocation_path;
        let affected: Vec<_> = self
            .candidates
            .iter()
            .filter(|r| {
                r.probe.location.invocation_path == *invocation
                    && r.probe.index >= probe.index
                    && (r.probe.location == probe.location
                        || probe.group.is_none()
                        || r.probe.group != probe.group)
            })
            .map(|r| r.probe.location.clone())
            .collect();
        for location in affected {
            self.invalid
                .insert((location.invocation_path.clone(), location.step_id.clone()));
            // Child completions remain candidates: they perform their own definition,
            // input and resource checks when the parent has to execute again.
            self.tree
                .records
                .retain(|r| r.probe.location != location && !r.descendants.contains(&location));
        }
        self.tree.report.invalidated_steps.push(Invalidation {
            location: probe.location.clone(),
            reason: reason.into(),
        });
    }
    fn note_restart(&mut self, location: &Location) {
        let replace = self.tree.report.restart.as_ref().is_none_or(|prior| {
            location
                .invocation_path
                .starts_with(&format!("{}/{}/", prior.invocation_path, prior.step_id))
        });
        if replace {
            self.tree.report.restart = Some(location.clone());
            if let Some(source) = &self.tree.report.source_run_id {
                eprintln!(
                    "recipe recovery: resumed run={} step={} invocation={}",
                    source, location.step_id, location.invocation_path
                );
            }
        }
    }
    pub fn commit(&mut self, mut record: Record) -> Result<()> {
        self.clear_blockage(&format!(
            "{}/{}",
            record.probe.location.invocation_path, record.probe.location.step_id
        ));
        validate_location(&record.probe.location)?;
        ensure!(
            self.started.contains(&(
                record.probe.location.invocation_path.clone(),
                record.probe.location.step_id.clone()
            )),
            "completion was not admitted"
        );
        let prefix = format!(
            "{}/{}/",
            record.probe.location.invocation_path, record.probe.location.step_id
        );
        let children: Vec<_> = self
            .tree
            .records
            .iter()
            .filter(|r| r.probe.location.invocation_path.starts_with(&prefix))
            .map(|r| r.probe.location.clone())
            .collect();
        if !children.is_empty()
            && children
                .iter()
                .all(|l| self.tree.report.restored_steps.contains(l))
            && self.tree.report.restart.as_ref() == Some(&record.probe.location)
        {
            // An entered container can discover that all its leaves replayed.
            // It is not the actual restart location; wait for newly executed work.
            self.tree.report.restart = None;
        }
        let subtree = format!(
            "{}/{}/",
            record.probe.location.invocation_path, record.probe.location.step_id
        );
        if self.started.iter().any(|(path, id)| {
            path.starts_with(&subtree)
                && !self.tree.records.iter().any(|r| {
                    r.probe.location.invocation_path == *path && r.probe.location.step_id == *id
                })
        }) {
            record.probe.permitted = false;
        }
        record.child_resources = self
            .tree
            .records
            .iter()
            .filter(|r| r.probe.location.invocation_path.starts_with(&subtree))
            .flat_map(|r| r.probe.resources.iter())
            .cloned()
            .collect();
        // An enclosing structured output has no authority to rewrite embedded
        // child paths. Enter the container and rebind at its declared child slot.
        if record.child_resources.iter().any(|r| r.kind == "artifact") {
            record.probe.permitted = false;
        }
        record.child_definitions = self
            .tree
            .records
            .iter()
            .filter(|r| r.probe.location.invocation_path.starts_with(&subtree))
            .flat_map(|r| r.probe.definitions.iter().chain(r.child_definitions.iter()))
            .map(|(p, h)| (p.clone(), h.clone()))
            .collect();
        if !record.probe.permitted {
            return self.publish();
        }
        if record
            .probe
            .resources
            .iter()
            .any(|r| r.digest.is_empty() || r.kind == "live")
        {
            return Ok(());
        }
        resources::capture(&mut record.probe.resources, &self.directory)?;
        for resource in &record.probe.resources {
            if resource.kind == "repository" {
                self.tree
                    .repositories
                    .insert(resource.path.clone(), resource.digest.clone());
            }
        }
        let prefix = format!(
            "{}/{}/",
            record.probe.location.invocation_path, record.probe.location.step_id
        );
        record.descendants = self
            .tree
            .records
            .iter()
            .filter(|r| r.probe.location.invocation_path.starts_with(&prefix))
            .map(|r| r.probe.location.clone())
            .collect();
        if let Some(previous) = self
            .tree
            .records
            .iter()
            .find(|r| r.probe.location == record.probe.location)
        {
            ensure!(
                fingerprint(previous)? == fingerprint(&record)?,
                "conflicting repeated recovery completion"
            );
            return Ok(());
        }
        self.tree.records.push(record);
        self.publish()
    }
    pub fn retry_subtree(&mut self, prefix: &str) -> Result<()> {
        self.started
            .retain(|(path, _)| !path.starts_with(&format!("{prefix}/")));
        for record in &self.tree.records {
            if record
                .probe
                .location
                .invocation_path
                .starts_with(&format!("{prefix}/"))
            {
                self.candidates
                    .retain(|r| r.probe.location != record.probe.location);
                self.candidates.push(record.clone());
            }
        }
        self.scoped_outcome(prefix, Outcome::Running)
    }
    pub fn fresh_subtree(&mut self, prefix: &str) -> Result<()> {
        let keep = |r: &Record| {
            !prefix.is_empty()
                && !r
                    .probe
                    .location
                    .invocation_path
                    .starts_with(&format!("{prefix}/"))
                && !r
                    .descendants
                    .iter()
                    .any(|l| l.invocation_path.starts_with(&format!("{prefix}/")))
        };
        self.candidates.retain(keep);
        self.tree.records.retain(keep);
        if prefix.is_empty() {
            self.tree.report = Report::fresh("fresh_requested");
        }
        self.publish()
    }
    pub fn attachment_admitted(&self, prefix: &str) -> bool {
        self.started
            .iter()
            .any(|(path, id)| prefix == format!("{path}/{id}"))
    }
    fn clear_blockage(&mut self, prefix: &str) {
        self.tree.blockages.retain(|l| {
            let path = format!("{}/{}", l.invocation_path, l.step_id);
            path != prefix && !path.starts_with(&format!("{prefix}/"))
        });
        self.tree.outcome = if self.tree.blockages.is_empty() {
            Outcome::Running
        } else {
            Outcome::UsageLimit
        };
    }
    pub fn scoped_outcome(&mut self, prefix: &str, outcome: Outcome) -> Result<()> {
        match outcome {
            Outcome::Running => self.clear_blockage(prefix),
            Outcome::UsageLimit => {
                let (path, step) = prefix
                    .rsplit_once('/')
                    .ok_or_else(|| anyhow::anyhow!("invalid blockage scope"))?;
                let location = Location {
                    invocation_path: path.into(),
                    step_id: step.into(),
                };
                validate_location(&location)?;
                if !self.tree.blockages.contains(&location) {
                    self.tree.blockages.push(location);
                }
                self.tree.outcome = Outcome::UsageLimit;
            }
            _ => anyhow::bail!("child cannot finalize attempt"),
        }
        self.tree.generation = self
            .tree
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("recovery generation exhausted"))?;
        self.publish()
    }
    pub fn finalize_result(
        &mut self,
        recipe: &str,
        failed: &[String],
        requested: Outcome,
    ) -> Result<Option<Location>> {
        let cause = if requested == Outcome::Success {
            None
        } else {
            self.tree
                .blockages
                .iter()
                .find(|l| {
                    failed.iter().any(|step| {
                        let prefix = format!("{recipe}/{step}");
                        let origin = format!("{}/{}", l.invocation_path, l.step_id);
                        origin == prefix || origin.starts_with(&format!("{prefix}/"))
                    })
                })
                .cloned()
        };
        self.outcome(if cause.is_some() {
            Outcome::UsageLimit
        } else {
            requested
        })?;
        Ok(cause)
    }
    pub fn outcome(&mut self, outcome: Outcome) -> Result<()> {
        self.tree.generation = self
            .tree
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("recovery generation exhausted"))?;
        self.tree.outcome = outcome;
        if outcome != Outcome::UsageLimit {
            self.tree.blockages.clear();
        }
        self.publish()
    }
}
use serde_json::Value;
