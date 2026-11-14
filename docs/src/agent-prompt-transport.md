---
title: Agent prompt transport
description: Backend input delivery, content preservation, permissions, and cleanup.
doc_type: reference
last_updated: 2026-11-14
---

# Agent prompt transport

Agent steps deliver complete task and system instructions without putting their
contents in the Amplihack wrapper's command-line arguments, for every prompt size.

## Contents

- [Backend transport](#backend-transport)
- [Content preservation](#content-preservation)
- [Configuration and usage](#configuration-and-usage)
- [Private resources](#private-resources)
- [Delivery, cancellation, and failures](#delivery-cancellation-and-failures)
- [Privacy boundary](#privacy-boundary)

## Backend transport

| Backend | Wrapper arguments for instructions | Input delivery |
|---|---|---|
| Claude | `--system-prompt-file PATH` and a bare `-p` | The file holds effective system instructions; stdin carries the complete task and autonomy footer, followed by EOF. |
| Codex | `exec … -` | Stdin carries the complete system/task envelope, followed by EOF. No temporary prompt file is needed. |
| Copilot | `-p` with a fixed pointer sentence naming an absolute private file path | The file holds the complete system/task envelope. `--add-dir` grants access to that file's directory. |

The runner retains the `--` separator between the Amplihack subcommand and
backend options. For Claude, `amplihack claude -- … -p` passes the bare `-p`
through and the child inherits the wrapper's stdin. The compatible `launch` and
`RustyClawd` aliases use the same transport as Claude.

Copilot's pointer contains fixed wording and a generated path only. It includes
no task excerpt. The private directory grant applies even with `--no-add-dir`;
that option suppresses the working-directory grant. The grant covers the prompt
directory, not its parent. Codex retains its
[exec options and final-message output](codex-exec.md#provider-and-configuration).

## Content preservation

The runner preserves supplied task and system text exactly, including leading
and trailing whitespace, whitespace-only personas, tabs, blank lines, Unicode,
and leading dashes. Prompt size never selects a different transport: there is
no inline preview, continuation split, truncation, or argv fallback.

Effective instructions include the supplied system/persona text and the existing
instruction to execute the current recipe step without starting another workflow.
The task retains the autonomy footer. Codex keeps its `# System instructions`
and `# Task instructions` envelope labels. Separators and instruction additions
surround supplied content without trimming or replacing it. Copilot receives
the supplied persona even for a small task.

Preservation applies to inputs received by the adapter after recipe parsing,
template rendering, and agent resolution. YAML scalar rules still determine
the input text; see the [recipe format](yaml-format.md).

## Configuration and usage

Transport is automatic. No recipe field, environment switch, or public adapter
parameter selects inline delivery. Existing model, launcher, working-directory,
permission, output, and timeout settings still apply.

Existing recipe syntax remains valid. This example validates recipe
structure; it does not verify transport. Save it as `agent-summary.yaml`:

```yaml
name: agent-summary
version: "1.0"
steps:
  - id: summarize
    type: agent
    prompt: |
      Explain the purpose of this repository in three sentences.
      Return only the explanation.
    auto_stage: false
    output: summary
```

```bash
recipe-runner-rs agent-summary.yaml --validate-only
# Output: ✓ Recipe 'agent-summary' is valid (1 steps)
```

When executed with `--agent-binary claude`, `codex`, or `copilot`, the recipe
stores the agent's response in `summary`. Each backend uses its existing
authentication and launcher setup; see [Quick Start](quickstart.md) and
[Codex agent steps](codex-exec.md). Persona references use the existing `agent`
field. The public Rust `Adapter` and `StepResult` interfaces are unchanged.

## Private resources

Each attempt owns fresh temporary prompt resources, separate from retained logs.
The platform temporary-directory configuration, including `TMPDIR` on Unix,
determines their location. Prompt bytes never become file or directory names.

| Property | Contract |
|---|---|
| Directory | Created with mode `0700` on Unix. |
| Prompt file | Created exclusively with mode `0600` on Unix, using a fixed filename inside the fresh directory. |
| Preparation | File contents are completely written and the file closed before the wrapper starts. |
| Lifetime | Files remain available until the wrapper and its backend processes have stopped, including during graceful shutdown and after a report-only timeout. |
| Cleanup | Files and directories are removed after success, nonzero exit, preparation or command-construction failure, startup failure, stdin-delivery failure, terminating timeout, or supported cancellation. |
| Retained logs | `--log-dir` retains output logs without retaining prompt files. |
| Repeated attempts | Resources are removed before any existing retry delay; a later attempt receives fresh resources. |

Codex still owns private final-message resources; those are output resources,
not prompt files. Resource creation, writing, or removal failures are reported.
A cleanup failure does not establish that every resource was removed.
If backend retirement cannot be confirmed, the runner retains the private prompt
directory, reports its location, and returns a terminal cleanup failure. Both
explicit cleanup and resource guards preserve these files; an operator can remove
them after establishing that their consumers have stopped.

## Delivery, cancellation, and failures

Claude and Codex require complete stdin delivery and EOF. Nonblocking delivery
remains responsive to cancellation and existing execution deadlines even when
the wrapper stops reading a large input. A broken pipe, zero write, other write
error, or child exit before all input is delivered fails the attempt, including
when the child exits zero. There is no fallback that places text in argv.
Transport failures do not enter rate-limit recovery.

Shared scoped SIGINT/SIGTERM ownership starts before prompt preparation and
continues through child shutdown, I/O completion, resource removal, and explicit
restoration of prior signal handlers. Application cancellation and handled SIGINT
or SIGTERM request the wrapper's supported SIGTERM shutdown. Each file-backed
invocation starts in a dedicated process group. A retained group anchor preserves
authority to signal that invocation even if the wrapper exits first. The wrapper
gets up to four seconds to finish its shutdown path (including its three-second
backend grace period); group cleanup then sends TERM,
allows a bounded grace period, escalates to KILL, and confirms backend retirement.
The runner never signals its inherited process group.

Cancellation completes only after the invocation's backend processes have stopped
and its prompt resources have been cleaned up. Termination or observation failures
are reported explicitly and retain prompt resources when retirement is unconfirmed.
The process-group boundary covers backend descendants that remain in the invocation's
group; a backend that deliberately detaches into another session is outside this
ownership contract. Typed `Interruption`
stops subsequent steps, retries, JSON repair, and recovery even under nonfatal
recipe policies. Typed `CleanupFailure` remains terminal too; combined failures
retain both markers and the primary execution failure.

Cancellation assurance requires descendant-process evidence. The earlier
single-process transport checks established ordinary input handling and cleanup,
but did not establish that a wrapper's backend had stopped. The descendant
regressions now require acknowledged backend input receipt and shutdown, a live
prompt read during shutdown, and retirement before adapter return under both
SIGINT and SIGTERM, including escalation and wrapper-first exit.

Existing timeout behavior remains in force. A terminating timeout attempts cleanup
before returning. A report-only timeout keeps waiting and retains resources until
completion or cancellation. See [timeout options](cli-reference.md) and the
[terminal cleanup contract](codex-exec.md#terminal-cleanup-failures).

## Privacy boundary

On Linux, the live wrapper's `/proc/<pid>/cmdline` contains neither supplied
brief nor system-prompt contents. Transport diagnostics and debug representations
do not copy payloads. The runner uses typed command arguments; instruction bytes
are not interpolated into shell commands.

This contract covers agent-to-wrapper prompt transport. It does not change
runner command-line context options, bash command transport, agent-generated
output, or log retention. File permissions do not isolate prompts from other
processes running as the same user or privileged processes. SIGKILL and host
failure prevent executable cleanup and can leave private files behind.

See [prompt transport verification](testing-recipes.md#prompt-transport-verification)
for the required live argv, exact delivery, permissions, and cleanup checks.
The QA scenario runs the Rust behavior suite against live wrapper stubs.
