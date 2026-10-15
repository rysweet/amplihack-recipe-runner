//! Isolated workers for implementation-specific proof obligations.
pub(in crate::adapters::codex_exec) fn run(case: &str) {
    super::retirement_fixtures::run_case(
        case,
        "adapters::codex_exec::tests::extended_fixtures::lifecycle_worker",
    );
}
#[test]
fn lifecycle_worker() {
    let Ok(case) = std::env::var("LIFECYCLE_CASE") else {
        return;
    };
    if case.starts_with("observation_") {
        super::super::signal_observations::tests::worker(&case);
    } else if case.starts_with("descriptors_") {
        super::anchor_descriptors::worker();
    } else if case.starts_with("fault_") {
        super::retirement_failures::worker(&case);
    } else if case.starts_with("protocol_") || case.starts_with("state_") {
        super::super::group_anchor::tests::worker(&case);
    } else {
        panic!("unknown extended worker case {case}");
    }
}
