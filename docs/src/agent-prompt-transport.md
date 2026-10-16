---
title: Agent prompt transport
description: How the runner delivers agent-step prompts and system prompts to Claude, Copilot and other CLIs, including the file-backed path for prompts over 32 KiB.
doc_type: reference
last_updated: 2026-10-16
---

# Agent prompt transport

An agent step has two pieces of text:

- The **prompt**: the rendered `prompt:` field followed by the runner's
  non-interactive footer (`NON_INTERACTIVE_FOOTER`, "IMPORTANT: Proceed
  autonomously. Do not ask questions. …"). The footer is part of the prompt
  for every size check and split described below.
- The **effective system prompt**: the agent's persona or system prompt, if
  any, with surrounding whitespace trimmed, then `\n\n`, then the runner's
  built-in no-reentry guard (`RECIPE_CHILD_NO_REENTRY_SYSTEM_PROMPT`). A
  persona that is empty after trimming is dropped, leaving only the guard.

The guard tells the
child agent that it is already inside a recipe step and must not start
`/dev`, `smart-orchestrator` or another workflow. It is always present, so the
effective system prompt is never empty.

This page describes how both pieces reach the child CLI. Codex is not covered
here: it gets everything on stdin for every prompt size (see
[Codex agent steps](codex-exec.md)).

## Contents

- [Choosing a transport](#choosing-a-transport)
- [Inline transport](#inline-transport)
- [File-backed transport](#file-backed-transport)
- [The prompt file](#the-prompt-file)
- [Example: what a Claude child receives](#example-what-a-claude-child-receives)
- [Limits](#limits)

## Choosing a transport

| Condition | Transport |
|---|---|
| `len(prompt + footer) + len(effective system prompt)` ≤ 32 KiB (`MAX_INLINE_AGENT_PROMPT_BYTES`) | Inline |
| Over 32 KiB | File-backed |

Sizes are counted in bytes. Because the footer counts, a `prompt:` field just
under the limit can still cross it. The threshold keeps the argument vector well under
the kernel's `execve` limit (see
[Environment Budget & Safe Spawning](env-budget.md)).

## Inline transport

| CLI | Arguments after `amplihack <cli> --` |
|---|---|
| `claude`, `launch`, `RustyClawd` | `-p <prompt> --system-prompt <effective system prompt> --add-dir <cwd> [--model M]` |
| `copilot` | `--allow-all-tools -p <prompt> --add-dir <cwd> [--model M]` |
| others | `-p <prompt> --add-dir <cwd> [--model M]` |

`--model M` is added only when the step (or the final-attempt `--model auto`
fallback) selects a model. Copilot gets `--allow-all-tools` on both
transports so it can run without tool-approval prompts; set
`AMPLIHACK_NO_ALLOW_ALL_TOOLS` to any non-empty value to leave it out.

Non-Claude CLIs have no system-prompt flag, so they get no system prompt on
this path.

## File-backed transport

The runner splits the prompt at 8 KiB (`FILE_BACKED_INLINE_PROMPT_BYTES`),
moving the split back to the nearest UTF-8 character boundary so that a
multibyte character is never cut in half.

- **Inline head**: the first ≤ 8 KiB of the prompt, sent with `-p`.
- **Overflow**: the rest of the prompt, written to the
  [prompt file](#the-prompt-file) after the effective system prompt.

When there is overflow, a continuation note is added to the end of the inline
head, so the agent knows the instructions carry on elsewhere:

| CLI | How the prompt file is passed | Continuation note (only when there is overflow) |
|---|---|---|
| `claude`, `launch`, `RustyClawd` | `--append-system-prompt-file <absolute path>` | "…continue in the appended system prompt…" (`FILE_BACKED_PROMPT_CONTINUATION_NOTE`) |
| `copilot` and others | `--add-dir <output dir>`, so the agent can read the file | "…continue in the file `<absolute path>`. Read that file and treat its content as part of this same request and follow it fully." |

Claude-family CLIs always get `--append-system-prompt-file`, never
`--append-system-prompt`. `--append-system-prompt` takes literal **text**: if
it were given a path, Claude would use the path string as its system prompt.
The overflow and the no-reentry guard would be lost without any error. This
was issue #159.

The note for non-Claude CLIs only ever contains the path the runner just wrote
to. No recipe input or environment value goes into it.

Full argv for each case (after `amplihack <cli> --`):

```text
claude:   --append-system-prompt-file /tmp/.tmpAb12Cd/.recipe-output/agent-system-prompt.md -p <head+note> --add-dir <cwd> [--model M]
copilot:  [--allow-all-tools] --add-dir /tmp/.tmpAb12Cd/.recipe-output -p <head+note> --add-dir <cwd> [--model M]
```

For Copilot and other CLIs, `--add-dir <output dir>` exposes the whole
`.recipe-output` directory, not only the prompt file. That directory also
holds the step's per-attempt agent logs (`agent-step-<attempt>-<time>.log`),
so the agent can read them too.

## The prompt file

| Property | Value |
|---|---|
| Name | `agent-system-prompt.md` |
| Location | `<tempdir>/.recipe-output/agent-system-prompt.md`, where `<tempdir>` is the step's private `tempfile::tempdir()` (mode `0700`). The path is always absolute. `.recipe-output` is created with the process's default umask, but it sits inside the `0700` temp dir, so other users can't reach it. |
| Mode | `0600`, set when the file is created and again after every write |
| Lifetime | Rewritten on each retry attempt. Deleted when the step finishes and the temp dir is dropped. |
| Content | `<effective system prompt>` + `"\n\n# Continued task instructions\n\n"` + `<overflow>`. Without overflow it holds only the effective system prompt. |

The split is byte-exact: the inline head **without the continuation note**,
followed by the overflow, equals the prompt (including the footer) exactly.
When there is overflow, the footer is at the end of the prompt, so it ends up
in the file, not in `-p`.

If the file can't be created or written, the step fails. The agent is never
launched without its system prompt.

## Example: what a Claude child receives

This step has a 40 KiB prompt:

```yaml
- id: review
  agent: amplihack:core:reviewer
  prompt: "{{large_diff_and_instructions}}"
```

The child is started as:

```text
amplihack claude -- \
  --append-system-prompt-file /tmp/.tmpAb12Cd/.recipe-output/agent-system-prompt.md \
  -p "<first 8 KiB of the prompt>\n\nIMPORTANT: Additional task instructions ... continue in the appended system prompt. ..." \
  --add-dir /path/to/working_dir
```

No model is selected here, so there is no `--model`.
`/tmp/.tmpAb12Cd/.recipe-output/agent-system-prompt.md` contains:

```text
<reviewer persona>

You are already inside an active recipe-managed workflow step. ...

# Continued task instructions

<remaining ~32 KiB of the prompt>

IMPORTANT: Proceed autonomously. Do not ask questions. Make reasonable decisions and continue.
```

The last line is the footer, carried over with the rest of the overflow.

To check this yourself, set `AMPLIHACK_LAUNCHER_BINARY` to a script that logs
its arguments and copies the file named after `--append-system-prompt-file`.
The file is deleted when the step ends, so the script has to copy it while it
is running. Create a private capture directory first, rather than writing to
fixed paths in `/tmp`:

```bash
CAPTURE_DIR=$(mktemp -d)
export CAPTURE_DIR
cat > "$CAPTURE_DIR/fake-amplihack" <<'SH'
#!/bin/sh
printf '%s\n' "$@" > "$CAPTURE_DIR/argv.txt"
while [ $# -gt 0 ]; do
  [ "$1" = "--append-system-prompt-file" ] && cat "$2" > "$CAPTURE_DIR/prompt-file.txt"
  shift
done
exit 0
SH
chmod 700 "$CAPTURE_DIR/fake-amplihack"
export AMPLIHACK_LAUNCHER_BINARY="$CAPTURE_DIR/fake-amplihack"
```

After the step runs, read `$CAPTURE_DIR/argv.txt` and
`$CAPTURE_DIR/prompt-file.txt`, then remove `$CAPTURE_DIR`.

## Limits

- **Non-Claude CLIs with a large system prompt but no overflow.** The prompt
  file is written and `--add-dir` is passed, but no continuation note is added,
  so nothing tells the agent to read the file. This matches the inline path,
  where these CLIs get no system prompt either.
- **No amplihack routing contract for launcher children.** The `amplihack
  claude` launcher does not add its own system-prompt flag when
  `--append-system-prompt` or `--append-system-prompt-file` is already on the
  command line, so a file-backed child does not get amplihack's routing
  contract. The old flag triggered the same skip. Recipe children carry the
  no-reentry guard instead.
- **Argv exposure.** The ≤ 8 KiB inline head is still on the command line, and
  other processes of the same user can read it through `/proc/<pid>/cmdline`.
