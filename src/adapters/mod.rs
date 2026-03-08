/// Adapter trait and implementations for recipe step execution.
///
/// Direct port from Python `amplihack.recipes.adapters`.
pub mod cli_subprocess;

/// Trait that all recipe execution adapters must implement.
///
/// Adapters must be `Sync` to support parallel step execution via scoped threads.
pub trait Adapter: Sync {
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

/// An adapter that tries a primary adapter and falls back to a secondary on failure.
pub struct FallbackAdapter<P: Adapter, S: Adapter> {
    primary: P,
    secondary: S,
}

impl<P: Adapter, S: Adapter> FallbackAdapter<P, S> {
    pub fn new(primary: P, secondary: S) -> Self {
        Self { primary, secondary }
    }
}

impl<P: Adapter, S: Adapter> Adapter for FallbackAdapter<P, S> {
    fn execute_agent_step(
        &self, prompt: &str, agent_name: Option<&str>,
        system_prompt: Option<&str>, mode: Option<&str>, working_dir: &str,
    ) -> Result<String, anyhow::Error> {
        match self.primary.execute_agent_step(prompt, agent_name, system_prompt, mode, working_dir) {
            Ok(output) => Ok(output),
            Err(primary_err) => {
                log::warn!(
                    "Primary adapter '{}' failed: {}, trying fallback '{}'",
                    self.primary.name(), primary_err, self.secondary.name()
                );
                self.secondary.execute_agent_step(prompt, agent_name, system_prompt, mode, working_dir)
            }
        }
    }

    fn execute_bash_step(
        &self, command: &str, working_dir: &str, timeout: Option<u64>,
    ) -> Result<String, anyhow::Error> {
        match self.primary.execute_bash_step(command, working_dir, timeout) {
            Ok(output) => Ok(output),
            Err(primary_err) => {
                log::warn!(
                    "Primary adapter '{}' failed: {}, trying fallback '{}'",
                    self.primary.name(), primary_err, self.secondary.name()
                );
                self.secondary.execute_bash_step(command, working_dir, timeout)
            }
        }
    }

    fn is_available(&self) -> bool {
        self.primary.is_available() || self.secondary.is_available()
    }

    fn name(&self) -> &str {
        "fallback"
    }
}
