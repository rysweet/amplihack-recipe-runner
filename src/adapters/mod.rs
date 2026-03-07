/// Adapter trait and implementations for recipe step execution.
///
/// Direct port from Python `amplihack.recipes.adapters`.
pub mod cli_subprocess;

/// Trait that all recipe execution adapters must implement.
pub trait Adapter {
    /// Execute an agent step and return the output.
    fn execute_agent_step(
        &self,
        prompt: &str,
        agent_name: Option<&str>,
        system_prompt: Option<&str>,
        mode: Option<&str>,
        working_dir: &str,
    ) -> Result<String, anyhow::Error>;

    /// Execute a bash step and return the output.
    fn execute_bash_step(
        &self,
        command: &str,
        working_dir: &str,
        timeout: Option<u64>,
    ) -> Result<String, anyhow::Error>;

    /// Check if the adapter is available.
    fn is_available(&self) -> bool;

    /// Return the adapter name.
    fn name(&self) -> &str;
}
