# How to Monitor Long-Running Recipes with Progress Markers

This guide shows how to enable and consume progress markers for recipes that
take more than a few seconds to run — useful in CI logs, dashboards, and
operator terminals.

> **Reference**: [Progress Markers Reference](progress-markers.md) covers the
> full wire format, thread model, and every configuration option.

---

## Contents

- [Enable markers on a single recipe run](#enable-markers-on-a-single-recipe-run)
- [Annotate recipe steps for progress reporting](#annotate-recipe-steps-for-progress-reporting)
- [Separate progress from JSON output in a script](#separate-progress-from-json-output-in-a-script)
- [Parse markers in CI (grep / awk)](#parse-markers-in-ci-grep--awk)
- [Forward markers to a monitoring system](#forward-markers-to-a-monitoring-system)
- [Understand what you see on the terminal](#understand-what-you-see-on-the-terminal)

---

## Enable markers on a single recipe run

Pass `progress=True` to `run_recipe_via_rust`. That is the only required
change.

```python
from amplihack.recipes.rust_runner import run_recipe_via_rust

result = run_recipe_via_rust(
    "default-workflow",
    user_context={
        "task_description": "Add password reset flow",
        "repo_path": "/home/user/my-app",
    },
    progress=True,
)
```

Stderr output during execution:

```
[STEP 00/22] step-00-workflow-preparation @ 2026-03-23T14:05:33Z
  ✓ step-00-workflow-preparation (0.3s)
[STEP 01/22] step-01-prepare-workspace @ 2026-03-23T14:05:34Z
  ✓ step-01-prepare-workspace (6.9s)
[STEP 02/22] step-02-clarify-requirements @ 2026-03-23T14:05:41Z
[HEARTBEAT] step-02-clarify-requirements — 10s elapsed
[HEARTBEAT] step-02-clarify-requirements — 20s elapsed
  ✓ step-02-clarify-requirements (24.1s)
```

`result` is still a `RecipeResult` — progress markers have no effect on the
return value.

---

## Annotate recipe steps for progress reporting

Add `progress: true` to the steps that are long-running or where visibility
matters most. This annotation documents intent; it does not change execution
behaviour inside the Rust binary.

```yaml
name: my-workflow
version: "1.0"
description: Example workflow with progress annotations

steps:
  - id: step-00-setup
    type: bash
    progress: true          # long enough to be worth watching
    command: ./scripts/setup.sh

  - id: step-01-analyse
    type: agent
    progress: true
    agent: amplihack:core:analyzer
    prompt: "Analyse the codebase at {{repo_path}}"
    output: analysis

  - id: step-02-implement
    type: agent
    progress: true
    agent: amplihack:core:builder
    prompt: "Implement {{task_description}} based on: {{analysis}}"

  - id: step-03-test
    type: bash
    command: cargo test       # fast, annotation not needed
```

Steps without `progress: true` still emit `[STEP]` markers when the caller
passes `progress=True`. The annotation is a documentation convention, not a
gate.

---

## Separate progress from JSON output in a script

The Rust binary writes JSON to **stdout** and progress markers to **stderr**.
Redirect each stream separately to process them independently.

```bash
#!/bin/bash
recipe-runner-rs default-workflow \
    --progress \
    --output-format json \
    --set task_description="Add telemetry" \
    2>progress.log \
    1>result.json

echo "Exit code: $?"
echo "Steps completed:"
grep '^\[STEP' progress.log | wc -l

echo "Recipe succeeded:"
jq '.success' result.json
```

From Python:

```python
import subprocess, json, sys

proc = subprocess.run(
    ["recipe-runner-rs", "my-workflow", "--progress", "--output-format", "json"],
    capture_output=True,
    text=True,
)

# stderr contains [STEP] and [HEARTBEAT] lines
for line in proc.stderr.splitlines():
    if line.startswith("[STEP") or line.startswith("[HEARTBEAT]"):
        print("PROGRESS:", line, file=sys.stderr)

# stdout contains the JSON result
data = json.loads(proc.stdout)
print("Success:", data["success"])
```

---

## Parse markers in CI (grep / awk)

### Extract step names and timestamps

```bash
# List every step that started, with its timestamp
grep '^\[STEP' progress.log
# [STEP 00/22] step-00-workflow-preparation @ 2026-03-23T14:05:33Z
# [STEP 01/22] step-01-prepare-workspace @ 2026-03-23T14:05:34Z

# Extract just the step IDs
grep -oP '(?<=\] )[\w-]+(?= @)' progress.log
# step-00-workflow-preparation
# step-01-prepare-workspace
```

### Detect long-running steps

```bash
# Show steps that needed at least one heartbeat (ran > 10s)
grep '^\[HEARTBEAT\]' progress.log | \
    grep -oP '(?<=\] )[\w-]+(?= —)' | \
    sort -u
# step-02-clarify-requirements
# step-08-implement-solution
```

### Check for failed steps

Completion lines from the Rust runner appear after each `[STEP]` block.
Failed steps use the `✗` icon:

```bash
# Any failed steps?
grep '^\s*✗' progress.log
#   ✗ step-07-write-tests (12.1s)
```

### Count total elapsed time

```bash
# Pull the timestamp from the first and last [STEP] markers
first=$(grep '^\[STEP' progress.log | head -1 | grep -oP '\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z')
last=$(grep '^\[STEP' progress.log | tail -1 | grep -oP '\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z')
echo "First step: $first"
echo "Last step:  $last"
```

---

## Forward markers to a monitoring system

Tee stderr to both the terminal and a collector while the recipe runs:

```bash
recipe-runner-rs default-workflow \
    --progress \
    --output-format json \
    --set task_description="Deploy to staging" \
    2> >(tee progress.log | grep '^\[STEP\|^\[HEARTBEAT\]' | \
         while IFS= read -r line; do
           curl -s -X POST https://monitor.example.com/events \
             -H "Content-Type: application/json" \
             -d "{\"event\": $(printf '%s' "$line" | jq -Rs .)}" \
             &
         done) \
    1>result.json
```

From Python using a thread to relay markers in real time:

```python
import subprocess, sys, threading, json

def relay_markers(stderr_pipe):
    for line in stderr_pipe:
        sys.stderr.write(line)
        sys.stderr.flush()
        if line.startswith("[STEP") or line.startswith("[HEARTBEAT]"):
            send_to_monitor(line.strip())  # your monitoring call

proc = subprocess.Popen(
    ["recipe-runner-rs", "my-workflow", "--progress", "--output-format", "json"],
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
    bufsize=1,
)

relay_thread = threading.Thread(target=relay_markers, args=(proc.stderr,), daemon=True)
relay_thread.start()

proc.wait()
relay_thread.join()

data = json.loads(proc.stdout.read())
print("Success:", data["success"])
```

---

## Understand what you see on the terminal

A complete run of a two-step recipe with progress enabled looks like:

```
[STEP 00/02] step-build @ 2026-03-23T14:10:00Z
  ✓ step-build (3.4s)
[STEP 01/02] step-test @ 2026-03-23T14:10:03Z
[HEARTBEAT] step-test — 10s elapsed
[HEARTBEAT] step-test — 20s elapsed
[HEARTBEAT] step-test — 30s elapsed
  ✓ step-test (33.7s)
```

| Line pattern              | What it means                                    |
|---------------------------|--------------------------------------------------|
| `[STEP NN/TT] id @ time`  | Step `id` started; it is step `NN` of `TT` total |
| `[HEARTBEAT] id — Ns`     | Step `id` is still running; `N` seconds elapsed  |
| `  ✓ id (Xs)`             | Step `id` completed successfully in `X` seconds  |
| `  ✗ id (Xs)`             | Step `id` failed after `X` seconds               |
| `  ⊘ id (0.0s)`           | Step `id` was skipped (condition false or tag-filtered) |
| `  ⚠ id (Xs)`             | Step `id` completed with warnings                |

The `[STEP]` line is always first for each step. One or more `[HEARTBEAT]`
lines may follow if the step takes more than 10 seconds. The completion line
(`✓/✗/⊘/⚠`) is always the last line for each step.

If `total` shows `??` instead of a number, the Python layer could not read
the recipe YAML to count steps. The run continues normally; only the total is
missing from the display.
