# Codex agent steps

The runner executes Codex agent steps through Amplihack using modern noninteractive `codex exec`. Claude remains the default provider; selecting Codex leaves Claude and Copilot invocation and result contracts unchanged.

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
recipe-runner-rs codex-summary.yaml --agent-binary codex -C /path/to/repository --progress
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
TMPDIR=/path/to/private-runtime recipe-runner-rs codex-summary.yaml --agent-binary codex
```

Create that directory before running. Production has no hardcoded `/d0` path. Each attempt owns a unique private directory and a final pathname that is absent before launch. On Unix, attempt directories are owner-only (`0700`), with owner-only final-file access (`0600`). Resources remain owned through process completion and extraction, then are removed. Allocation and cleanup failures are reported.

## Instructions, results, and failures

Every prompt size uses the same deterministic UTF-8 stdin envelope: effective system instructions, resolved persona, recipe leaf/no-reentry instructions, task, and autonomy footer. The full envelope is written and stdin closed. Leading dashes, newlines, Unicode, and large prompts do not change transport. Instructions are not placed in `AGENTS.md`.

Successful raw output is only the content of the final-message file. Whitespace, trailing newlines, and empty content are preserved verbatim. An existing zero-byte file is a valid empty response; a missing file is an error. Progress stdout/stderr never substitutes for a final response. Explicit recipe JSON parsing retains its documented extraction/transformation semantics.

The existing `MAX_STEP_OUTPUT_BYTES` limit is 10,000,000 bytes. Codex final output at or below the limit is preserved; larger output fails explicitly before unbounded allocation. Successful Codex output is never silently truncated. There is no runtime output-limit setting.

Spawn errors, incomplete or failed stdin writes, timeout, nonzero exit, missing or nonregular final files, symlinks, unsafe permissions, read failures, invalid UTF-8, and cleanup failures fail the attempt. A final file does not turn a nonzero exit or incomplete instruction delivery into success. Stdout and stderr are drained concurrently, retaining the latest 64 KiB per stream in memory while continuing to drain. After shutdown, each reader drains queued bytes until EOF or the pipe would block, bounded by 1 MiB of additional reads or 50 ms, so detached writers cannot prevent completion. No diagnostic spool files are created. Reader failures fail the attempt.

A step's `timeout` applies separately to each attempt and covers stdin delivery and execution. `timeout: 0` expires immediately; omission means no execution deadline. Timeout cleanup terminates and reaps the owned launcher/Codex process tree and finishes I/O cleanup before returning. Cleanup attempts TERM, observes a bounded 100 ms grace period, attempts KILL, confirms shutdown within two seconds, polls launcher reaping for at most two additional seconds (including direct-kill fallback), and joins diagnostic readers before extraction. A failed direct kill followed by an unreaped launcher reports both errors rather than waiting indefinitely. Failures are aggregated and prevent extraction. Linux process metadata distinguishes zombies from live members; other Unix platforms require group absence. Unix process groups contain ordinary inherited descendants; deliberately detached sessions are outside that containment. Codex execution is supported only on Unix; all non-Unix execution fails explicitly. SIGINT and SIGTERM during active Codex work stop stdin delivery and use the same bounded process, I/O and resource cleanup before returning a nonretryable cancellation error. Prior signal dispositions are restored after active Codex calls finish.

Bounded rate-limit retries and JSON repair retries retain the effective instructions, task, persona, no-reentry context, provenance, working directory, explicit model, and timeout. A JSON repair attempt adds repair instructions. Every attempt gets fresh private output resources. Backoff waits and repeated attempts increase total step duration beyond a single timeout. Retries can repeat tool calls and other side effects; use idempotent operations where possible. Stdin, file, decoding, and cleanup errors are not made retryable merely by rate-limit text in diagnostics.

The runner has no generic Codex argument passthrough API. Use recipe fields for task, model, working directory, and timeout.

## Compatibility probe API

```bash
recipe-runner-rs --capabilities
```

The standalone probe exits zero and writes exactly one JSON object, optionally followed by a newline, with empty stderr:

```json
{"schema_version":1,"version":"<package_version>","capabilities":["codex_exec"]}
```

`version` is the compiled Cargo package version. The producer emits exactly these three keys. Unix builds advertise `codex_exec`; non-Unix builds emit an empty capability list because execution is unsupported. The probe runs before logging initialization, update checks, update-cache writes, network access, or agent startup, regardless of ambient logging/update settings. Mixed recipe/subcommand/option invocations with `--capabilities` are rejected before execution or update effects. Failed stdout writes report failure.

Consumers require numeric `schema_version: 1` and membership of `codex_exec`. Compatible newer/custom builds may pass; an exact source SHA is a separate delivery pin, not part of this JSON. `codex_exec` promises full stdin instructions, final-message-only results, explicit failure propagation, and no implicit model.

A stale runner that rejects the probe is incompatible. Managed Amplihack installations can upgrade to their corrected immutable runner revision and re-probe. An incompatible explicit `RECIPE_RUNNER_RS_PATH` override must be replaced or unset by the operator; it is not silently replaced. Check the runner capability and launcher exec support separately.

## Rust adapter API

The public `Adapter: Sync` interface and `StepResult` types are unchanged. `execute_agent_step` accepts prompt, optional agent name, effective system prompt, mode, working directory, optional model, and optional timeout, returning `Result<String, anyhow::Error>`.

For a Codex `CLISubprocessAdapter`, `Ok(String)` contains the verbatim final message within the output bound, including `Ok(String::new())` for an intentionally empty file. Execution/resource failures return contextual `Err`; the recipe runner reports a failed step using existing failure and `continue_on_error` policies. Optional JSON parsing happens after successful raw extraction. No new public error enum, output-path parameter, or JSONL protocol is required.
