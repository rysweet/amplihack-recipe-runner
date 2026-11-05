use super::*;

// These variables are injected into a disposable test parent, avoiding any
// process-global mutation in the parallel test harness.
const AMBIENT_SENTINELS: [&str; 7] = [
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GH_TOKEN",
    "AMPLIHACK_AGENT_BINARY",
    "AMPLIHACK_BASH",
    "PYTHONPATH",
    "FIXTURE_AMBIENT_SENTINEL",
];

#[test]
fn ambient_credentials_and_overrides_do_not_reach_either_launch_path() {
    let mut probe = Command::new(std::env::current_exe().unwrap());
    probe.args([
        "--exact",
        "fixtures::environment_tests::environment_probe_worker",
        "--nocapture",
    ]);
    for name in AMBIENT_SENTINELS {
        probe.env(name, "harmless-fixture-sentinel");
    }
    let mut child = probe.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "isolated environment probe failed");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("isolated environment probe exceeded watchdog");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn environment_probe_worker() {
    if std::env::var("FIXTURE_AMBIENT_SENTINEL").as_deref() != Ok("harmless-fixture-sentinel") {
        return;
    }
    for name in AMBIENT_SENTINELS {
        assert_eq!(
            std::env::var(name).as_deref(),
            Ok("harmless-fixture-sentinel")
        );
    }
    let (_, result, record) = run("success", "synthetic environment probe", None, "codex");
    assert!(result.get("ok").is_some(), "{result}");
    assert_eq!(record["ambient_present"], serde_json::json!([]));
    super::super::cancellation::assert_runner_environment_is_private();
}
