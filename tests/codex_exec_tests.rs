//! Deterministic product-boundary tests. Fixtures are not live Codex proof.
#![cfg(unix)]
use recipe_runner_rs::adapters::{Adapter, cli_subprocess::CLISubprocessAdapter};
use serde_json::json;
use std::fs;

#[path = "codex_exec/cancellation.rs"]
mod cancellation;
#[path = "codex_exec/final_output.rs"]
mod final_output;
#[path = "codex_exec/fixtures.rs"]
mod fixtures;
#[path = "codex_exec/lifecycle.rs"]
mod lifecycle;
#[path = "codex_exec/process_observation.rs"]
mod process_observation;
#[path = "codex_exec/transport.rs"]
mod transport;

// Run the adapter in a separate test process: no process-global environment races.
#[test]
fn adapter_worker() {
    let Ok(root) = std::env::var("CODEX_TEST_ROOT") else {
        return;
    };
    fn dispositions() -> [(usize, i32); 2] {
        [libc::SIGINT, libc::SIGTERM].map(|signal| unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut action), 0);
            // Linux libc adds its internal SA_RESTORER trampoline on install;
            // compare caller-visible disposition flags, not that ABI detail.
            #[cfg(target_os = "linux")]
            let flags = action.sa_flags & !0x04000000;
            #[cfg(not(target_os = "linux"))]
            let flags = action.sa_flags;
            (action.sa_sigaction, flags)
        })
    }
    for name in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GH_TOKEN",
        "AMPLIHACK_AGENT_BINARY",
        "AMPLIHACK_BASH",
        "PYTHONPATH",
        "FIXTURE_AMBIENT_SENTINEL",
    ] {
        assert!(
            std::env::var_os(name).is_none(),
            "ambient variable reached adapter worker: {name}"
        );
    }
    let prior = dispositions();
    let prompt = fs::read_to_string(format!("{root}/prompt")).unwrap();
    let observes_descendant = matches!(
        std::env::var("TEST_SCENARIO").unwrap().as_str(),
        "tree_timeout" | "tree_success" | "writer"
    );
    let result = CLISubprocessAdapter::new()
        .with_binary(&std::env::var("TEST_PROVIDER").unwrap())
        .execute_agent_step(
            &prompt,
            Some("persona"),
            Some("PERSONA_SENTINEL"),
            Some("autonomous"),
            &root,
            std::env::var("TEST_MODEL").ok().as_deref(),
            if std::env::var("TEST_SCENARIO").as_deref() == Ok("flood") {
                None
            } else {
                Some(
                    if std::env::var("TEST_SCENARIO").as_deref() == Ok("zero_timeout") {
                        0
                    } else {
                        1
                    },
                )
            },
        );
    // Inspect at adapter return, before observer joining or fallback cleanup.
    let observation = observes_descendant
        .then(|| process_observation::capture_return(std::path::Path::new(&root)));
    if let Some(observation) = observation {
        observation.expect("adapter-return descendant observation failed");
    }
    assert_eq!(
        dispositions(),
        prior,
        "signal dispositions must be restored"
    );
    let value = match result {
        Ok(s) => json!({"ok":s}),
        Err(e) => json!({"error":format!("{e:#}")}),
    };
    fs::write(format!("{root}/result.json"), value.to_string()).unwrap();
}
