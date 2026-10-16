# Environment Budget & Safe Subprocess Spawning

The recipe runner starts `bash` and agent subprocesses for many steps. On Linux
the kernel puts two limits on the environment of a new process. If either is
exceeded, `execve(2)` fails with `E2BIG` (`Argument list too long (os error 7)`)
and the child never starts:

| Limit | Value on Linux | What it caps |
| --- | --- | --- |
| `ARG_MAX` (and `RLIMIT_STACK / 4`) | usually 2 MiB | the **total** size of argv plus the environment |
| `MAX_ARG_STRLEN` | 131072 bytes (128 KiB), fixed | **each single** argv or env string, `KEY=value` |

The second limit applies even when the total is well under `ARG_MAX`. One
200 KiB step output stored in `RECIPE_VAR_report` is enough to make `execve`
fail.

This page explains how the runner keeps every spawn under both limits:

1. **Sanitized environment.** Before every spawn the inherited parent
   environment is cleared (`env_clear`) and rebuilt from a bounded set.
   Nothing leaks in from the parent implicitly.
2. **Per-value env spill.** For bash steps, any recipe value whose `KEY=value`
   string is too large for one env string, and the largest remaining values
   when the total is over budget, are written to private files. A short
   preamble reads them back into shell variables of the same name before your
   script runs. Recipe scripts need no changes.
3. **A measured budget.** The total-size ceiling comes from the host's real
   kernel limits at runtime. It isn't hardcoded.
4. **Fail-loud enforcement.** If a variable that can't be spilled can't fit
   either, the runner refuses to spawn and gives a named error. It never lets a
   raw `E2BIG` reach you.

As a result, a recipe never dies with an opaque `os error 7`, and a script
never silently sees an empty value where its recipe set one.

---

## The measured budget

The environment byte budget is computed **once per process** from the host's
real limits and cached in a `OnceLock`.

### Formula

```text
budget = min( sysconf(_SC_ARG_MAX), RLIMIT_STACK / 4 )
         - argv_reserve
```

| Term             | Meaning                                                                                 |
| ---------------- | --------------------------------------------------------------------------------------- |
| `sysconf(_SC_ARG_MAX)` | The kernel's maximum combined argv + env size for a new process.                  |
| `RLIMIT_STACK / 4`     | Linux also caps argv+env at 1/4 of the stack rlimit. The smaller of the two wins.  |
| `argv_reserve`   | A fixed **128 KiB** reserved for the command path and its arguments.                    |

Every spawn calls `Command::env_clear()` and installs **only** the curated
environment, so that curated set *is* the child's whole environment block.
There's no separately inherited baseline to subtract.

The computation uses **saturating** arithmetic, and the result is **clamped to
a floor of 256 KiB**.

### Conservative fallback

If `sysconf(_SC_ARG_MAX)` or `getrlimit(RLIMIT_STACK)` is unavailable or returns
a non-positive value, the budget falls back to the **256 KiB floor**.

### How sizes are measured

Every size check (the per-value cap, the budget planner and `bounded_env`)
counts a variable the way the kernel does, including the `=` and the
terminating NUL:

```text
env_pair_bytes(KEY, value) = len(KEY) + 1 + len(value) + 1     // "KEY=value\0"
```

There is exactly one definition, in `adapters::env_spill`, so the checks always
agree.

### Single source of truth

```rust
/// Returns the measured environment byte budget for child processes.
///
/// Computed once from `sysconf(_SC_ARG_MAX)` and `RLIMIT_STACK`, cached in a
/// `OnceLock`.
pub(crate) fn env_byte_budget() -> usize
```

The bash-step spill planner and `bounded_env` both call `env_byte_budget()`.
No second copy of the limit exists.

---

## Per-value env spill (bash steps)

### When a value is spilled

Each bash step gets its environment by merging the recipe values
(`RECIPE_VAR_<name>`, nested `RECIPE_VAR_<a>__<b>`, and the legacy uppercase
alias `<NAME>`) over the sanitized inherited environment. The runner then plans
a spill in two phases:

1. **Per-value cap.** Every recipe value with
   `env_pair_bytes(KEY, value) > PER_VALUE_ENV_CAP` is spilled, whatever the
   total is.

   ```text
   MAX_ARG_STRLEN    = 131072
   PER_VALUE_ENV_CAP = MAX_ARG_STRLEN - 4096 = 126976
   ```

