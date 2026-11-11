#![cfg(unix)]
use recipe_runner_rs::recovery::{
    admission::Admission,
    model::*,
    private_fs::{Directory, parse},
    store::Store,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command};

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    recipe: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        Directory::open(&root.join("state"), true).unwrap();
        Directory::open(&root.join("repo"), true).unwrap();
        let recipe = root.join("recipe.yaml");
        let f = Self {
            _tmp: tmp,
            root,
            recipe,
        };
        f.write();
        f
    }
    fn write(&self) {
        let step = |id: &str| json!({"id":id,"command":format!("printf '{id}\\n' >> '{}'; if test -f '{}/limit-{id}'; then printf \"You've hit your session limit\\n\" >&2; exit 1; fi; printf '%s' '{{\"n\":7,\"nil\":null,\"yes\":true}}'",self.root.join("events").display(),self.root.display()),"parse_json":true,"output":id,"auto_stage":false,"recovery":{"replay":"reuse"}});
        fs::write(&self.recipe,serde_yaml::to_string(&json!({"name":"workflow","recovery":{"version":1,"persist":{"context":["first","second","third"],"outputs":["first","second","third"]}},"steps":[step("first"),step("second"),step("third")]})).unwrap()).unwrap();
    }
    fn admit(&self, id: &str, fresh: bool) -> Admission {
        let dir = self.root.join("state/runs").join(id);
        Directory::open(&dir, true).unwrap();
        Admission::begin(
            &self.root.join("state"),
            &dir,
            Some("repo/issue/recipe"),
            fresh,
            "settings".into(),
        )
        .unwrap()
    }
    fn run(&self, admission: &Admission) -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
            .arg(&self.recipe)
            .args(["--output-format", "json", "--no-update-check", "-C"])
            .arg(self.root.join("repo"))
            .arg("--recovery-descriptor")
            .arg(admission.descriptor_path())
            .env_remove("AMPLIHACK_RECOVERY_ATTACHMENT")
            .output()
            .unwrap();
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stderr)))
    }
    fn events(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("events"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}
#[test]
fn separate_process_recovery_preserves_typed_outputs_and_original_results() {
    let f = Fixture::new();
    fs::write(f.root.join("limit-second"), "").unwrap();
    let first = f.admit("a", false);
    let result = f.run(&first);
    assert_eq!(result["success"], false);
    let original = result["step_results"][0].clone();
    drop(first);
    fs::remove_file(f.root.join("limit-second")).unwrap();
    let next = f.admit("b", false);
    assert!(next.descriptor.source.is_some());
    let result = f.run(&next);
    assert_eq!(result["success"], true);
    assert_eq!(result["step_results"][0], original);
    assert_eq!(
        result["context"]["first"],
        json!({"n":7,"nil":null,"yes":true})
    );
    assert_eq!(f.events(), ["first", "second", "second", "third"]);
}
#[test]
fn active_owner_and_latest_tombstone_prevent_older_recovery() {
    let f = Fixture::new();
    let first = f.admit("a", false);
    let second = f.admit("b", false);
    assert!(second.descriptor.source.is_none());
    assert_eq!(second.descriptor.reason, "owner_active");
    drop(first);
    drop(second);
    fs::remove_dir_all(f.root.join("state/runs/b")).unwrap();
    let third = f.admit("c", false);
    assert!(third.descriptor.source.is_none());
    assert_eq!(third.descriptor.sequence, 3);
}
#[test]
fn repeated_recovery_is_self_contained_and_fresh_keeps_history() {
    let f = Fixture::new();
    fs::write(f.root.join("limit-second"), "").unwrap();
    let a = f.admit("a", false);
    f.run(&a);
    drop(a);
    let b = f.admit("b", true);
    let result = f.run(&b);
    assert_eq!(result["recovery"]["reason"], "fresh_requested");
    drop(b);
    assert!(f.root.join("state/runs/a/recovery/tree.json").is_file());
    fs::remove_file(f.root.join("limit-second")).unwrap();
    fs::write(f.root.join("limit-third"), "").unwrap();
    let c = f.admit("c", false);
    f.run(&c);
    drop(c);
    fs::remove_dir_all(f.root.join("state/runs/b")).unwrap();
    fs::remove_file(f.root.join("limit-third")).unwrap();
    let d = f.admit("d", false);
    let result = f.run(&d);
    assert_eq!(result["success"], true);
    assert_eq!(result["recovery"]["restart"]["step_id"], "third");
    assert_eq!(
        f.events(),
        [
            "first", "second", "first", "second", "second", "third", "third"
        ]
    );
}
#[test]
fn corruption_and_unknown_versions_never_replay() {
    for damaged in [
        b"{\"schema_version\":".as_slice(),
        b"{\"schema_version\":999}".as_slice(),
    ] {
        let f = Fixture::new();
        fs::write(f.root.join("limit-second"), "").unwrap();
        let a = f.admit("a", false);
        f.run(&a);
        drop(a);
        fs::write(f.root.join("state/runs/a/recovery/tree.json"), damaged).unwrap();
        fs::remove_file(f.root.join("limit-second")).unwrap();
        let b = f.admit("b", false);
        let result = f.run(&b);
        assert_eq!(result["recovery"]["reason"], "state_unavailable");
        assert_eq!(f.events(), ["first", "second", "first", "second", "third"]);
    }
}
#[test]
fn typed_context_delta_preserves_null_and_removal() {
    use recipe_runner_rs::{
        context::RecipeContext,
        models::{StepResult, StepStatus},
    };
    use std::collections::BTreeMap;
    let mut ctx =
        RecipeContext::new(serde_json::from_value(json!({"remove":1,"null":"old"})).unwrap());
    let record = Record {
        child_resources: vec![],
        child_definitions: Default::default(),
        probe: probe("workflow", "x", 0),
        result: StepResult {
            step_id: "x".into(),
            status: StepStatus::Completed,
            output: "".into(),
            error: "".into(),
            duration: None,
        },
        delta: BTreeMap::from([
            ("remove".into(), ContextChange::Remove),
            ("null".into(), ContextChange::Assign { value: Value::Null }),
        ]),
        descendants: vec![],
    };
    let record: Record =
        parse(&recipe_runner_rs::recovery::private_fs::serialize(&record).unwrap()).unwrap();
    recipe_runner_rs::recovery::replay::apply(&mut ctx, &record);
    assert!(!ctx.data().contains_key("remove"));
    assert_eq!(ctx.data()["null"], Value::Null);
}
fn probe(path: &str, step: &str, index: usize) -> Probe {
    Probe {
        rebind: false,
        definitions: Default::default(),
        location: Location {
            invocation_path: path.into(),
            step_id: step.into(),
        },
        index,
        group: None,
        definition: "d".repeat(64),
        inputs: Default::default(),
        permitted: true,
        resources: vec![],
        settings: "s".repeat(64),
    }
}
#[test]
fn duplicate_properties_at_any_depth_are_rejected() {
    for bytes in [
        br#"{"a":1,"a":1}"#.as_slice(),
        br#"{"inner":{"b":null,"b":true}}"#.as_slice(),
    ] {
        assert!(parse::<Value>(bytes).is_err());
    }
}
#[test]
fn private_storage_rejects_links_special_files_and_insecure_modes() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let tmp = tempfile::tempdir().unwrap();
    let root = Directory::open(&tmp.path().canonicalize().unwrap().join("private"), true).unwrap();
    root.publish("valid", b"{}").unwrap();
    symlink(root.path.join("valid"), root.path.join("link")).unwrap();
    assert!(root.read("link").is_err());
    fs::hard_link(root.path.join("valid"), root.path.join("hard")).unwrap();
    assert!(root.read("hard").is_err());
    root.publish("mode", b"{}").unwrap();
    fs::set_permissions(root.path.join("mode"), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(root.read("mode").is_err());
    let linked = root.path.join("linked");
    symlink(&root.path, &linked).unwrap();
    assert!(Directory::open(&linked, false).is_err());
}
#[test]
fn publication_failure_cannot_count_as_completion() {
    let f = Fixture::new();
    let a = f.admit("a", false);
    let mut store = Store::open(a.descriptor.clone()).unwrap();
    let p = probe("workflow", "x", 0);
    assert!(store.begin(&p).unwrap().is_none());
    fs::remove_file(a.descriptor.directory.join("tree.json")).unwrap();
    fs::create_dir(a.descriptor.directory.join("tree.json")).unwrap();
    assert!(store.outcome(Outcome::UsageLimit).is_err());
}
#[test]
fn delegated_attachment_cannot_read_siblings_or_finalize_root() {
    use recipe_runner_rs::recovery::ipc::Server;
    let f = Fixture::new();
    let a = f.admit("a", false);
    let server = Server::start(Store::open(a.descriptor.clone()).unwrap()).unwrap();
    let root = server.client();
    root.begin(probe("workflow", "delegate", 0)).unwrap();
    let child = root.attach("workflow/delegate".into()).unwrap();
    assert!(child.report().is_err());
    assert!(child.outcome(Outcome::Success).is_err());
    assert!(
        child
            .begin(probe("workflow/sibling/child", "x", 0))
            .is_err()
    );
    assert!(
        child
            .begin(probe("workflow/delegate/child", "x", 0))
            .unwrap()
            .is_none()
    );
    server.finish(Outcome::Failure).unwrap();
}

#[test]
fn renewal_revokes_stale_descendants_and_root_failure_wins() {
    use recipe_runner_rs::recovery::ipc::Server;
    let f = Fixture::new();
    let a = f.admit("a", false);
    let server = Server::start(Store::open(a.descriptor.clone()).unwrap()).unwrap();
    let root = server.client();
    root.begin(probe("workflow", "delegate", 0)).unwrap();
    let child = root.attach("workflow/delegate".into()).unwrap();
    child
        .begin(probe("workflow/delegate/child", "agent", 0))
        .unwrap();
    let descendant = child
        .attach("workflow/delegate/child/agent".into())
        .unwrap();
    descendant.outcome(Outcome::UsageLimit).unwrap();
    let renewed = child.renew().unwrap();
    assert!(child.outcome(Outcome::UsageLimit).is_err());
    assert!(descendant.outcome(Outcome::UsageLimit).is_err());
    renewed.outcome(Outcome::UsageLimit).unwrap();
    server.finish(Outcome::Failure).unwrap();
    drop(a);
    let b = f.admit("b", false);
    assert!(b.descriptor.source.is_none());
    assert_eq!(b.descriptor.reason, "latest_not_usage_limit");
}
fn completion(probe: Probe) -> Record {
    use recipe_runner_rs::models::{StepResult, StepStatus};
    Record {
        result: StepResult {
            step_id: probe.location.step_id.clone(),
            status: StepStatus::Completed,
            output: String::new(),
            error: String::new(),
            duration: None,
        },
        probe,
        delta: Default::default(),
        descendants: vec![],
        child_definitions: Default::default(),
        child_resources: vec![],
    }
}
#[test]
fn completed_container_checks_delegated_child_definitions_and_resources() {
    for change_definition in [true, false] {
        let f = Fixture::new();
        let definition = f.root.join("child.yaml");
        let resource = f.root.join("input");
        fs::write(&definition, "original").unwrap();
        fs::write(&resource, "original").unwrap();
        let a = f.admit("a", false);
        let mut store = Store::open(a.descriptor.clone()).unwrap();
        let parent = probe("workflow", "delegate", 0);
        store.begin(&parent).unwrap();
        let mut child = probe("workflow/delegate/child", "first", 0);
        child
            .definitions
            .insert(definition.display().to_string(), digest(b"original"));
        child.resources.push(Observation {
            kind: "file".into(),
            path: resource.display().to_string(),
            digest: digest(b"original"),
            context_key: None,
            blob: None,
        });
        store.begin(&child).unwrap();
        store.commit(completion(child)).unwrap();
        store.commit(completion(parent.clone())).unwrap();
        store.outcome(Outcome::UsageLimit).unwrap();
        drop(store);
        drop(a);
        if change_definition {
            fs::write(&definition, "changed").unwrap();
        } else {
            fs::remove_file(&resource).unwrap();
        }
        let b = f.admit("b", false);
        let mut store = Store::open(b.descriptor.clone()).unwrap();
        assert!(store.begin(&parent).unwrap().is_none());
        assert!(store.tree.report.invalidated_steps.iter().any(|i| i.reason
            == if change_definition {
                "definition_changed"
            } else {
                "resource_unavailable"
            }));
    }
}
#[test]
fn an_unrecordable_child_cannot_be_hidden_by_a_completed_parent() {
    let f = Fixture::new();
    let a = f.admit("a", false);
    let mut store = Store::open(a.descriptor.clone()).unwrap();
    let parent = probe("workflow", "delegate", 0);
    store.begin(&parent).unwrap();
    let mut child = probe("workflow/delegate/child", "secret", 0);
    child.permitted = false;
    store.begin(&child).unwrap();
    store.commit(completion(child)).unwrap();
    store.commit(completion(parent)).unwrap();
    assert!(store.tree.records.is_empty());
}
#[test]
fn nested_separate_process_recovery_enters_incomplete_parent_and_reuses_child_prefix() {
    let f = Fixture::new();
    let original: Value = serde_yaml::from_str(&fs::read_to_string(&f.recipe).unwrap()).unwrap();
    let child = f.root.join("child.yaml");
    fs::write(&child, serde_yaml::to_string(&original).unwrap()).unwrap();
    let mut parent = original;
    parent["steps"] =
        json!([{"id":"delegate","type":"recipe","recipe":"child","recovery":{"replay":"reuse"}}]);
    fs::write(&f.recipe, serde_yaml::to_string(&parent).unwrap()).unwrap();
    fs::write(f.root.join("limit-second"), "").unwrap();
    let a = f.admit("a", false);
    let result = f.run(&a);
    assert_eq!(result["success"], false);
    drop(a);
    fs::remove_file(f.root.join("limit-second")).unwrap();
    let b = f.admit("b", false);
    let result = f.run(&b);
    assert_eq!(result["success"], true);
    assert_eq!(result["recovery"]["restart"]["step_id"], "second");
    assert!(
        result["recovery"]["restart"]["invocation_path"]
            .as_str()
            .unwrap()
            .contains("delegate/workflow")
    );
    assert_eq!(f.events(), ["first", "second", "second", "third"]);
}

#[test]
fn current_child_limit_survives_a_generic_parent_error_but_not_an_unrelated_failure() {
    use recipe_runner_rs::{
        models::{RecipeResult, StepResult, StepStatus},
        recovery::ipc::Server,
    };
    for failed in ["delegate", "unrelated"] {
        let f = Fixture::new();
        let a = f.admit("a", false);
        let server = Server::start(Store::open(a.descriptor.clone()).unwrap()).unwrap();
        let root = server.client();
        root.begin(probe("workflow", "delegate", 0)).unwrap();
        let child = root.attach("workflow/delegate".into()).unwrap();
        child.outcome(Outcome::UsageLimit).unwrap();
        // A successful parallel sibling must not erase the originating blockage.
        let sibling = probe("workflow", "sibling", 1);
        root.begin(sibling.clone()).unwrap();
        root.commit(completion(sibling)).unwrap();
        let result = RecipeResult {
            recipe_name: "workflow".into(),
            success: false,
            step_results: vec![StepResult {
                step_id: failed.into(),
                status: StepStatus::Failed,
                output: String::new(),
                error: "child failed".into(),
                duration: None,
            }],
            context: Default::default(),
            duration: None,
        };
        let (_, cause) = server.finish_result(&result).unwrap();
        assert_eq!(cause.is_some(), failed == "delegate");
        drop(a);
        let b = f.admit("b", false);
        assert_eq!(b.descriptor.source.is_some(), failed == "delegate");
    }
}

#[test]
fn repeated_delegation_is_distinct_and_retry_preserves_its_call_identity() {
    use recipe_runner_rs::recovery::ipc::Server;
    let f = Fixture::new();
    let a = f.admit("a", false);
    let server = Server::start(Store::open(a.descriptor.clone()).unwrap()).unwrap();
    let root = server.client();
    root.begin(probe("workflow", "delegate", 0)).unwrap();
    let parent = root.attach("workflow/delegate".into()).unwrap();
    let discriminator = fingerprint(&"workstream-a").unwrap();
    let first = parent
        .enter_invocation("child".into(), discriminator.clone())
        .unwrap();
    let second = parent
        .enter_invocation("child".into(), discriminator.clone())
        .unwrap();
    assert_ne!(first.prefix, second.prefix);
    let p = probe(&format!("{}/child", first.prefix), "first", 0);
    first.begin(p.clone()).unwrap();
    first.commit(completion(p.clone())).unwrap();
    parent.outcome(Outcome::UsageLimit).unwrap();
    let renewed = parent.renew().unwrap();
    let next = renewed
        .enter_invocation("child".into(), discriminator)
        .unwrap();
    assert_eq!(next.prefix, first.prefix);
    assert!(next.begin(p).unwrap().is_some());
    server.finish(Outcome::Success).unwrap();
}
#[test]
fn delegated_fresh_only_discards_its_own_candidates() {
    let f = Fixture::new();
    let a = f.admit("a", false);
    let mut store = Store::open(a.descriptor.clone()).unwrap();
    for path in ["workflow/left/child", "workflow/right/child"] {
        let p = probe(path, "first", 0);
        store.begin(&p).unwrap();
        store.commit(completion(p)).unwrap();
    }
    store.outcome(Outcome::UsageLimit).unwrap();
    drop(store);
    drop(a);
    let b = f.admit("b", false);
    let mut store = Store::open(b.descriptor.clone()).unwrap();
    store.fresh_subtree("workflow/left").unwrap();
    assert!(
        store
            .begin(&probe("workflow/left/child", "first", 0))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .begin(&probe("workflow/right/child", "first", 0))
            .unwrap()
            .is_some()
    );
}

#[test]
fn surviving_writer_prevents_recovery_after_the_host_owner_disappears() {
    let f = Fixture::new();
    let a = f.admit("a", false);
    let mut writer = Store::open(a.descriptor.clone()).unwrap();
    writer.outcome(Outcome::UsageLimit).unwrap();
    drop(a);
    let b = f.admit("b", false);
    assert!(b.descriptor.source.is_none());
    assert_eq!(b.descriptor.reason, "owner_active");
    drop(writer);
}

#[test]
fn current_live_admission_does_not_discard_compatible_semantic_progress() {
    let f = Fixture::new();
    let mut recipe: Value = serde_yaml::from_str(&fs::read_to_string(&f.recipe).unwrap()).unwrap();
    recipe["steps"][0]["recovery"]["replay"] = json!("rebind");
    fs::write(&f.recipe, serde_yaml::to_string(&recipe).unwrap()).unwrap();
    fs::write(f.root.join("limit-third"), "").unwrap();
    let a = f.admit("a", false);
    f.run(&a);
    drop(a);
    fs::remove_file(f.root.join("limit-third")).unwrap();
    let b = f.admit("b", false);
    let result = f.run(&b);
    assert_eq!(result["success"], true);
    assert_eq!(f.events(), ["first", "second", "third", "first", "third"]);
    assert_eq!(result["recovery"]["restart"]["step_id"], "third");
}

#[test]
fn step_owner_retires_the_renewed_generation_when_the_step_ends() {
    use recipe_runner_rs::recovery::ipc::{Server, enter_owned};
    let f = Fixture::new();
    let a = f.admit("a", false);
    let server = Server::start(Store::open(a.descriptor.clone()).unwrap()).unwrap();
    let root = server.client();
    root.begin(probe("workflow", "agent", 0)).unwrap();
    let child = root.attach("workflow/agent".into()).unwrap();
    let scope = enter_owned(Some(child.clone()), Some(root));
    let renewed = child.renew().unwrap();
    drop(scope);
    assert!(renewed.outcome(Outcome::UsageLimit).is_err());
    server.finish(Outcome::Success).unwrap();
}

#[test]
fn canonical_fingerprints_ignore_map_order_and_preserve_scalar_and_list_types() {
    let a: Value =
        serde_json::from_str(r#"{"outer":{"b":true,"a":null},"list":[1,"two"]}"#).unwrap();
    let b: Value =
        serde_json::from_str(r#"{"list":[1,"two"],"outer":{"a":null,"b":true}}"#).unwrap();
    assert_eq!(fingerprint(&a).unwrap(), fingerprint(&b).unwrap());
    assert_ne!(fingerprint(&true).unwrap(), fingerprint(&"true").unwrap());
    assert_ne!(
        fingerprint(&json!([1, 2])).unwrap(),
        fingerprint(&json!([2, 1])).unwrap()
    );
}
