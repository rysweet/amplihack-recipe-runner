//! Version-one durable contracts, independent of the legacy checkpoint format.
use crate::models::StepResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Persistence {
    #[serde(default)]
    pub context: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipePolicy {
    pub version: u32,
    pub persist: Persistence,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepPolicy {
    #[serde(default)]
    pub replay: Replay,
    #[serde(default)]
    pub resources: Vec<Resource>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Replay {
    Reuse,
    #[default]
    Never,
    Rebind,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Resource {
    File { path: String },
    Repository { path: String },
    Artifact { path: String, context_key: String },
    Live { name: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub schema_version: u32,
    pub run_id: String,
    pub sequence: u64,
    pub identity: String,
    pub directory: PathBuf,
    pub source: Option<Source>,
    pub reason: String,
    pub settings: String,
    pub fresh: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub run_id: String,
    pub sequence: u64,
    pub directory: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub invocation_path: String,
    pub step_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invalidation {
    #[serde(flatten)]
    pub location: Location,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub mode: String,
    pub reason: String,
    pub source_run_id: Option<String>,
    pub source_sequence: Option<u64>,
    pub restart: Option<Location>,
    pub restored_steps: Vec<Location>,
    pub invalidated_steps: Vec<Invalidation>,
}
impl Report {
    pub fn fresh(reason: &str) -> Self {
        Self {
            mode: "fresh".into(),
            reason: reason.into(),
            source_run_id: None,
            source_sequence: None,
            restart: None,
            restored_steps: vec![],
            invalidated_steps: vec![],
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub kind: String,
    pub path: String,
    pub digest: String,
    pub context_key: Option<String>,
    pub blob: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub definitions: BTreeMap<String, String>,
    pub location: Location,
    pub index: usize,
    pub group: Option<String>,
    pub definition: String,
    pub inputs: BTreeMap<String, Value>,
    pub permitted: bool,
    pub resources: Vec<Observation>,
    pub settings: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub child_resources: Vec<Observation>,
    pub child_definitions: BTreeMap<String, String>,
    pub probe: Probe,
    pub result: StepResult,
    pub delta: BTreeMap<String, Option<Value>>,
    pub descendants: Vec<Location>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tree {
    pub blockages: Vec<Location>,
    pub schema_version: u32,
    pub run_id: String,
    pub sequence: u64,
    pub identity: String,
    pub revision: u64,
    pub generation: u64,
    pub outcome: Outcome,
    pub records: Vec<Record>,
    pub repositories: BTreeMap<String, String>,
    pub report: Report,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Running,
    UsageLimit,
    Success,
    Failure,
}

pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub fn fingerprint<T: Serialize>(value: &T) -> anyhow::Result<String> {
    // serde_json's default map is a BTreeMap, so recursively converting to Value
    // gives a deterministic encoding even when the input uses HashMap.
    Ok(digest(&serde_json::to_vec(&serde_json::to_value(value)?)?))
}
