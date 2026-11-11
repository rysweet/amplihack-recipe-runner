use super::*;
use recipe_runner_rs::{
    context::RecipeContext,
    models::{Recipe, Step},
};
use std::os::unix::fs::PermissionsExt;

fn index(f: &Fixture) -> Directory {
    Directory::open(
        &f.root
            .join("state/recovery-index")
            .join(digest(b"repo/issue/recipe")),
        false,
    )
    .unwrap()
}

#[test]
fn corrupt_latest_contents_admit_fresh_with_monotonic_order_and_no_older_source() {
    for fresh in [false, true] {
        for legacy in [false, true] {
            let f = Fixture::new();
            fs::write(f.root.join("limit-second"), "").unwrap();
            let a = f.admit("a", false);
            f.run(&a);
            drop(a);
            let index = index(&f);
            if legacy {
                fs::remove_file(index.path.join("ordering.json")).unwrap();
            }
            index
                .publish("latest.json", b"{\"schema_version\":")
                .unwrap();
            fs::remove_file(f.root.join("limit-second")).unwrap();
            let b = f.admit("b", fresh);
            assert_eq!(b.descriptor.sequence, 2);
            assert!(b.descriptor.source.is_none());
            assert_eq!(f.run(&b)["success"], true);
            drop(b);
            let c = f.admit("c", false);
            assert_eq!(c.descriptor.sequence, 3);
            assert!(c.descriptor.source.is_none());
            assert_eq!(c.descriptor.reason, "latest_not_usage_limit");
            assert_eq!(f.events(), ["first", "second", "first", "second", "third"]);
        }
    }
}

#[test]
fn interrupted_latest_publication_and_replaced_old_marker_cannot_select_old_history() {
    let f = Fixture::new();
    fs::write(f.root.join("limit-second"), "").unwrap();
    let a = f.admit("a", false);
    f.run(&a);
    drop(a);
    let index = index(&f);
    let old_marker = index.read("latest.json").unwrap();
    let b = f.admit("b", false);
    drop(b);
    // Simulate a crash after the durable high-water registration, before rename
    // of the replacement latest marker. Older eligible history is still present.
    index.publish("latest.json", &old_marker).unwrap();
    let c = f.admit("c", false);
    assert_eq!(c.descriptor.sequence, 3);
    assert!(c.descriptor.source.is_none());
    assert_eq!(c.descriptor.reason, "state_unavailable");
    drop(c);
    fs::remove_dir_all(f.root.join("state/runs/b")).unwrap();
    fs::remove_dir_all(f.root.join("state/runs/c")).unwrap();
    index.publish("latest.json", b"{").unwrap();
    let d = f.admit("d", true);
    assert_eq!(d.descriptor.sequence, 4);
    assert!(d.descriptor.source.is_none());
}

