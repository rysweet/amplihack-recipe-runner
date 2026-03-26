# Progress Markers Reference

Structured step-boundary events emitted to stderr during recipe execution.
When enabled, the Python integration layer transforms the Rust runner's raw
progress output into machine-parseable `[STEP]` and `[HEARTBEAT]` lines.

> **See also**: [How to monitor long-running recipes](howto-progress-markers.md)

---

## Contents

- [Wire Format](#wire-format)
  - [Step-start marker](#step-start-marker)
  - [Heartbeat marker](#heartbeat-marker)
  - [Completion lines (forwarded)](#completion-lines-forwarded)
  - [Suppressed output](#suppressed-output)
- [Enabling progress markers](#enabling-progress-markers)
  - [Python API](#python-api)
  - [YAML `progress` field](#yaml-progress-field)
- [Step index and total](#step-index-and-total)
- [Heartbeat timing](#heartbeat-timing)
- [Step ID sanitisation](#step-id-sanitisation)
- [Interaction with `--progress` flag](#interaction-with---progress-flag)
- [Thread model](#thread-model)

---

## Wire Format

All markers are written to `sys.stderr` with `flush=True` after each line.
Output goes to stderr so it does not contaminate JSON on stdout.

### Step-start marker

Emitted once at the beginning of every step when `progress=True`.

```
[STEP {index:02d}/{total}] {step_id} @ {YYYY-MM-DDTHH:MM:SSZ}
```

| Field     | Format                           | Example                    |
|-----------|----------------------------------|----------------------------|
| `index`   | 0-based, zero-padded 2 digits    | `00`, `01`, `21`           |
| `total`   | Zero-padded 2 digits, or `??`    | `22`, `07`, `??`           |
| `step_id` | Step identifier from recipe YAML | `step-02-clarify-requirements` |
| timestamp | UTC, ISO-8601, second precision  | `2026-03-23T14:05:33Z`     |

Example:

```
[STEP 00/22] step-00-workflow-preparation @ 2026-03-23T14:05:33Z
[STEP 01/22] step-01-prepare-workspace @ 2026-03-23T14:05:41Z
[STEP 02/22] step-02-clarify-requirements @ 2026-03-23T14:05:58Z
```

When the total step count cannot be determined (e.g., the recipe YAML is not
resolvable at call time), `total` is `??`:

```
[STEP 00/??] step-00-workflow-preparation @ 2026-03-23T14:05:33Z
```

### Heartbeat marker

Emitted every ~10 seconds while a step is still running.

```
[HEARTBEAT] {step_id} — {elapsed}s elapsed
```

| Field     | Format                           | Example                           |
|-----------|----------------------------------|-----------------------------------|
| `step_id` | Same step ID as the `[STEP]` line | `step-02-clarify-requirements`   |
| `elapsed` | Integer seconds since step start | `10`, `20`, `30`                  |

Example sequence for a slow step:

```
[STEP 02/22] step-02-clarify-requirements @ 2026-03-23T14:05:58Z
[HEARTBEAT] step-02-clarify-requirements — 10s elapsed
[HEARTBEAT] step-02-clarify-requirements — 20s elapsed
[HEARTBEAT] step-02-clarify-requirements — 30s elapsed
  ✓ step-02-clarify-requirements (38.2s)
```

### Completion lines (forwarded)

The Rust runner's completion lines are forwarded to stderr unchanged. They
carry timing data and status icons.

```
  ✓ step-02-clarify-requirements (38.2s)
  ✗ step-07-write-tests (12.1s)
  ⊘ step-03-create-issue (0.0s)
  ⚠ step-08-implement-solution (91.4s)
```

| Icon | `StepStatus`  | Meaning                              |
|------|---------------|--------------------------------------|
| `✓`  | `Completed`   | Step finished successfully           |
| `✗`  | `Failed`      | Step failed                          |
| `⊘`  | `Skipped`     | Step skipped (condition was false or tag-filtered) |
| `⚠`  | `Degraded`    | Step completed with warnings (e.g. JSON extraction failed) |
| `?`  | *(other)*     | Unknown status                       |

### Suppressed output

The Rust binary's raw step-start lines (`▶ step-id (StepType)`) are
**not** forwarded to stderr when `progress=True`. The `[STEP]` marker
supersedes them. This avoids duplicate information and ensures the
`[STEP NN/TT]` line is always first.

```
# With progress=False (or no --progress flag):
▶ step-02-clarify-requirements (Agent)
  ✓ step-02-clarify-requirements (38.2s)

# With progress=True:
[STEP 02/22] step-02-clarify-requirements @ 2026-03-23T14:05:58Z
[HEARTBEAT] step-02-clarify-requirements — 10s elapsed
  ✓ step-02-clarify-requirements (38.2s)
```

Raw `▶` lines are still collected internally and included in error messages
if the Rust binary exits with a non-zero code.

---

## Enabling progress markers

Progress markers are only emitted when `progress=True` is passed to the
Python API **and** the recipe is executed via the streaming code path
(`_stream_process_output`). Neither condition alone is sufficient.

### Python API

Pass `progress=True` to `run_recipe_via_rust`:

```python
from amplihack.recipes.rust_runner import run_recipe_via_rust

result = run_recipe_via_rust(
    "default-workflow",
    user_context={"task_description": "Add user auth"},
    progress=True,   # enables [STEP] and [HEARTBEAT] markers on stderr
)
```

With `progress=False` (the default), the function uses `subprocess.run`
instead of `subprocess.Popen`. No streaming occurs; no markers are emitted.

### YAML `progress` field

Individual steps in a recipe can carry `progress: true` as a hint to
callers that progress reporting is appropriate for that step. The Rust
runner ignores unknown fields (serde `deny_unknown_fields` is not set at
the step level), so adding this field does not break binary compatibility.

```yaml
steps:
  - id: step-00-workflow-preparation
    type: bash
    progress: true      # hint: emit progress markers for this step
    command: |
      echo "Initialising workflow…"

  - id: step-01-prepare-workspace
    type: agent
    progress: true
    agent: amplihack:core:builder
    prompt: "Prepare workspace for {{task_description}}"
```

The `progress: true` annotation on the first four steps of `default-workflow`
and `smart-orchestrator` recipes documents that those steps are long-running
and benefit from real-time visibility.

---

## Step index and total

### Index

The step index is **0-based** and counts every step the Python layer sees in
the Rust binary's stderr stream. The first `▶` line intercepted becomes
`[STEP 00/…]`, the second becomes `[STEP 01/…]`, and so on.

The index does not reset between recipe invocations; it reflects the position
within a single `run_recipe_via_rust` call.

### Total

`total` is determined by reading the recipe YAML before execution and counting
the `steps` list:

```python
# Internally:
total_steps = _count_recipe_steps(resolved_path)  # returns len(data["steps"])
```

If counting fails for any reason (file not found, YAML parse error, path not
absolute), `total` is displayed as `??`. This is a soft fallback — execution
continues normally.

---

## Heartbeat timing

The heartbeat thread fires every **10 seconds** using `threading.Event.wait(10)`.
This is not a precise wall-clock interval; the actual gap between heartbeats
may be slightly longer if the process is under heavy load.

The heartbeat is cancelled (by setting a `threading.Event`) at the next step
boundary or when the process exits. The thread is a daemon thread, so it does
not prevent process exit even if cancellation is delayed.

`elapsed` in heartbeat messages is rounded to the nearest integer second:

```
[HEARTBEAT] step-02-clarify-requirements — 10s elapsed   # ~10.0s
[HEARTBEAT] step-02-clarify-requirements — 20s elapsed   # ~20.0s
```

---

## Step ID sanitisation

Step IDs from the Rust binary are sanitised before embedding in markers to
prevent terminal control character injection:

- All characters in Unicode ranges `U+0000–U+001F` and `U+007F–U+009F` are stripped.
- The result is truncated to 128 characters.

Valid recipe YAML step IDs (ASCII letters, digits, hyphens) are unchanged by
this process. The sanitisation is a defence-in-depth measure for unusual
inputs.

---

## Interaction with `--progress` flag

The `--progress` CLI flag passed to `recipe-runner-rs` enables the Rust
binary's `StderrListener`, which produces the raw `▶` and `✓/✗` lines.

The Python `progress=True` parameter enables the Python layer's marker
transformation. Both must be active for markers to appear:

| `--progress` flag | Python `progress=True` | Result |
|-------------------|------------------------|--------|
| ✓                 | ✓                      | `[STEP]` and `[HEARTBEAT]` markers on stderr, `▶` suppressed |
| ✓                 | ✗                      | Raw `▶` and `✓/✗` lines on stderr (no `[STEP]` markers) |
| ✗                 | ✓                      | No progress output at all (Rust never emits `▶` lines) |
| ✗                 | ✗                      | No progress output (default) |

`run_recipe_via_rust(progress=True)` automatically adds `--progress` to the
Rust command. You do not need to set both independently.

---

## Thread model

When `progress=True`, `_stream_process_output` creates three threads:

| Thread          | Role                                                  |
|-----------------|-------------------------------------------------------|
| stdout thread   | Collects Rust JSON output (for `RecipeResult` parsing) |
| stderr thread   | Reads Rust stderr; intercepts `▶` lines; emits `[STEP]` markers; manages heartbeat thread lifecycle |
| heartbeat thread (per step) | Daemon thread; wakes every 10s; emits `[HEARTBEAT]` |

The stderr thread holds a `threading.Lock` when reading or writing the
current step ID and start time. The lock is released before any `print()`
call to avoid holding it during I/O.

When the stderr pipe closes (process exit), the stderr thread sets the
heartbeat cancel event in a `try/finally` block, ensuring the heartbeat
thread is always cancelled even if an exception occurs mid-stream.
