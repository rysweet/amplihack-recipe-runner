//! Genuine faults remain terminal even if a final group observation is absent.
use super::super::{CleanupFailure, Interruption, finish_resources, process::shutdown};
use std::cell::Cell;

fn fault(kind: &str) {
    let observations = Cell::new(0);
    let mut failures = Vec::new();
    shutdown(
        |signal| {
            if (kind == "term" && signal == libc::SIGTERM)
                || (kind == "kill" && signal == libc::SIGKILL)
            {
                anyhow::bail!("genuine {kind} permission failure");
            }
            Ok(())
        },
        || {
            let n = observations.get();
            observations.set(n + 1);
            if kind == "probe" && n == 0 {
                anyhow::bail!("genuine probe failure");
            }
            Ok(kind == "descendant" || (kind == "owned-group" && n >= 2))
        },
        || {
            if kind == "reap" {
                anyhow::bail!("genuine reap failure");
            }
            Ok(())
        },
        &mut failures,
    );
    let error = finish_resources(
        Err(anyhow::anyhow!("primary ordinary failure")),
        Err(anyhow::anyhow!(failures.join("; "))),
    )
    .unwrap_err();
    assert!(error.downcast_ref::<CleanupFailure>().is_some());
    assert!(error.downcast_ref::<Interruption>().is_none());
    assert!(format!("{error:#}").contains("primary ordinary failure"));
    if matches!(kind, "descendant" | "owned-group") {
        assert!(format!("{error:#}").contains("group remains live"));
    } else {
        assert!(format!("{error:#}").contains(&format!("genuine {kind}")));
    }
    assert!(!super::super::retryable(&error));
}
#[test]
fn genuine_term_error_survives_absence() {
    fault("term");
}
#[test]
fn genuine_kill_error_survives_absence() {
    fault("kill");
}
#[test]
fn genuine_probe_error_survives_absence() {
    fault("probe");
}
#[test]
fn genuine_reap_error_survives_absence() {
    fault("reap");
}
#[test]
fn genuine_descendant_error_retains_terminal_marker() {
    fault("descendant");
}
#[test]
fn genuine_owned_group_error_retains_terminal_marker() {
    fault("owned-group");
}