2. **Total budget.** If the whole merged environment (inherited plus remaining
   recipe values) is still over `env_byte_budget()`, the remaining recipe values
   are spilled largest first (ties go to the name that sorts first) until the
   total fits.

The plan is deterministic. Only recipe values (the step's `extra_env`) are
candidates; inherited variables are never spilled. Empty values are never
spilled. Because the spill runs before `bounded_env`, the runner moves recipe
values to files rather than dropping ambient variables: a non-protected
inherited variable is trimmed only if the environment is still over budget
after every spillable recipe value has been moved.

`RECIPE_VAR_x` and its legacy alias `X` hold the same value but are planned
independently, each with its own file. Each is judged on its own
`KEY=value` size, and that size includes the key. A large value usually
spills under both names. Near the cap, though, `RECIPE_VAR_x=value` (11 bytes
of name) can be over while `X=value` is still under, so only the longer name
spills. Budget-only spilling can also leave the shorter alias inline. Either
way, both names give the script the same value.

When nothing exceeds the cap or the budget, nothing changes: no spill
directory is created, and the script and the environment are byte-identical
to what they would be without this feature.

### What the script sees

The runner removes each spilled variable from the child environment and puts
one preamble line per variable in front of your `command:`:

```bash
{ IFS= read -r -d '' RECIPE_VAR_report || :; } < '/tmp/amplihack-env-spill-Xa81Qz/v0' || { printf 'amplihack: failed to load spilled env value RECIPE_VAR_report\n' >&2; exit 125; }
{ IFS= read -r -d '' REPORT || :; } < '/tmp/amplihack-env-spill-Xa81Qz/v1' || { printf 'amplihack: failed to load spilled env value REPORT\n' >&2; exit 125; }
# ... your command: follows unchanged
```

`IFS= read -r -d ''` reads the whole file into the variable without splitting
it, without interpreting backslashes, and without dropping leading or trailing
whitespace. **The value is byte-identical, trailing newlines included.**
Quotes, `$()`, backticks, `*`, `;` and embedded newlines come back exactly as
they went in, because value bytes only ever live in the file and never in shell
text. The only shell text in the preamble is the fixed `read` syntax, a
validated variable name, and a single-quoted path (with `'` written as
`'\''`).

The preamble **fails closed**. If a spill file can't be opened, the step exits
with status **125**, prints a message naming only the variable, and your
command does not run. The step never continues with an empty value.

A recipe that writes a large report and reads it in a later step needs no
changes:

```yaml
name: big-report
steps:
  - id: gen
    command: python3 scripts/build_report.py   # prints ~200 KiB
    output: report
  - id: check
    command: |
      printf '%s' "$RECIPE_VAR_report" | sha256sum
      printf '%s' "$REPORT" | wc -c          # legacy alias, same bytes
```

> **Note:** The runner trims captured step output before storing it. A value
> that came from a step's output therefore has no trailing whitespace. Values
> set some other way, such as `--set` or a sub-recipe `context:`, keep their
> trailing newlines.

The preamble applies to every path that goes through `execute_bash_step`:
sequential bash steps, `parallel_group` bash steps, and `pre_step`/`post_step`
hooks.

### Spilled variables are shell-local, not exported

A spilled variable is an ordinary **shell variable** inside the step script. It
isn't exported, so child processes of your script (`jq`, `git`, `python3`)
don't inherit it. Variables that weren't spilled are exported exactly as
before.

This is deliberate:

| If spilled values were exported... | Result |
| --- | --- |
| A value over the per-value cap | Every later external command in the script fails with the same `E2BIG`, so the bug just moves into your script. |
| A value spilled because of the total budget | Children can be pushed back over `ARG_MAX`. |

No working recipe can depend on the export. Before this feature these values
either crashed `execve` or were silently replaced by an empty string, so no
child ever saw them.

To pass a spilled value to a child process, use stdin or a file instead of the
environment:

```bash
printf '%s' "$RECIPE_VAR_report" | jq -r '.summary'
printf '%s' "$RECIPE_VAR_report" > "$tmp/report.json" && python3 check.py "$tmp/report.json"
```

`export RECIPE_VAR_report` is still possible. A child started after that
fails with `E2BIG` if `RECIPE_VAR_report=value\0` is over `MAX_ARG_STRLEN`
(131072 bytes, name included), or if the export pushes the total over
`ARG_MAX`. `PER_VALUE_ENV_CAP` is the runner's own safety margin and doesn't
apply here. A value spilled under the cap but still under `MAX_ARG_STRLEN`
exports without error. That's under your script's control and outside the
runner's guarantees.

### Names that can't be spilled

A recipe value can only be read back into a shell variable of the same name if
the name is a valid identifier that bash allows you to assign. A name is
spillable when:

- it matches `^[A-Za-z_][A-Za-z0-9_]*$` (ASCII only), and
- it isn't one of `IFS PATH HOME EUID PPID SHELLOPTS UID GROUPS FUNCNAME
  LINENO RANDOM SRANDOM SECONDS EPOCHSECONDS EPOCHREALTIME HISTCMD DIRSTACK
  OPTIND OPTARG PS4 ENV POSIXLY_CORRECT CDPATH GLOBIGNORE`, and
- it doesn't start with `BASH` (which covers `BASHOPTS`, `BASHPID` and
  `BASH_*`) or `COMP_`.

A recipe value whose name fails these rules and is over the per-value cap is
**dropped from the child environment**, with one warning naming the variable
and its size. The step still starts. Such a value couldn't have reached the
script before this feature either, because the spawn would have failed.

### Values containing NUL bytes

`read -d ''` stops at the first NUL byte, so a NUL can't round-trip through a
spill file. If a value that has to be spilled contains a NUL, the step fails
before spawning with an error naming the variable and its length. The value
is never silently truncated.

### Spill files

| Property | Value |
| --- | --- |
| Location | `${TMPDIR:-/tmp}/amplihack-env-spill-<random>/`, one directory per step invocation |
| Directory mode | `0700` |
| File names | `v0`, `v1`, ... by index. Variable names never appear in paths. |
| File mode | `0600`, created with `O_CREAT \| O_EXCL` |
| Contents | the raw value bytes, nothing else |
| Cleanup | removed when the step finishes: on success, non-zero exit, `timeout:` expiry, and any runner-side error after the spill (including a `bounded_env` failure) |

Parallel steps each get their own directory. Spill files are never written
into the step's `working_dir`, and spilled values are never copied into the
temporary script file used for large scripts (below).

If the runner process itself is killed with `SIGKILL`, it can't clean up, and
the directory is left in `TMPDIR`. Its contents are still private (`0600` files
in a `0700` directory). If `TMPDIR` points at a world-writable directory
without the sticky bit, other local users can rename or delete entries in it.
Use a private or sticky `TMPDIR` on shared hosts.

### Interaction with large scripts

Commands over 64 KiB are already written to a private temporary script file
instead of being passed as `bash -c <script>`. The 64 KiB check is applied to
the **combined** script (preamble plus your command), so the preamble always
runs first, whether the script is inline or file-backed.

### `$LINENO` and shebangs

When something is spilled, the preamble adds one line per spilled variable in
front of your command:

- `$LINENO` and bash error messages (`line 12: ...`) are offset by that many
  lines.
- A `#!` line at the top of `command:` stops being the first line and becomes
  an ordinary comment. Bash steps always run under the resolved
  [bash interpreter](cli-reference.md#amplihack_bash), so it never selected the
  interpreter anyway.

Neither applies when nothing is spilled.

### No more context-file fallback

Earlier versions handled an over-budget context differently. They wrote the
whole context to an `AMPLIHACK_CONTEXT_FILE` JSON file and kept only four
variables inline, so every other `RECIPE_VAR_*` silently became empty. That
fallback has been **removed**:

- Every recipe variable is available to every bash step, either inline or
  through the preamble.
- Step execution no longer sets `AMPLIHACK_CONTEXT_FILE`.
- `RecipeContext::shell_env_for_step()` returns the full
  `HashMap<String, String>` of shell variables. It used to return a
  `(HashMap<String, String>, Option<PathBuf>)` tuple.
- `RecipeContext::write_context_file()` remains public API for library users.
  Step execution doesn't call it.

Scripts that read `$AMPLIHACK_CONTEXT_FILE` must switch to `$RECIPE_VAR_<name>`.
See [Migration to 0.6.0](migration-v0.6.md#amplihack_context_file-is-no-longer-set).

---

## Agent steps

Agent subprocesses don't receive `RECIPE_VAR_*` values. Recipe data reaches an
agent through its prompt, and prompts over `MAX_INLINE_AGENT_PROMPT_BYTES`
already go through a file. An agent child's environment is the sanitized
inherited environment plus `AMPLIHACK_AGENT_BINARY`.

That inherited environment is filtered before any step-specific variable is
added. A variable is dropped, with a warning naming it and giving its size, when
its whole `KEY=value\0` string is longer than `MAX_ARG_STRLEN`:

```text
env_pair_bytes(KEY, value) = len(KEY) + 1 + len(value) + 1 > 131072
```

This is the same check the kernel applies in `execve`, so the runner drops only
inherited variables that no child could receive. The rule counts the key, `=`
and the NUL, so a 1,000-byte name with a 130,071-byte value (131,073 bytes in
all) is dropped, while the same name with a 130,070-byte value (exactly 131,072
bytes) is passed through. `PER_VALUE_ENV_CAP` does not apply to inherited
variables. That 4 KiB margin governs only the recipe values the runner spills.
The filter applies to bash and agent children alike. For a bash step, the
interpreter is resolved first, from the unfiltered parent environment, and the
filter and the `extra_env` merge follow (see
[Order of operations for a bash step](#order-of-operations-for-a-bash-step)).

---

## Protected variables

Some variables are **protected**. They're needed for correct nested execution,
and `bounded_env` never drops them to fit the budget.

A variable is protected if its name is one of:

- `PATH`
- `HOME`
- `TASK_DESCRIPTION`
- `REPO_PATH`
- `TASK_TYPE`
- `WORKSTREAM_COUNT`

…or if its name starts with one of these prefixes:

- `AMPLIHACK_` (e.g. `AMPLIHACK_SESSION_DEPTH`, `AMPLIHACK_TREE_ID`, `AMPLIHACK_BASH`)
- `RECIPE_VAR_`

`PATH` is protected so that budget trimming can never change binary resolution
in the child. For bash steps the one live dependency is the `timeout` binary: a
step with `timeout:` spawns `timeout <secs> <bash> …`, and `timeout` is looked
up by name against the **child's** `PATH`.

The bash interpreter itself does **not** depend on this. It's resolved to an
absolute path against the *parent's* `PATH` before the child environment is
built (see
[Bash interpreter resolution](architecture.md#bash-interpreter-resolution)).
That's also why the spill happens after interpreter resolution: a recipe value
can never choose the interpreter.

`AMPLIHACK_BASH` is protected by the `AMPLIHACK_` prefix rule, so an operator's
interpreter pin survives trimming.

Everything else is **non-protected** and can be trimmed.

---

## Order of operations for a bash step

```text
resolve_bash_interpreter          (parent env only)
build_child_env                   (sanitized inherited env, per-string filter)
merge extra_env                   (RECIPE_VAR_*, nested, legacy aliases)
plan + write spill                (per-value cap, then budget; 0600 files)
script = preamble + command
bounded_env                       (unchanged: trims non-protected inherited vars)
64 KiB inline-vs-file decision    (on the combined script)
spawn, wait
drop script file, drop spill dir
```

`bounded_env` runs after the spill and behaves exactly as before. Because the
recipe values that would overflow have already been moved to files, it only
has to deal with inherited variables:

1. **Drop non-protected inherited variables largest-first** until the total
   fits. Each drop is logged by name and size only. These are parent-process
   variables, not recipe values.
2. **Fail loud if protected variables still overflow.** If the protected
   variables *alone* exceed the budget after every non-protected variable has
   been dropped, the runner doesn't spawn and returns `EnvBudgetError`.

---

## Fail-loud error

The message below is illustrative. Its content is guaranteed (the offending
names, their sizes and the budget, plus advice to move large inherited
values out of the environment); its exact wording isn't.

```text
Error: environment budget exceeded before subprocess spawn.

The following required variable(s) do not fit within the measured
environment budget (262144 bytes):
  - AMPLIHACK_SESSION_STATE (198734 bytes)

These variables are protected and were not dropped. Recipe values are
spilled to private files automatically; large inherited or protected
variables must be moved out of the environment.
```

Guarantees:

- The error names the **offending variables and their byte sizes**, never
  their values.
- A raw `execve` `os error 7` never reaches you, because the guard runs first.
- Protected variables are never silently dropped to make room.
- The message doesn't point to `AMPLIHACK_CONTEXT_FILE`, which no longer
  exists.

---

## Logging & privacy

The environment code **never logs a variable's value**. Diagnostics only ever
contain variable names, byte sizes, and (at `debug`) the spill directory.

The examples below are **illustrative**. The events, levels and the fields
they carry (names and byte sizes, never values) are guaranteed; the exact
wording isn't, so don't match on it in scripts.

| Event | Level | Illustrative message |
| --- | --- | --- |
| Values spilled for a step | `info` | `env spill: 2 value(s) moved to private files: RECIPE_VAR_report (204800 bytes), REPORT (204800 bytes)` |
| Spill directory | `debug` | `env spill dir: /tmp/amplihack-env-spill-Xa81Qz` |
| Unspillable name dropped | `warn` | `env spill: dropping BASH_REPORT (204800 bytes): name cannot be restored as a shell variable` |
| Inherited var over `MAX_ARG_STRLEN` | `warn` | `dropping inherited env var "BIG_BLOB" (KEY=value\0 is 140010 bytes): exceeds MAX_ARG_STRLEN (131072)` |
| Non-protected inherited var trimmed | `warn` | `env budget: dropping non-protected var "BIG_CACHED_BLOB" (73422 bytes)` |

Spill errors name the variable and its byte length, plus the underlying I/O
error, for example `failed to spill NAME (LEN bytes): <io error>`.
Types that hold values don't derive `Debug`. A sentinel test checks that no
value bytes show up in logs or in the error chain.

---

## Retired: `max_env_value_bytes`

Some recipes set a per-step `max_env_value_bytes: 65536`. An older in-process
executor honoured it to avoid `E2BIG` at checkpoint steps. This runner
**ignores the field**, and the parser warns about it once per step:

```text
recipe 'workflow-tdd': step 'checkpoint-3': field 'max_env_value_bytes' is retired and ignored — the runner now spills oversized env values to private files automatically; remove it from the recipe
```

It's retired, not implemented, because automatic per-value spilling already
keeps every env string under `MAX_ARG_STRLEN` with no per-step setting.
Implementing the field would only bring back lossy truncation of values. You
can remove it from your recipes. See
[Unknown and retired step fields](yaml-format.md#unknown-and-retired-step-fields).

---

## Configuration

The budget and the spill are **fully automatic**, with nothing to configure in
normal use. Operators who want a larger total budget can raise the stack limit
before starting the runner:

```bash
# Inspect the host's kernel argument-list limit:
getconf ARG_MAX

# Raise the stack rlimit (raises RLIMIT_STACK / 4, which may raise the budget):
ulimit -s unlimited
```

Spill files go to `TMPDIR`, so set `TMPDIR` to put them somewhere else.

There's intentionally **no** environment variable or config-file setting to
override the budget or the per-value cap. `MAX_ARG_STRLEN` is a fixed kernel
constant.

### Test-only seams

Two `#[cfg(test)]`-only seams make the spill paths deterministic in unit tests:

```rust
#[cfg(test)]
fn set_env_byte_budget_for_test(bytes: usize);   // hold ENV_BUDGET_TEST_LOCK

#[cfg(test)]
struct SpillRootGuard;  // thread-local spill-root override, restored on Drop
```

Neither is an environment variable, so neither can leak into a child process
or be triggered in production.

---

## Behavior summary

| Situation | Outcome |
| --- | --- |
| Everything fits under the cap and the budget | Spawn proceeds with an identical env and script. No files are created. |
| One recipe value's `KEY=value` > 126976 bytes | That value is spilled and read back by the preamble, byte-identical. |
| Recipe values over budget in total | The largest are spilled until the total fits. Every variable stays readable. |
| Unspillable name over the cap | Dropped with a warning (name and size). The step still runs. |
| Value that has to be spilled contains NUL | The step fails before spawning, naming the variable and its length. |
| Spill file missing when the step starts | The step exits 125 naming the variable. The command doesn't run. |
| Non-protected inherited vars over budget | Dropped largest-first, with names and sizes logged. |
| Protected vars alone over budget | **Fail loud**: `EnvBudgetError` naming the vars, no spawn, no `E2BIG`. |
| Inherited var's `KEY=value\0` > 131072 bytes (`MAX_ARG_STRLEN`) | Filtered out of every child env, with a warning. Smaller inherited vars pass through unchanged. |
| `sysconf` / `getrlimit` unavailable | The budget falls back to the 256 KiB floor. |

---

## Related

- [Architecture](architecture.md): where the subprocess adapter and context
  layer sit in the overall design.
- [YAML Recipe Format](yaml-format.md#unknown-and-retired-step-fields): parse-time
  warnings for unknown and retired step fields.
- [CLI Reference](cli-reference.md): running recipes that spawn subprocesses.
- [`AMPLIHACK_BASH`](cli-reference.md#amplihack_bash): choosing the interpreter
  those bash subprocesses run under.
