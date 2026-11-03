//! The release workflow builds each archive reproducibly and attests its
//! provenance: `cargo build --locked` on an exact toolchain, and an
//! attestation step, pinned to a commit, for every archive the build job makes.
use serde_yaml::Value;

fn workflow() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/.github/workflows/release.yml");
    serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn build_steps(wf: &Value) -> &Vec<Value> {
    wf["jobs"]["build"]["steps"].as_sequence().unwrap()
}

fn step_index(steps: &[Value], pred: impl Fn(&Value) -> bool) -> usize {
    steps
        .iter()
        .position(pred)
        .expect("expected step is missing from the build job")
}

#[test]
fn release_build_uses_the_lockfile() {
    let wf = workflow();
    let build = build_steps(&wf)
        .iter()
        .find(|s| text(s, "run").contains("cargo build"))
        .expect("build job has a cargo build step");
    let run = text(build, "run");
    assert!(run.contains("--release"), "{run}");
    assert!(
        run.contains("--locked"),
        "cargo build must use --locked: {run}"
    );
}

#[test]
fn release_build_installs_an_exact_toolchain() {
    let wf = workflow();
    let pinned = text(&wf["env"], "RELEASE_RUST_TOOLCHAIN");
    let parts: Vec<&str> = pinned.split('.').collect();
    assert!(
        parts.len() == 3 && parts.iter().all(|p| p.parse::<u32>().is_ok()),
        "RELEASE_RUST_TOOLCHAIN must be an exact x.y.z version, got {pinned:?}"
    );
    let install = build_steps(&wf)
        .iter()
        .find(|s| text(s, "uses").starts_with("dtolnay/rust-toolchain@"))
        .expect("build job installs a toolchain");
    assert_eq!(
        text(&install["with"], "toolchain"),
        "${{ env.RELEASE_RUST_TOOLCHAIN }}"
    );
}

#[test]
fn every_archive_is_attested_before_it_is_uploaded() {
    let wf = workflow();
    let steps = build_steps(&wf);
    let package = step_index(steps, |s| text(s, "name") == "Package");
    let attest = step_index(steps, |s| {
        text(s, "uses").starts_with("actions/attest-build-provenance@")
    });
    let upload = step_index(steps, |s| {
        text(s, "uses").starts_with("actions/upload-artifact@")
    });
    assert!(
        package < attest && attest < upload,
        "order: package, attest, upload"
    );

    let uses = text(&steps[attest], "uses");
    let sha = uses
        .split('@')
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap();
    assert!(
        sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()),
        "the attestation action must be pinned to a commit: {uses}"
    );
    let subject = text(&steps[attest]["with"], "subject-path");
    assert_eq!(subject, "recipe-runner-rs-${{ matrix.target }}.tar.gz");
    assert!(
        text(&steps[package], "run").contains(subject),
        "the attested path must be the archive the package step creates"
    );
}

#[test]
fn signing_permissions_are_on_the_build_job_only() {
    let wf = workflow();
    let top = &wf["permissions"];
    assert_eq!(text(top, "contents"), "read");
    assert!(top.get("id-token").is_none() && top.get("attestations").is_none());

    let build = &wf["jobs"]["build"]["permissions"];
    assert_eq!(text(build, "contents"), "read");
    assert_eq!(text(build, "id-token"), "write");
    assert_eq!(text(build, "attestations"), "write");

    for (name, job) in wf["jobs"].as_mapping().unwrap() {
        if name.as_str() != Some("build") {
            assert!(
                job["permissions"].get("id-token").is_none(),
                "only the build job may request an OIDC token: {name:?}"
            );
        }
    }
}
