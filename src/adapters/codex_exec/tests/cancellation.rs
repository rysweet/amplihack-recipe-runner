use super::super::{cancellation::Interruption, finish_resources};

#[test]
fn cleanup_aggregation_preserves_typed_interruption() {
    for (execution, cleanup) in [
        (
            Err(Interruption { signal: 15 }.into()),
            Err(anyhow::anyhow!("delete failed")),
        ),
        (
            Err(anyhow::anyhow!("execution failed")),
            Err(Interruption { signal: 2 }.into()),
        ),
    ] {
        let error = finish_resources(execution, cleanup).unwrap_err();
        assert!(error.downcast_ref::<Interruption>().is_some());
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("cancelled by signal"));
        assert!(diagnostic.contains("failed"));
    }
}

#[test]
fn interruption_survives_shutdown_and_resource_failures_for_both_signals() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let interrupted = Err(Interruption { signal }.into());
        let shutdown = Err(anyhow::anyhow!("group kill failed; reader join failed"));
        let result = finish_resources(interrupted, shutdown);
        let error =
            finish_resources(result, Err(anyhow::anyhow!("resource deletion failed"))).unwrap_err();
        assert_eq!(error.downcast_ref::<Interruption>().unwrap().signal, signal);
        let message = format!("{error:#}");
        for detail in [
            "group kill failed",
            "reader join failed",
            "resource deletion failed",
        ] {
            assert!(message.contains(detail), "lost cleanup detail: {detail}");
        }
    }
}
