# Migration to 0.6.0 (upcoming)

These changes ship in the next minor release, 0.6.0.

Version 0.6.0 changes how bash steps receive large recipe values, and how
the parser reports unknown step fields. Most recipes need no changes. This
page lists every breaking change and how to update.

| Change | Who is affected |
| --- | --- |
| [`AMPLIHACK_CONTEXT_FILE` is no longer set](#amplihack_context_file-is-no-longer-set) | Bash scripts that read the context JSON file |
| [Unknown step field warnings have new wording](#unknown-step-field-warnings-have-new-wording) | Tools that match warning text |
| [`shell_env_for_step` returns a `HashMap`](#shell_env_for_step-returns-a-hashmap) | Rust library users |
| [`RecipeParser` is no longer a unit struct](#recipeparser-is-no-longer-a-unit-struct) | Rust library users |

For background, see
[Environment Budget & Safe Spawning](env-budget.md#per-value-env-spill-bash-steps).

---

## `AMPLIHACK_CONTEXT_FILE` is no longer set

Before 0.6.0, when a step's recipe values were too large in total for the
environment, the runner wrote the whole context to a JSON file, set
`AMPLIHACK_CONTEXT_FILE` to its path, and passed only four variables inline.
Every other `RECIPE_VAR_*` was empty in that step.

From 0.6.0, every recipe variable is always available as `$RECIPE_VAR_<name>`.
Oversized values are read back from private spill files before your command
runs. Step execution never sets `AMPLIHACK_CONTEXT_FILE`.

A script that still reads the file breaks:

- With `set -u`, `"$AMPLIHACK_CONTEXT_FILE"` fails with
  `AMPLIHACK_CONTEXT_FILE: unbound variable`.
- Without `set -u`, the path is empty, so `jq` fails to open `''`, or the
  script quietly gets an empty value.

### Before

```yaml
- id: summarise
  command: |
    set -euo pipefail
    summary=$(jq -r '.report' "$AMPLIHACK_CONTEXT_FILE")
    printf '%s\n' "$summary" | head -n 20
```

### After

Read the variable directly:

```yaml
- id: summarise
  command: |
    set -euo pipefail
    summary=$RECIPE_VAR_report
    printf '%s\n' "$summary" | head -n 20
```

Nested values use `__` as the separator, so `.build.log` becomes
`$RECIPE_VAR_build__log`.

If a script parses a JSON-valued variable, pipe it in on stdin instead of
exporting it. A spilled variable is shell-local, so it is not passed to child
processes through the environment:

```bash
printf '%s' "$RECIPE_VAR_report" | jq -r '.summary'
```

### Scripts that must run on both versions

```bash
if [ -n "${AMPLIHACK_CONTEXT_FILE:-}" ]; then
  summary=$(jq -r '.report' "$AMPLIHACK_CONTEXT_FILE")   # before 0.6.0
else
  summary=$RECIPE_VAR_report                              # 0.6.0 and later
fi
```

---

## Unknown step field warnings have new wording

The parser now reports unknown step fields during every parse, not only under
`--validate-only`. Unknown fields are still warnings, and `--validate-only`
still exits `0`. The step-field message changed:

| | Before 0.6.0 | 0.6.0 |
| --- | --- | --- |
| Unknown step field | `Step 'build': unrecognized field 'comand' (possible typo)` | `recipe 'deploy': step 'build' has unknown field 'comand' (ignored)` |
| Step without an `id` | `Step 'index 2': ...` | `recipe 'deploy': step '<index 2>' ...` |
| `max_env_value_bytes` | generic unknown-field message | specific "retired and ignored" message |

Update any CI check or log filter that matches `(possible typo)` or
`index N` in step-field warnings. The full rules (escaping, the 128-character
limit, the 100-warning cap) are in
[Unknown and retired step fields](yaml-format.md#unknown-and-retired-step-fields).

The top-level field warning, `Unrecognized top-level field '<field>' (possible
typo)`, is unchanged.

If a recipe sets `max_env_value_bytes`, delete the field. The runner now keeps
every environment string under the kernel limit without truncating values.

---

## `shell_env_for_step` returns a `HashMap`

`RecipeContext::shell_env_for_step()` no longer returns a temp-file path,
because no context file is written.

```rust
// Before 0.6.0
let (env, context_file): (HashMap<String, String>, Option<PathBuf>) =
    ctx.shell_env_for_step();
// ... run the step, then delete `context_file` if it is Some

// 0.6.0
let env: HashMap<String, String> = ctx.shell_env_for_step();
```

Remove any cleanup code for the returned path. If you need the context as a
JSON file for your own purposes, `RecipeContext::write_context_file()` is
still public.

---

## `RecipeParser` is no longer a unit struct

`RecipeParser` now has a private field that controls the parse-time
warnings. The bare expression `RecipeParser` no longer compiles:

```rust
// Before 0.6.0
let parser = RecipeParser;

// 0.6.0
let parser = RecipeParser::new();          // or RecipeParser::default()
```

To parse without printing step-field warnings, for example when you report
them yourself:

```rust
let parser = RecipeParser::new().without_field_warnings();
```

---

## Related

- [Environment Budget & Safe Spawning](env-budget.md): how spilling works.
- [YAML Recipe Format](yaml-format.md#unknown-and-retired-step-fields): the
  warning rules.
- [Migration to 0.5.0](migration-v0.5.md): the previous migration guide.
