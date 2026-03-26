# How to Read Enriched Heartbeat Output

This guide explains the enriched heartbeat format introduced to make
long-running agent steps observable in CI logs, operator terminals, and
monitoring systems.

> **Reference**: [Progress Markers Reference](progress-markers.md) covers the
> full wire format, thread model, and every configuration option.
>
> **Related**: [How to Monitor Long-Running Recipes](howto-progress-markers.md)
> covers enabling `progress=True` and forwarding markers to external systems.

---

## Contents

- [What changed and why](#what-changed-and-why)
- [Read the enriched heartbeat format](#read-the-enriched-heartbeat-format)
- [Understand agent output streaming](#understand-agent-output-streaming)
- [Interpret working vs waiting status](#interpret-working-vs-waiting-status)
- [Correlate heartbeat lines with external logs](#correlate-heartbeat-lines-with-external-logs)
- [Filter heartbeat lines in scripts](#filter-heartbeat-lines-in-scripts)
- [Understand platform differences](#understand-platform-differences)

---

## What changed and why

Before this improvement, heartbeat output looked like:

```
  [agent] ... still running (60s since last output)
```

That line told an operator the step was alive, but not:

- **When** the message was emitted — no timestamp to cross-reference logs
- **Which agent** produced it when multiple steps ran — label lacked context
- **Which process** to inspect with `ps` or `kill` — PID was missing
- **What the agent last said** — output was truncated to 120 characters

The enriched format addresses all four gaps:

```
  [14:32:15] [amplihack:architect:48291] Analysing module boundaries…
  [14:32:15] [amplihack:architect:48291] Found 12 boundary candidates.
  [14:33:47] [amplihack:architect:48291] ... working (92s elapsed, 32s since last output)
```

---

## Read the enriched heartbeat format

Every heartbeat line written to stderr has the same prefix:

```
  [HH:MM:SS] [agent_name:pid] <content>
```

| Field         | Example              | Description                                        |
|---------------|----------------------|----------------------------------------------------|
| `HH:MM:SS`    | `14:32:15`           | UTC wall-clock time when the line was emitted      |
| `agent_name`  | `amplihack:architect`| Name passed to the `agent:` key in the recipe YAML |
| `pid`         | `48291`              | OS process ID of the child `amplihack` subprocess  |
| `content`     | `Analysing module…`  | New output line from the agent, or a status notice |

A complete run with two agent steps and progress enabled:

```
[STEP 04/22] step-04-design @ 2026-03-26T14:31:43Z
  [14:31:45] [amplihack:architect:48291] Loading project context…
  [14:31:47] [amplihack:architect:48291] Context loaded. 3 files read.
  [14:33:47] [amplihack:architect:48291] ... working (124s elapsed, 32s since last output)
  [14:34:22] [amplihack:architect:48291] Module spec written to /tmp/.recipe-output/design.md
  ✓ step-04-design (161.3s)
[STEP 05/22] step-05-implement @ 2026-03-26T14:34:24Z
  [14:34:26] [amplihack:builder:48819] Reading design spec…
  [14:36:56] [amplihack:builder:48819] ... working (150s elapsed, 30s since last output)
  [14:37:38] [amplihack:builder:48819] Implementation complete.
  ✓ step-05-implement (194.1s)
```

---

## Understand agent output streaming

The heartbeat thread polls the agent's output log file every 2 seconds. When
the file has grown since the last poll, it reads **all new lines** from the
last known byte position to the end of the file and prints each non-empty line
immediately.

This means:

- **All output is visible** — there is no truncation of output lines.
- **Lines arrive in bursts** — if an agent writes 20 lines in 2 seconds, all
  20 appear together in the next poll cycle.
- **Empty lines are suppressed** — the heartbeat skips blank lines to keep
  stderr readable.

If an agent produces very high-volume output (e.g., streaming token-by-token
responses), stderr can become voluminous. Redirect stderr to a file if you
need a clean terminal:

```bash
recipe-runner-rs default-workflow \
    --progress \
    --output-format json \
    --set task_description="Refactor auth module" \
    2>run.log \
    1>result.json

# Tail only the heartbeat status lines during the run
tail -f run.log | grep '^\.\.\. working\|^\.\.\. waiting'
```

---

## Interpret working vs waiting status

When no new output has appeared for **30 seconds**, the heartbeat emits a
status notice instead of an output line:

### Process alive — working

```
  [14:33:47] [amplihack:architect:48291] ... working (124s elapsed, 32s since last output)
```

The child process exists in `/proc/<pid>`. It has produced no output for 32
seconds but the process is alive — normal for agents doing inference, large
file reads, or waiting for an API response.

### Process not found in /proc — waiting

```
  [14:34:02] [amplihack:architect:48291] ... waiting (139s elapsed, process may be finishing)
```

`/proc/<pid>` does not exist, which typically means the process has exited but
the main thread has not yet reaped it. This state is transient — the
completion line (`✓` or `✗`) usually follows within seconds.

> **Note**: On macOS and Windows, `/proc` does not exist. The heartbeat always
> emits the `... waiting` form on those platforms, even when the process is
> running. The run still completes correctly; only the status wording differs.

---

## Correlate heartbeat lines with external logs

Each heartbeat line includes a UTC `HH:MM:SS` timestamp computed from
`SystemTime::now()`. Use this to correlate with:

- **GitHub Actions / Azure DevOps log timestamps** — both record UTC wall time
  against each log line. The heartbeat timestamp matches the CI log timestamp
  within ±1 second.
- **Agent trace files** — `amplihack` traces (if enabled) record UTC
  timestamps in the same `HH:MM:SS` format. Match the PID to confirm which
  trace belongs to which heartbeat.
- **System logs** — `journalctl` and `/var/log/syslog` use UTC. The PID in the
  heartbeat label maps directly to the syslog entry for the child process.

Example: correlate a heartbeat line with a GitHub Actions log line:

```
# GitHub Actions log (UTC)
2026-03-26T14:33:47.312Z  [14:33:47] [amplihack:architect:48291] ... working (124s elapsed, 32s since last output)

# amplihack trace excerpt (UTC)
14:33:45.901 [48291] tool_call: Read path=/src/auth.rs
14:33:46.412 [48291] tool_result: 312 bytes
```

The 2-second gap between the trace and the heartbeat is the poll interval —
the heartbeat emits at the next 2-second tick after the trace entry.

---

## Filter heartbeat lines in scripts

Heartbeat lines always start with two spaces followed by `[`:

```bash
# Extract only agent output lines (not status notices)
grep -v '\.\.\. working\|\.\.\. waiting' run.log | grep '^\s\s\['

# Extract only status notices
grep '\.\.\. working\|\.\.\. waiting' run.log

# Extract lines from a specific agent
grep '\[amplihack:architect:[0-9]*\]' run.log

# Extract lines from a specific PID
grep '\[amplihack:architect:48291\]' run.log

# Count output lines per agent
grep -oP '\[[\w:]+:\d+\]' run.log | sort | uniq -c | sort -rn
```

From Python:

```python
import re
from pathlib import Path

HEARTBEAT_RE = re.compile(
    r"^\s{2}\[(\d{2}:\d{2}:\d{2})\] \[([\w:]+):(\d+)\] (.+)$"
)

lines = Path("run.log").read_text().splitlines()
for line in lines:
    m = HEARTBEAT_RE.match(line)
    if m:
        ts, agent, pid, content = m.groups()
        if "working" in content or "waiting" in content:
            print(f"STATUS  {ts} {agent}:{pid} → {content}")
        else:
            print(f"OUTPUT  {ts} {agent}:{pid} → {content}")
```

Output for the example run above:

```
OUTPUT  14:31:45 amplihack:architect:48291 → Loading project context…
OUTPUT  14:31:47 amplihack:architect:48291 → Context loaded. 3 files read.
STATUS  14:33:47 amplihack:architect:48291 → ... working (124s elapsed, 32s since last output)
OUTPUT  14:34:22 amplihack:architect:48291 → Module spec written to /tmp/.recipe-output/design.md
```

---

## Understand platform differences

| Behaviour                     | Linux              | macOS / Windows         |
|-------------------------------|--------------------|-------------------------|
| `HH:MM:SS` timestamp          | ✓ UTC              | ✓ UTC                   |
| `agent_name:pid` label        | ✓                  | ✓                       |
| Full output streaming         | ✓                  | ✓                       |
| Process alive check (`/proc`) | ✓ accurate         | ✗ always `... waiting`  |
| Status wording on exit        | `working` → `waiting` at exit | always `waiting` |

The only functional difference across platforms is the process alive check.
On all platforms the recipe executes correctly, all output lines stream, and
the `HH:MM:SS` label is accurate.

> **Security note**: Agent output written to stderr may contain secrets if the
> agent echoes environment variables or credentials. In CI pipelines where
> logs are stored as public artefacts, redirect stderr to a private log store:
>
> ```bash
> recipe-runner-rs my-recipe --progress 2>private-run.log 1>result.json
> ```
