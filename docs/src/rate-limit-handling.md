# Rate-Limit Handling

When the Recipe Runner executes an agent step, it spawns the `amplihack` launcher
to run an AI agent (`copilot`, `claude`, or `codex`). Hosted agents enforce
**transient rate limits**: under load, the underlying provider can reject a request
with a short cooldown window — for example, a Copilot enterprise limit exits with
status `1` and the message:

> You've hit your rate limit. Please wait for your limit to reset in under a minute
> or switch to auto model to continue.

This is **not** a logic failure. It is temporary back-pressure that clears on its own.
Rather than aborting the whole workflow run — and discarding every step that already
succeeded — the runner **detects** rate-limit signals, **waits**, and **retries** the
same step with bounded exponential backoff before giving up with a clear error.

> **Scope.** Rate-limit handling applies only to **agent steps**
> (`amplihack <agent>` subprocesses). Bash steps (`execute_bash_step`) are
> unaffected. The existing per-step timeout and output-capture/truncation behavior
> are preserved unchanged.

## How it works

1. **Run the step.** The agent subprocess is spawned, monitored against the
   per-step timeout, and its `stdout`/`stderr` are captured (and truncated) exactly
   as before.
2. **Inspect failures.** On a **non-zero exit**, the runner checks the captured
   output for rate-limit signals (case-insensitive substring match):
   - `hit your rate limit`
   - `reset in`
   - `rate limit`
   - `429`
   - `too many requests`
3. **Decide.**
   - **No rate-limit signal** → **fail fast** with the existing
     `amplihack <cli> failed (exit N)` error. Authentication errors, bad arguments,
     and genuine agent failures are **never** retried or swallowed.
   - **Rate-limit signal** and retries remain → log a loud banner, **sleep** the
     backoff delay, then **retry the same step**.
4. **Bound the work.** After the configured number of retries is exhausted, the
   runner stops and **fails explicitly** — it never loops forever.

Total agent executions for a single step are bounded to `1 + max_retries`.

## Backoff policy

The wait before retry number `n` is:

```
delay = min(base_delay * 2^(n-1), max_delay)
```

> **`n` is a retry counter, not the overall execution count.** It starts at `1`
> for the **first retry** (the second total execution of the step) and increments
> by one per retry. The initial attempt is execution `0` and has no preceding wait.
> Implementations must call the backoff function with this retry index — passing
> the overall execution count would shift every wait by one doubling (the first
> wait would become 120s) and contradict the table below.

- The first retry (`n = 1`) waits one `base_delay` window (default ~60s, matching
  the provider's "under a minute" hint).
- Each subsequent retry doubles the wait, capped at `max_delay` (default 600s).
- The delay arithmetic is saturating: extreme attempt counts can never overflow,
  and the result is always `≤ max_delay`.
- Setting `base_delay = 0` makes every wait instant — useful for fast tests.

Example with defaults (`base=60`, `cap=600`, `max_retries=5`):

| Retry | Formula            | Wait (s) |
|-------|--------------------|----------|
| 1     | `min(60·2⁰, 600)`  | 60       |
| 2     | `min(60·2¹, 600)`  | 120      |
| 3     | `min(60·2², 600)`  | 240      |
| 4     | `min(60·2³, 600)`  | 480      |
| 5     | `min(60·2⁴, 600)`  | 600 (capped) |

## Loud, never silent

Every detected rate-limit and every backoff is reported to **stderr** — the runner
never waits silently. Each retry emits a banner naming the matched signal, the wait
duration, and the attempt counter:

```text
================================================================================
⏳ RATE LIMIT detected (signal: "hit your rate limit")
   Agent step throttled by the provider. This is transient — backing off.
   Waiting 60s before retry 2/5 ...
================================================================================
```

When retries are exhausted, the step fails with an explicit, actionable message
(this exact wording is the canonical target the implementation's `bail!` must emit):

```text
Error: amplihack copilot rate limit persisted after 5 retries (6 attempts total).
The provider kept throttling this step. Increase AMPLIHACK_RATELIMIT_MAX_RETRIES
or AMPLIHACK_RATELIMIT_BASE_DELAY_SECS, or retry later.
```

## Optional `--model auto` fallback

The provider message suggests switching to the auto model to keep working. The
runner can apply this automatically as a last resort. When
`AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL` is set to a non-empty value, the **final**
retry attempt is run with `--model auto` appended, overriding any explicit `model`
configured for that step. This behavior is **off by default** so that normal runs
never silently change models.

## Configuration

All knobs are environment variables, parsed leniently — an unset or unparseable
value falls back to its default rather than failing the run. The variable names and
defaults below are the **published contract**: `RateLimitConfig::from_env()` must read
exactly these keys and apply exactly these defaults.

| Variable | Default | Description |
|---|---|---|
| `AMPLIHACK_RATELIMIT_MAX_RETRIES` | `5` | Maximum retries **after** the initial attempt. Total executions are `1 + this`. Clamped to a hard ceiling of `100` to bound the worst-case budget. |
| `AMPLIHACK_RATELIMIT_BASE_DELAY_SECS` | `60` | Base backoff window in seconds. `0` makes all waits instant (used by tests). |
| `AMPLIHACK_RATELIMIT_MAX_DELAY_SECS` | `600` | Upper cap on any single backoff delay. Enforced to be `≥ base_delay`. |
| `AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL` | _unset_ | When non-empty, force `--model auto` on the final retry attempt. |
| `AMPLIHACK_LAUNCHER_BINARY` | `amplihack` | Override the launcher executable. Test-only override for injecting a fake agent binary; production behavior is identical when unset. |

### Examples

```bash
# More patient: up to 8 retries with a longer base window
AMPLIHACK_RATELIMIT_MAX_RETRIES=8 \
AMPLIHACK_RATELIMIT_BASE_DELAY_SECS=90 \
recipe-runner-rs build.yaml

# Aggressive cap so no single wait exceeds 2 minutes
AMPLIHACK_RATELIMIT_MAX_DELAY_SECS=120 \
recipe-runner-rs deploy.yaml

# Let the runner fall back to --model auto on the last attempt
AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL=1 \
recipe-runner-rs long-task.yaml

# Disable backoff waits entirely (fast feedback in CI / tests)
AMPLIHACK_RATELIMIT_BASE_DELAY_SECS=0 \
recipe-runner-rs smoke.yaml
```

## Behavior summary

| Situation | Outcome |
|---|---|
| Agent exits `0` | Step succeeds (no change from prior behavior). |
| Non-zero exit, **no** rate-limit signal | **Fail fast** with `amplihack <cli> failed (exit N)`. |
| Non-zero exit **with** rate-limit signal, retries remain | Loud banner → backoff sleep → retry the same step. |
| Rate-limit signal, retries exhausted | Explicit "rate limit persisted after N retries" error. |
| Per-step timeout exceeded | Unchanged — handled by the existing timeout path. |
| Bash step | Unaffected; no rate-limit handling applied. |

## FAQ

**Does this retry every failing step?**
No. Only failures whose captured output matches a rate-limit signal are retried.
All other non-zero exits fail immediately, exactly as before.

**Can it loop forever?**
No. Executions are bounded to `1 + max_retries`, and `max_retries` is clamped to a
hard ceiling. A persistent rate limit always terminates with a clear error.

**Does it parse the exact reset time from the message?**
No. The message only hints "under a minute," so the runner uses the configured
`base_delay` with exponential backoff instead of fragile text parsing.

**Will it change my model without asking?**
Only if you opt in with `AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL`, and only on the
final retry attempt.