#[test]
fn unsafe_current_marker_is_rejected_even_for_fresh_launches() {
    for fresh in [false, true] {
        let f = Fixture::new();
        drop(f.admit("a", false));
        let index = index(&f);
        fs::set_permissions(
            index.path.join("latest.json"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let run = f.root.join("state/runs/b");
        Directory::open(&run, true).unwrap();
        assert!(
            Admission::begin(
                &f.root.join("state"),
                &run,
                Some("repo/issue/recipe"),
                fresh,
                "s".into()
            )
            .is_err()
        );
    }
}

fn shell_probe(value: Option<Value>, permitted: bool) -> Probe {
    let recipe: Recipe = serde_yaml::from_str("name: workflow\nrecovery:\n  version: 1\n  persist:\n    context: [input]\n    outputs: [read]\nsteps: []\n").unwrap();
    let step: Step = serde_json::from_value(
        json!({"id":"read","command":"helper", "recovery":{"replay":"reuse"}}),
    )
    .unwrap();
    let mut data = std::collections::HashMap::new();
    if let Some(value) = value {
        data.insert("input".into(), value);
    }
    if !permitted {
        data.insert("secret".into(), json!("DO_NOT_PERSIST"));
    }
    recipe_runner_rs::recovery::replay::probe(
        &recipe,
        &step,
        0,
        "workflow",
        &RecipeContext::new(data),
        ".",
        "s",
        Value::Null,
    )
    .unwrap()
}

#[test]
fn shell_dependencies_preserve_absence_null_and_types_without_broadening_permissions() {
    let probes: Vec<_> = [
        None,
        Some(Value::Null),
        Some(json!("")),
        Some(json!(false)),
        Some(json!(0)),
        Some(json!({"a":1})),
    ]
    .into_iter()
    .map(|value| shell_probe(value, true))
    .collect();
    for (i, a) in probes.iter().enumerate() {
        assert!(a.permitted);
        for b in probes.iter().skip(i + 1) {
            assert_ne!(a.inputs, b.inputs);
        }
    }
    let secret = shell_probe(Some(json!("input")), false);
    assert!(!secret.permitted);
    assert!(
        !serde_json::to_string(&secret)
            .unwrap()
            .contains("DO_NOT_PERSIST")
    );
}

#[test]
fn repository_invalidation_is_durable_before_new_baseline_and_preserves_other_paths() {
    let f = Fixture::new();
    let repo = |mut p: Probe, path: &str, bytes: &[u8]| {
        p.resources.push(Observation {
            kind: "repository".into(),
            path: path.into(),
            digest: digest(bytes),
            context_key: None,
            blob: None,
        });
        p
    };
    let a = f.admit("a", false);
    let mut store = Store::open(a.descriptor.clone()).unwrap();
    let parent = probe("workflow", "delegate", 1);
    store.begin(&parent).unwrap();
    let reader = repo(
        probe("workflow/delegate/child", "reader", 0),
        "/repo-a",
        b"old",
    );
    store.begin(&reader).unwrap();
    store.commit(completion(reader.clone())).unwrap();
    let other = repo(
        probe("workflow/other/child", "independent", 0),
        "/repo-b",
        b"same",
    );
    store.begin(&other).unwrap();
    store.commit(completion(other.clone())).unwrap();
    store.outcome(Outcome::UsageLimit).unwrap();
    drop(store);
    drop(a);
    let b = f.admit("b", false);
    let mut store = Store::open(b.descriptor.clone()).unwrap();
    let classify = repo(probe("workflow", "classify", 0), "/repo-a", b"new");
    store.begin(&classify).unwrap();
    store.commit(completion(classify)).unwrap();
    assert!(
        !store
            .tree
            .records
            .iter()
            .any(|r| r.probe.location == reader.location)
    );
    assert!(store.begin(&other).unwrap().is_some());
    store.outcome(Outcome::UsageLimit).unwrap();
    drop(store);
    drop(b);
    let c = f.admit("c", false);
    let mut store = Store::open(c.descriptor.clone()).unwrap();
    let reader = repo(
        probe("workflow/delegate/child", "reader", 0),
        "/repo-a",
        b"new",
    );
    assert!(store.begin(&reader).unwrap().is_none());
}

#[test]
fn shell_braced_and_indirect_inputs_rerun_with_new_outputs_in_separate_processes() {
    for indirect in [false, true] {
        let f = Fixture::new();
        let mut recipe: Value =
            serde_yaml::from_str(&fs::read_to_string(&f.recipe).unwrap()).unwrap();
        recipe["context"]["input"] = json!("old");
        recipe["recovery"]["persist"]["context"]
            .as_array_mut()
            .unwrap()
            .push(json!("input"));
        let helper = f.root.join("helper");
        fs::write(
            &helper,
            "#!/bin/bash\nkey=RECIPE_VAR_input\nprintf '\"%s\"' \"${!key}\"\n",
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let read = if indirect {
            format!("'{}'", helper.display())
        } else {
            "printf '\"%s\"' \"${RECIPE_VAR_input:-}\"".into()
        };
        recipe["steps"][0]["command"] = json!(format!(
            "printf 'first\\n' >> '{}'; {read}",
            f.root.join("events").display()
        ));
        fs::write(&f.recipe, serde_yaml::to_string(&recipe).unwrap()).unwrap();
        fs::write(f.root.join("limit-third"), "").unwrap();
        let a = f.admit("a", false);
        assert_eq!(f.run(&a)["context"]["first"], "old");
        drop(a);
        fs::remove_file(f.root.join("limit-third")).unwrap();
        let b = f.admit("b", false);
        let output = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
            .arg(&f.recipe)
            .args(["--output-format", "json", "--no-update-check", "-C"])
            .arg(f.root.join("repo"))
            .args(["--set", "input=new", "--recovery-descriptor"])
            .arg(b.descriptor_path())
            .env_remove("AMPLIHACK_RECOVERY_ATTACHMENT")
            .output()
            .unwrap();
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result["success"],
            true,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(result["context"]["first"], "new");
        assert_eq!(
            f.events(),
            ["first", "second", "third", "first", "second", "third"]
        );
    }
}

#[test]
fn new_workflow_mutation_invalidates_unentered_inherited_children_before_baseline_advance() {
    let f = Fixture::new();
    let observed = |mut p: Probe, bytes: &[u8]| {
        p.resources.push(Observation {
            kind: "repository".into(),
            path: "/repo".into(),
            digest: digest(bytes),
            context_key: None,
            blob: None,
        });
        p
    };
    let a = f.admit("a", false);
    let mut store = Store::open(a.descriptor.clone()).unwrap();
    let child = observed(probe("workflow/delegate/child", "reader", 0), b"old");
    store.begin(&child).unwrap();
    store.commit(completion(child.clone())).unwrap();
    store.outcome(Outcome::UsageLimit).unwrap();
    drop(store);
    drop(a);
    let b = f.admit("b", false);
    let mut store = Store::open(b.descriptor.clone()).unwrap();
    let classify = observed(probe("workflow", "classify", 0), b"old");
    store.begin(&classify).unwrap();
    let mut changed = classify;
    changed.resources[0].digest = digest(b"new");
    store.commit(completion(changed)).unwrap();
    let mut child = child;
    child.resources[0].digest = digest(b"new");
    assert!(store.begin(&child).unwrap().is_none());
}
