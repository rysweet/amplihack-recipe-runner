---
title: Codex agent steps
description: Codex execution, configuration, results, and terminal failure contracts.
doc_type: reference
last_updated: 2026-10-07
---

# Codex agent steps

The runner executes Codex agent steps through Amplihack using modern noninteractive `codex exec`. Claude remains the default provider; selecting Codex leaves Claude and Copilot invocation and result contracts unchanged.

## Contents

- [Run a recipe](#run-a-recipe)
- [Provider and configuration](#provider-and-configuration)
- [Instructions, results, and failures](#instructions-results-and-failures)
- [Cancellation and ownership](#cancellation-and-ownership)
- [Terminal cleanup failures](#terminal-cleanup-failures)
- [Compatibility probe API](#compatibility-probe-api)
- [Rust adapter API](#rust-adapter-api)

## Run a recipe

Install the runner, a production Amplihack launcher with Codex exec stdin support, and Codex CLI. Configure authentication using Codex's native configuration. The runner uses that existing authentication; it does not provision credentials or change Codex configuration. Codex CLI 0.160.0 is the compatibility baseline.

Save this as `codex-summary.yaml`:

```yaml
name: codex-summary
version: "1.0"
context:
  subject: "the purpose of this repository"
steps:
  - id: summarize
    type: agent
    prompt: |
      Explain {{subject}} in three sentences.
      Return only the explanation.
    timeout: 120
    auto_stage: false
    output: summary
```

Validate, then run in the repository you want Codex to inspect:

```bash
recipe-runner-rs codex-summary.yaml --validate-only
# Output: ✓ Recipe 'codex-summary' is valid (1 steps)
recipe-runner-rs codex-summary.yaml --agent-binary codex -C . --progress
```

The final response is stored in `summary`. `--output-format json` serializes the recipe result; it does not enable Codex JSON streaming. Progress and diagnostics are separate from the stored response.

To apply a packaged persona, add an existing resolvable agent reference, for example `agent: "amplihack:core:architect"`. Resolved persona text joins the effective system instructions. Existing agent-resolution warnings still apply when a reference cannot be resolved.

## Provider and configuration

`--agent-binary codex` takes precedence over `AMPLIHACK_AGENT_BINARY=codex`. With neither set, the runner uses Claude. Provider selection is separate from the recipe's `agent` persona reference.

The runner launches this argument layout, with an optional explicit model:

```text
amplihack codex -- exec --output-last-message UNIQUE_FINAL_PATH [--model EXPLICIT_MODEL] -
```

The task is delivered through stdin. Codex `-p` selects a profile and is not used for prompt delivery. The child runs in the effective step working directory, falling back to the recipe working directory. The runner adds no implicit `--add-dir` scope.

Omitting the recipe step's `model` leaves model selection to native Codex configuration. If supplied, the value is forwarded as an explicit `--model` value; choose an identifier supported by your Codex setup. The runner never selects `auto` for Codex, including when `AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL` is enabled. Claude/Copilot retain their existing opt-in fallback behavior.

Temporary resources use portable tempfile handling and the platform's temporary-directory configuration, including `TMPDIR` on Unix. To use a chosen writable runtime directory:

```bash
runtime_dir=$(mktemp -d)
TMPDIR="$runtime_dir" recipe-runner-rs codex-summary.yaml --agent-binary codex
rmdir "$runtime_dir"
```

The command creates an owner-only runtime directory and removes it after successful attempt-resource cleanup. Each attempt owns a unique private directory and a final pathname that is absent before launch. On Unix, attempt directories are owner-only (`0700`), with owner-only final-file access (`0600`). Resources remain owned through process completion and extraction, then are removed. Allocation and cleanup failures are reported.

## Instructions, results, and failures

Every prompt size uses the same deterministic UTF-8 stdin envelope: effective system instructions, resolved persona, recipe leaf/no-reentry instructions, task, and autonomy footer. The full envelope is written and stdin closed. Leading dashes, newlines, Unicode, and large prompts do not change transport. Instructions are not placed in `AGENTS.md`.

Successful raw output is only the content of the final-message file. Whitespace, trailing newlines, and empty content are preserved verbatim. An existing zero-byte file is a valid empty response; a missing file is an error. Progress stdout/stderr never substitutes for a final response. Explicit recipe JSON parsing retains its documented extraction/transformation semantics.

The existing `MAX_STEP_OUTPUT_BYTES` limit is 10,000,000 bytes. Codex final output at or below the limit is preserved; larger output fails explicitly before unbounded allocation. Successful Codex output is never silently truncated. There is no runtime output-limit setting.

Spawn errors, incomplete or failed stdin writes, timeout, nonzero exit, missing or nonregular final files, symlinks, unsafe permissions, read failures, invalid UTF-8, and cleanup failures fail the attempt. A final file does not turn a nonzero exit or incomplete instruction delivery into success. Stdout and stderr are drained concurrently, retaining the latest 64 KiB per stream in memory while continuing to drain. After shutdown, each reader drains queued bytes until EOF or the pipe would block, bounded by 1 MiB of additional reads or 50 ms, so detached writers cannot prevent completion. No diagnostic spool files are created. Reader failures fail the attempt.

A step's `timeout` applies separately to each attempt. Its absolute deadline starts immediately after successful launcher spawn and covers group-anchor establishment, reader setup, stdin delivery and execution. `timeout: 0` expires immediately; omission removes the execution deadline while retaining bounded helper startup and cleanup. Synchronous OS spawn is outside this polling deadline.

Cleanup stops stdin delivery, retires the owned launcher within two seconds, and terminates the original group while a private anchor retains its identity. Group TERM has at most 100 ms grace; the final group KILL is attempted even after TERM or probe errors when authority remains valid. Signaling is then sealed. Helper retirement and observational confirmation share a two-second deadline measured from completion of that KILL attempt. Process polling uses at most 4.1 seconds, plus the existing reader and resource cleanup bounds. Failed signals or reaping remain errors even after eventual absence. See the [lifecycle reference](codex-lifecycle.md) for startup, ownership assumptions and failure paths.

Linux process metadata distinguishes zombies from live members; other Unix platforms require group absence. A proven transient pre-reap zombie-only observation resolved by bounded successful reaping and final absence does not add a false terminal cleanup error. EPERM or final absence alone cannot establish that condition. Genuine permission, signal, probe, reap, reader, resource and surviving-descendant failures remain terminal. Unix process groups contain ordinary inherited descendants; deliberately detached sessions are outside that containment. Codex execution is Unix-only; non-Unix execution fails explicitly. Unix capability advertisement does not establish native runtime validation on every Unix platform.

Bounded rate-limit retries and JSON repair retries retain the effective instructions, task, persona, no-reentry context, provenance, working directory, explicit model, and timeout. A JSON repair attempt adds repair instructions. Every attempt gets fresh private output resources. Backoff waits and repeated attempts increase total step duration beyond a single timeout. Retries can repeat tool calls and other side effects; use idempotent operations where possible. Stdin, file, decoding, and cleanup errors are not made retryable merely by rate-limit text in diagnostics.

The runner has no generic Codex argument passthrough API. Use recipe fields for task, model, working directory, and timeout.

## Cancellation and ownership

SIGINT and SIGTERM observed during active Codex ownership stop stdin delivery and use bounded process, reader and resource cleanup. Cancellation remains effective during last-owner signal restoration, including SIGTERM delivered while its product handler remains installed after SIGINT restoration. The adapter captures the immutable interruption and restoration result under the ownership mutex before an independent owner can begin a new epoch.

Concurrent calls share signal ownership and the same observation baseline. Closing a nonlast owner samples cancellation without restoring handlers. The last owner attempts restoration of both complete prior dispositions. Registration, rollback or restoration failures retain diagnostics and pending restoration metadata; a later installation reconciles them within bounds or refuses before spawning.

The guarantee starts at the handler's observable atomic entry publication. A published event remains observable even if that handler is still in flight. Persistent per-signal observations are never reset. A kernel-selected handler that has not reached publication at the retirement snapshot lies outside this guarantee; atomics, the mutex and `sigaction` do not establish a universal kernel dispatch fence.

Cancellation stops the entire recipe, including nested recovery and pending parallel steps, even with `continue_on_error: true` or `fatal: false`. JSON repair cannot accept degraded success after cancellation. Already admitted work is joined under its existing execution contract. Ordinary Claude/Copilot failures retain their existing policies. No recipe field configures the private anchor, observation counters, handler retirement or cleanup budgets.

## Terminal cleanup failures

Cleanup failure stops the entire recipe, even without SIGINT or SIGTERM. Process
shutdown, diagnostic-reader completion, and owned-resource deletion failures
remain terminal through agent calls, Bash calls, lifecycle hooks, nested recipes,
recovery, parallel execution, and JSON repair.

| Boundary or setting | Behavior after cleanup failure |
|---|---|
| `continue_on_error: true` or `fatal: false` | The step and recipe fail; later steps do not run. |
| Nested recipes with `recovery_on_failure: true` | Child failure propagates through every enclosing recipe; recovery and later steps do not run. |
| Recovery already in progress | Cleanup failure in recovery stops enclosing recovery and continuation. |
| `parse_json: true` | Cleanup failure in the primary call prevents JSON repair. Cleanup failure during repair prevents further repair or raw-output fallback, whether `parse_json_required` is `true` or `false`. |
| `pre_step` hook | Cleanup failure blocks the primary step and subsequent hooks. |
| `post_step` hook | Cleanup failure makes the step and recipe fail, including after the last primary step succeeds. |
| `on_error` hook | Cleanup failure stops further hooks, recovery, and continuation. |
| Parallel group | A worker publishes terminal failure when it classifies the error, before joins. Pending agent, Bash, recipe, hook, recovery, and repair dispatch stops; all workers already started are joined before return. |

Already admitted work may finish. A parallel failure remains associated with its
original step, and successful siblings cannot turn the recipe into success.
Step results, listener notifications, audit entries, and checkpoints reflect the
effective failure, including failure in a final post-hook. Terminal state persists
through nesting and aggregation and resets only for an independent top-level
execution using the runner again.

Ordinary errors retain their existing policies: nonfatal continuation, nested
recovery, optional JSON degradation, and warning-only hook failures still apply.
Terminality comes from typed errors, not diagnostic text containing words such as
"cleanup" or "interrupted". There is no configuration switch to bypass it.

For example, a nested Codex step whose final message is valid but whose owned
resource deletion fails causes its parent recipe to fail. Setting
`recovery_on_failure: true` and `continue_on_error: true` on the parent step does
not launch recovery or the next step. An ordinary child error remains eligible
for the configured recovery policy.

Failure diagnostics retain the unsuccessful child exit status, safe failure
classification, and cleanup details. If interruption and cleanup failure occur
together, interruption takes precedence while cleanup diagnostics remain visible.
Both `Interruption` and `CleanupFailure` remain discoverable through typed error
composition, together with the primary ordinary error or unsuccessful exit status.
A cleanup error does not establish that every owned resource was removed.
See [Codex verification](testing-recipes.md#codex-verification) for regression
coverage of these boundaries.

## Compatibility probe API

```bash
recipe-runner-rs --capabilities
```

The standalone probe exits zero and writes exactly one JSON object, optionally followed by a newline, with empty stderr. For a Unix build with package version 0.4.1:

```json
{"capabilities":["codex_exec"],"schema_version":1,"version":"0.4.1"}
```

`version` is the compiled Cargo package version. The producer emits exactly these three keys. Unix builds advertise `codex_exec`; non-Unix builds emit an empty capability list because execution is unsupported. The probe runs before logging initialization, update checks, update-cache writes, network access, or agent startup, regardless of ambient logging/update settings. Mixed recipe/subcommand/option invocations with `--capabilities` are rejected before execution or update effects. Failed stdout writes report failure.

Consumers require numeric `schema_version: 1` and membership of `codex_exec`. Compatible newer/custom builds may pass; an exact source SHA is a separate delivery pin, not part of this JSON. `codex_exec` promises full stdin instructions, final-message-only results, explicit failure propagation, and no implicit model.

A stale runner that rejects the probe is incompatible. Managed Amplihack installations can upgrade to their corrected immutable runner revision and re-probe. An incompatible explicit `RECIPE_RUNNER_RS_PATH` override must be replaced or unset by the operator; it is not silently replaced. Check the runner capability and launcher exec support separately.

## Rust adapter API

The public `Adapter: Sync` interface and `StepResult` types are unchanged. `execute_agent_step` accepts prompt, optional agent name, effective system prompt, mode, working directory, optional model, and optional timeout, returning `Result<String, anyhow::Error>`.

For a Codex `CLISubprocessAdapter`, `Ok(String)` contains the verbatim final message within the output bound, including `Ok(String::new())` for an intentionally empty file. Execution/resource failures return contextual `Err`; ordinary failures follow existing failure and `continue_on_error` policies. SIGINT/SIGTERM cancellation and cleanup failure are terminal and stop the recipe regardless of those policies, including during nested recovery. Optional JSON parsing happens after successful raw extraction. No new public error enum, output-path parameter, or JSONL protocol is required.

The runner classifies private typed `CleanupFailure` and `Interruption` markers
in the error chain before converting an error to text. Classification survives
execution boundaries without changing the public result schema. Cleanup failures
never enter rate-limit retries; the [terminal cleanup contract](#terminal-cleanup-failures)
also governs recovery, nonfatal policies, hooks, and JSON repair.
