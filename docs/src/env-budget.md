# Environment Budget & Safe Subprocess Spawning

The recipe runner spawns `bash` and agent subprocesses for many steps. On Linux
the kernel caps the combined size of a new process' argument vector **and**
environment block. When that limit is exceeded, `execve(2)` fails with `E2BIG`
(`os error 7`) and the child never starts.

This page documents how the runner stays safely under that limit:

1. **Root-cause sanitization** — the inherited parent environment is cleared
   (`env_clear`) and deterministically rebuilt before every spawn from a
   *bounded* set: all protected variables, plus as many non-protected variables
   as fit under the budget. Nothing leaks in implicitly from the parent.
2. **File-first context** — large recipe context is written to a `0600` JSON
   file and passed by reference (`AMPLIHACK_CONTEXT_FILE`), never inline.
3. **A measured environment budget** — the size ceiling is computed from the
   host's real kernel limits at runtime, not hardcoded.
4. **Fail-loud enforcement** — if a *protected* variable cannot fit, the spawn
   is aborted with a named error instead of silently dropping the value or
   letting a raw `E2BIG` reach the user.

Together these guarantee that a recipe never dies with an opaque `os error 7`,
and that required variables are never silently discarded.

---

## The measured budget

The environment byte budget is derived **once per process** from the host's
real limits and cached in a `OnceLock`. It replaces the previous hardcoded
`1_500_000` literal, which was wrong on hosts with a smaller `ARG_MAX` or a
large argument vector.

### Formula

```text
budget = min( sysconf(_SC_ARG_MAX), RLIMIT_STACK / 4 )
         - argv_reserve
```

| Term             | Meaning                                                                                 |
| ---------------- | --------------------------------------------------------------------------------------- |
| `sysconf(_SC_ARG_MAX)` | Kernel's maximum combined argv + env size for a new process.                      |
| `RLIMIT_STACK / 4`     | Linux additionally caps argv+env at 1/4 of the stack rlimit; whichever is smaller wins. |
| `argv_reserve`   | Fixed **128 KiB** headroom reserved for the command path and its arguments.              |

Because every spawn calls `Command::env_clear()` and installs **only** the
curated `bounded_env` result, that curated set *is* the child's entire
environment block — there is no separately-inherited baseline to subtract. The
budget therefore bounds the whole environment directly, reserving `argv_reserve`
for the command line (large argv/scripts are spilled to files elsewhere).

The computation uses **saturating** arithmetic and the result is **clamped to a
floor of 256 KiB**, so a pathologically small `ARG_MAX` can never wrap around to
a huge value or produce a budget below the floor.

### Conservative fallback

If `sysconf(_SC_ARG_MAX)` or `getrlimit(RLIMIT_STACK)` are unavailable or return
a non-positive value, the budget falls back to the **256 KiB floor**. The floor
path is only taken when the syscalls genuinely fail — on a normal Linux host the
budget is always derived from `sysconf`.

### Single source of truth

The budget is exposed through one shared accessor:

```rust
/// Returns the measured environment byte budget for child processes.
///
/// Computed once from `sysconf(_SC_ARG_MAX)` and `RLIMIT_STACK`, cached in a
/// `OnceLock`. Both the context layer and the subprocess adapter call this
/// accessor so their notion of "how much env fits" can never drift apart.
pub(crate) fn env_byte_budget() -> usize
```

Both `context::shell_env_for_step` and the subprocess adapter's `bounded_env`
call `env_byte_budget()`. There is no second copy of the limit and no `const`
literal to fall out of sync. The accessor is `pub(crate)` — it is an internal
invariant shared across modules, not a public API surface.

---

## Protected variables

Some variables are **protected**: they are required for correct nested execution
and are *never* dropped or reordered when trimming the environment to fit the
budget.

A variable is protected if its name is one of:

- `PATH`
- `HOME`
- `TASK_DESCRIPTION`
- `REPO_PATH`
- `TASK_TYPE`
- `WORKSTREAM_COUNT`

…or if its name starts with one of these prefixes:

- `AMPLIHACK_` (e.g. `AMPLIHACK_CONTEXT_FILE`, `AMPLIHACK_SESSION_DEPTH`, `AMPLIHACK_TREE_ID`)
- `RECIPE_VAR_`

`PATH` is protected specifically so that binary resolution in the child can never
be altered by budget trimming.

Everything else is **non-protected** and may be trimmed.

---

## Trimming behavior

When the assembled child environment exceeds `env_byte_budget()`, the runner
trims it as follows:

1. **Drop non-protected variables largest-first** until the total fits. These
   values have already been relocated into the file-first context
   (`AMPLIHACK_CONTEXT_FILE`), so dropping them is **lossless** — a bash step can
   still recover any value with `jq`:

   ```bash
   value="$(jq -r '.some_var' "$AMPLIHACK_CONTEXT_FILE")"
   ```

   Each drop is logged by **name and size only** (see [Logging & privacy](#logging--privacy)).

2. **Fail loud if protected variables still overflow.** If, after every
   non-protected variable has been dropped, the protected variables *alone*
   still exceed the budget, the runner does **not** spawn. It returns a named
   error instead of hitting a raw `E2BIG`.

Because context is written to a file first, the budget should never trip in
normal operation. The fail-loud path is a safety net for pathological cases.

---

## Fail-loud error

When protected variables cannot fit, the subprocess adapter returns an
`EnvBudgetError` *before* any `execve` is attempted.

```text
Error: environment budget exceeded before subprocess spawn.

The following required variable(s) do not fit within the measured
environment budget (262144 bytes):
  - AMPLIHACK_SESSION_STATE (198734 bytes)
  - RECIPE_VAR_PAYLOAD (81002 bytes)

These variables are protected and were not dropped. Move large values into
the file-based context and read them via $AMPLIHACK_CONTEXT_FILE instead of
passing them through the environment.
```

Guarantees:

- The error names the **offending variables and their byte sizes** — but
  **never their values**.
- The message points the operator at `AMPLIHACK_CONTEXT_FILE` as the remedy.
- Raw `execve` `os error 7` can never surface to the user; the guard runs first.
- Protected variables are never silently dropped to "make room".

---

## Logging & privacy

The environment machinery **never logs a variable's value**. All diagnostics —
whether a routine trim or a fail-loud abort — emit only:

- the variable **name**, and
- the variable's **byte size**.

Example trim log:

```text
WARN  env budget: dropping non-protected var "BIG_CACHED_BLOB" (73422 bytes); recoverable via $AMPLIHACK_CONTEXT_FILE
```

This invariant is enforced by a dedicated test that asserts no value substring
ever appears in emitted logs or error messages.

---

## File-first context recap

The measured budget works together with the existing file-first context
mechanism:

- `RecipeContext::write_context_file()` serializes the full context to
  `${TMPDIR}/amplihack-context-<pid>.json` with `0600` permissions and exports
  `AMPLIHACK_CONTEXT_FILE` pointing at it.
- `shell_env_for_step()` relocates large context to that file and keeps only a
  small subset of critical variables inline (each under 4 KiB), plus
  `AMPLIHACK_CONTEXT_FILE`.
- The caller cleans up the temp file after the step completes.

Reading context in a bash step:

```bash
# Full context is available as JSON:
cat "$AMPLIHACK_CONTEXT_FILE"

# Pull a single value:
task="$(jq -r '.task_description' "$AMPLIHACK_CONTEXT_FILE")"
```

---

## Configuration

The budget is **fully automatic** — there is nothing to configure for normal
use. It adapts to each host's `ARG_MAX` and stack rlimit.

Operators who want a larger effective budget can raise the stack limit before
launching the runner:

```bash
# Inspect the host's kernel argument-list limit:
getconf ARG_MAX

# Raise the stack rlimit (raises RLIMIT_STACK / 4, which may raise the budget):
ulimit -s unlimited
```

There is intentionally **no** environment variable or config-file knob to
override the budget in production. The only override is a test-only seam (see
below), which is compiled out of release builds.

### Test-only seam

For unit testing, the budget can be overridden via a `#[cfg(test)]`-only
setter that shrinks the effective ceiling so the fail-loud path can be
exercised deterministically:

```rust
#[cfg(test)]
fn set_env_byte_budget_for_test(bytes: usize);
```

This seam is a `OnceLock` override guarded by the test environment mutex. It is
**not** an environment variable — deliberately, so it can never leak into a
child process or be triggered in production.

---

## Behavior summary

| Situation                                              | Outcome                                                                  |
| ------------------------------------------------------ | ------------------------------------------------------------------------ |
| Env fits within the measured budget                    | Spawn proceeds normally.                                                 |
| Env over budget, only non-protected vars overflow      | Non-protected vars dropped largest-first (lossless), names+sizes logged. |
| Env over budget, protected vars alone overflow         | **Fail loud**: `EnvBudgetError` naming the vars; no spawn; no `E2BIG`.   |
| `sysconf` / `getrlimit` unavailable                    | Budget falls back to the 256 KiB floor.                                  |
| Oversized inherited parent env + tiny command          | Sanitized baseline keeps the spawn well under budget; command succeeds.  |

---

## Known limitations & follow-up

- **Context temp-file TOCTOU (R7).** `write_context_file()` uses a predictable
  path (`${TMPDIR}/amplihack-context-<pid>.json`) and creates it with `0600`
  permissions. On a shared `TMPDIR` this leaves a small time-of-check /
  time-of-use and symlink-following window between path selection and file
  creation. The planned hardening is to create the file with `O_EXCL` (refusing
  to follow an existing symlink) and/or a random suffix instead of the bare PID.
  This does not affect the env-budget guarantees above; it is tracked as a
  separate security follow-up.

---

## Related

- [Architecture](architecture.md) — where the subprocess adapter and context
  layer sit in the overall design.
- [CLI Reference](cli-reference.md) — invoking recipes that spawn subprocesses.
