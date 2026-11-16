/// Adapter trait and implementations for recipe step execution.
///
///
pub mod cli_subprocess;
mod codex_exec;
mod env_spill;
#[cfg(test)]
mod test_log;
pub(crate) use codex_exec::{CleanupFailure, Interruption};

use crate::adapters::env_spill::{MAX_ARG_STRLEN, env_pair_bytes};
use crate::models::StepContextMode;
use std::fmt;

/// Names the step that an agent launch is acting for.
///
/// Set on every agent launch, with one exception: an id that cannot be placed
/// in an environment at all — one holding a control character, or one too
/// large for the kernel's per-string limit — is omitted, and the step runs
/// without it. A consumer should therefore treat this as usually present
/// rather than guaranteed.
///
/// The value is recipe-controlled text crossing into another program. A
/// consumer must treat it as untrusted: never interpolate it unquoted into a
/// shell, and never use it as an unsanitised path component — `../` is a legal
/// step id.
pub const STEP_ID_ENV: &str = "AMPLIHACK_STEP_ID";

/// Declares that the step asked to start with only its prompt and inputs.
///
/// Set only when the step declared `context_mode: minimal`, and then only to
/// [`MINIMAL_CONTEXT`]. Absence is the normal start. The runner implements no
/// part of the minimal start itself; this variable is the whole declaration.
///
/// Note the deliberate difference in names: the recipe field is `context_mode`
/// (because `context:` is already a step field, the sub-recipe input map),
/// while the launcher-facing variable keeps the contract name agreed with
/// amplihack-rs.
///
/// Presence means *less context*, never *more access*: a consumer must not
/// read it as permission to do anything it would not otherwise do.
pub const STEP_CONTEXT_ENV: &str = "AMPLIHACK_STEP_CONTEXT";

/// Carries the step's rendered `effort`, when it rendered to something.
///
/// The runner holds no effort vocabulary; the accepted words belong to the
/// harness that reads this.
pub const STEP_EFFORT_ENV: &str = "AMPLIHACK_STEP_EFFORT";

/// The one value [`STEP_CONTEXT_ENV`] ever takes.
pub const MINIMAL_CONTEXT: &str = StepContextMode::MINIMAL;

/// Every environment variable this contract owns.
///
/// Its consumer is the removal loop in
/// [`cli_subprocess::CLISubprocessAdapter::build_child_env_with`], which strips
/// all of these from the inherited environment of every child — agent and bash
/// alike — before deciding what to set. That removal is a security control: a
/// process that can set one of these variables once would otherwise silently
/// degrade every nested agent step of every recipe beneath it.
///
/// A new contract variable must be added here first, or it reopens that hole.
/// Kept a slice rather than a fixed-size array so the count never leaks into a
/// consumer's type.
pub const CONTRACT_ENV_NAMES: &[&str] = &[STEP_ID_ENV, STEP_CONTEXT_ENV, STEP_EFFORT_ENV];

/// Everything an adapter needs in order to launch one agent step.
///
/// Values are resolved: `model` and `effort` have been rendered against the
/// recipe context, trimmed, and checked for a shape that can safely reach an
/// argument vector and an environment. An absent value means the launcher's
/// own default stands.
///
/// `#[non_exhaustive]` so that a later contract field is an additive change for
/// code outside this crate.
#[non_exhaustive]
pub struct AgentLaunch {
    /// The step's rendered prompt.
    pub prompt: String,
    /// The `agent:` reference, when the step named one.
    pub agent_name: Option<String>,
    /// The resolved agent definition, when one was found.
    pub system_prompt: Option<String>,
    /// The step's `mode:`.
    pub mode: Option<String>,
    /// The resolved working directory for the agent process.
    pub working_dir: String,
    /// The rendered model, travelling to the launcher as an argument.
    pub model: Option<String>,
    /// The rendered effort, travelling on the environment.
    pub effort: Option<String>,
    /// The declared context mode, when the step declared one.
    pub context_mode: Option<StepContextMode>,
    /// The id of the step this launch acts for.
    pub step_id: String,
    /// The step's timeout in seconds, when it set one.
    pub timeout: Option<u64>,
}

impl AgentLaunch {
    /// The contract variables this launch sets on its child, in a fixed order.
    ///
    /// This is the whole policy: what is set, when, and with which value. The
    /// model is deliberately absent — it reaches the launcher as a `--model`
    /// argument and a second route would be a second thing to keep in step.
    pub fn contract_env(&self) -> Vec<(&'static str, &str)> {
        let mut pairs = Vec::with_capacity(CONTRACT_ENV_NAMES.len());
        // Two ways an id cannot be placed in the environment: a control
        // character, which cannot be carried safely, and a `KEY=value\0`
        // string over the kernel's per-string limit, which would fail the
        // whole `execve` with an opaque `E2BIG`. The length predicate is the
        // crate's existing one (`inherited_env_entry_too_large` applies it to
        // inherited variables at the same threshold), so there is one rule
        // here, not two. The parser caps no step id, deliberately; this is
        // where an id too long to travel costs one variable, not the run.
        let id_is_unusable = self.step_id.chars().any(char::is_control)
            || env_pair_bytes(STEP_ID_ENV, &self.step_id) > MAX_ARG_STRLEN;
        if id_is_unusable {
            // Omitting one variable is better than failing a valid step. The
            // log names the step and reports its length. The name is escaped,
            // because an id can carry a terminal escape sequence, and bounded,
            // because the length is exactly what can be wrong with it — an
            // unbounded identifier here would put the whole oversized id in
            // the log line that exists to report that it is oversized.
            log::debug!(
                "AgentLaunch::contract_env: omitting {STEP_ID_ENV}; step id '{}' ({} bytes) \
                 cannot be placed in an environment",
                crate::models::diagnostic_identifier(&self.step_id[..self.id_log_prefix_len()]),
                self.step_id.len()
            );
        } else {
            pairs.push((STEP_ID_ENV, self.step_id.as_str()));
        }
        if let Some(mode) = self.context_mode {
            pairs.push((STEP_CONTEXT_ENV, mode.as_str()));
        }
        if let Some(effort) = self.effort.as_deref() {
            pairs.push((STEP_EFFORT_ENV, effort));
        }
        pairs
    }

