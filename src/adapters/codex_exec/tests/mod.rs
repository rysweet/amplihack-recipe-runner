//! Private boundary contracts; compiled by the Codex facade on Unix.
mod cancellation;
mod coordinator;
mod diagnostics;
mod final_output;
#[cfg(target_os = "linux")]
mod launcher_retirement;
mod lifecycle_model;
mod process;
#[cfg(target_os = "linux")]
pub(super) mod retirement_fixtures;
#[cfg(target_os = "linux")]
mod retirement_signals;
mod runner_cleanup;

#[cfg(target_os = "linux")]
mod group_authority;
#[cfg(target_os = "linux")]
mod retirement_concurrency;

#[cfg(target_os = "linux")]
mod anchor_lifecycle;

#[cfg(target_os = "linux")]
mod anchor_descriptors;
#[cfg(target_os = "linux")]
mod retirement_failures;

#[cfg(target_os = "linux")]
pub(super) mod extended_fixtures;

mod launcher_policy;

#[cfg(target_os = "linux")]
mod first_launcher_fault;