    /// Byte length of the longest prefix of `step_id` safe to put in a log
    /// line, rounded down to a character boundary.
    ///
    /// A normal id is far shorter than this and is logged whole.
    fn id_log_prefix_len(&self) -> usize {
        /// Enough to recognise any id a recipe would plausibly write.
        const MAX_LOGGED_ID_BYTES: usize = 128;

        if self.step_id.len() <= MAX_LOGGED_ID_BYTES {
            return self.step_id.len();
        }
        (0..=MAX_LOGGED_ID_BYTES)
            .rev()
            .find(|&n| self.step_id.is_char_boundary(n))
            .unwrap_or(0)
    }
}

impl fmt::Debug for AgentLaunch {
    /// Reports sizes and presence, never the values themselves.
    ///
    /// A `Debug` of a launch can reach a log line, an error chain and a CI log,
    /// and every field here that a recipe can template is fed from the recipe
    /// context — which carries earlier step output, agent output and
    /// `--context-file` values. The prompt and system prompt can hold a
    /// client's private instructions; the working directory, model and effort
    /// are rendered, so each can hold a context value that the recipe author
    /// never meant to publish.
    ///
    /// Only the step id is shown as written: it is the recipe's own label for
    /// the step, and a diagnostic that cannot say which step it is about is
    /// not worth logging.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentLaunch")
            .field("step_id", &self.step_id)
            .field("prompt_len", &self.prompt.len())
            .field("has_system_prompt", &self.system_prompt.is_some())
            .field("agent_name", &self.agent_name)
            .field("mode", &self.mode)
            .field("working_dir_len", &self.working_dir.len())
            .field("has_model", &self.model.is_some())
            .field("has_effort", &self.effort.is_some())
            .field("context_mode", &self.context_mode)
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Trait that all recipe execution adapters must implement.
///
/// Adapters must be `Sync` to support parallel step execution via scoped threads.
pub trait Adapter: Sync {
    /// Execute an agent step and return the output.
    ///
    /// The optional `timeout` (in seconds) kills the agent process if it exceeds
    /// the duration. Without a timeout, agent steps run until completion.
    ///
    /// This is the forwarding path for [`Adapter::execute_agent_launch`]'s
    /// default body, and it drops the per-step contract fields — effort, the
    /// context mode and the step id — because its signature cannot carry them.
    /// The runner calls `execute_agent_launch`; an adapter that wants the
    /// contract overrides that method instead of this one. The two collapse
    /// into one signature at the next compatibility break.
    #[allow(clippy::too_many_arguments)]
    fn execute_agent_step(
        &self,
        prompt: &str,
        agent_name: Option<&str>,
        system_prompt: Option<&str>,
        mode: Option<&str>,
        working_dir: &str,
        model: Option<&str>,
        timeout: Option<u64>,
    ) -> Result<String, anyhow::Error>;

    /// Execute an agent step described by a resolved [`AgentLaunch`].
    ///
    /// The default body forwards to [`Adapter::execute_agent_step`], so every
    /// adapter written before the per-step launch contract keeps compiling and
    /// behaving exactly as it did — it simply sets no contract variables.
    fn execute_agent_launch(&self, launch: &AgentLaunch) -> Result<String, anyhow::Error> {
        log::debug!("Adapter::execute_agent_launch: forwarding {launch:?}");
        self.execute_agent_step(
            &launch.prompt,
            launch.agent_name.as_deref(),
            launch.system_prompt.as_deref(),
            launch.mode.as_deref(),
            &launch.working_dir,
            launch.model.as_deref(),
            launch.timeout,
        )
    }

    /// Execute a bash step and return the output.
    ///
    /// The optional `timeout` (in seconds) kills the process after the given duration.
    /// `extra_env` contains `RECIPE_VAR_*` environment variables — context values
    /// injected via the process environment instead of shell text substitution.
    fn execute_bash_step(
        &self,
        command: &str,
        working_dir: &str,
        timeout: Option<u64>,
        extra_env: &std::collections::HashMap<String, String>,
    ) -> Result<String, anyhow::Error>;

    /// Check if the adapter is available.
    fn is_available(&self) -> bool;

    /// Return the adapter name.
    fn name(&self) -> &str;
}
