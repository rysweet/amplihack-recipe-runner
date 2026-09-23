# Step Outputs in the Environment

Every value in a recipe's context is exported into the environment of every bash
step the runner spawns. A step that declares `output: my_result` can be read by
any later bash step without a template, and without depending on `jq`.

This page is the reference for how a context value becomes an environment
variable: the two names it is given, how each JSON type is encoded, which keys
are skipped, and what happens when the value is too large to pass through the
environment at all.

The rules are the same whether the value came from a plain string output, from
a step with `parse_json: true`, from the recipe's `context:` block, or from a
`-c key=value` argument on the command line. The runner does not distinguish
between them.

---

## The two names

For a top-level context key `k`, the runner exports:

| Name | When | Purpose |
|------|------|---------|
| `RECIPE_VAR_<k>` | for every key | Canonical accessor. Never suppressed by a naming rule. |
| `<K>` — the key upper-cased | when `k` is a shell identifier and `K` is not reserved | Plain-name alias, for recipes written against the legacy Python runner. |

Both names carry the identical value. `RECIPE_VAR_<k>` is the one to reach for
in new recipes: no key, character or collision suppresses it, and it survives
environment pressure that removes aliases. The one condition that removes it as
well is the whole context spilling to a file, covered in
[When the value does not fit](#when-the-value-does-not-fit).

In the canonical name, `.` and `-` in the key are both replaced with `_`
(`.` becomes `__`), so `api.region` is exported as `RECIPE_VAR_api__region`.

```yaml
name: two-names
version: "1.0"
description: Both exported names carry the same value

steps:
  - id: produce
    command: printf 'release\n'
    output: build_mode

  - id: consume
    command: |
      echo "canonical=$RECIPE_VAR_build_mode"
      echo "alias=$BUILD_MODE"
```

```text
canonical=release
alias=release
```

---

## Value encoding by JSON type

The context holds `serde_json` values. Each is encoded into its environment
string as follows, and both exported names get the same encoding.

| JSON type | Environment value | Example value → exported string |
|-----------|-------------------|----------------------------------|
| String | The string itself, unquoted and unescaped | `"hello world"` → `hello world` |
| Number | Its JSON rendering | `42` → `42` |
| Boolean | `true` or `false` | `true` → `true` |
| Null | The empty string | `null` → *(empty)* |
| Object | Compact JSON: no spaces, no newlines | `{"ok": true}` → `{"ok":true}` |
| Array | Compact JSON: no spaces, no newlines | `[1, 2]` → `[1,2]` |

Objects and arrays are serialised with `serde_json::Value::to_string`, which is
byte-for-byte what `{{template}}` substitution produces for the same value. A
recipe can read a JSON output either way and get the same characters.

Object keys are emitted in **sorted order**, not the order they appeared in the
step's output. Array element order is preserved. A step that matches on the
serialised text should therefore not assume the producer's field order.

An empty environment variable is therefore ambiguous between three cases: the
key held JSON `null`, the key held the empty string, or the key was not exported
at all. Test with `[ -n "$VAR" ]` when any of those should stop the step, and
see [When the value does not fit](#when-the-value-does-not-fit) for the third
case.

---

## Reading a JSON output

A step with `parse_json: true` stores a parsed object or array in the context.
The later bash step reads it as compact JSON from the environment.

`parse_json` is not the only route to a JSON value. The runner attempts to parse
**every** step output as JSON and stores the parsed value when that succeeds,
falling back to a plain string when it does not. A step that prints `{"a":1}` or
`42` lands an object or a number in the context whether or not it declared
`parse_json`. What the flag adds is extraction from noisy output and the
`parse_json_required` failure gate, not the storage behaviour — so the rules on
this page apply to any step whose output happens to be valid JSON.

```yaml
name: json-output-to-env
version: "1.0"
description: A parse_json output reaches a later bash step as compact JSON

steps:
  - id: preflight
    command: printf '{"should_run":true,"state_dir":"/tmp/run-42"}\n'
    output: my_preflight
    parse_json: true

  - id: consume
    command: |
      echo "ENV MY_PREFLIGHT=${MY_PREFLIGHT:-}"
      echo "ENV RECIPE_VAR_my_preflight=${RECIPE_VAR_my_preflight:-}"
```

```text
ENV MY_PREFLIGHT={"should_run":true,"state_dir":"/tmp/run-42"}
ENV RECIPE_VAR_my_preflight={"should_run":true,"state_dir":"/tmp/run-42"}
```

Arrays behave the same way:

```yaml
  - id: preflight-list
    command: printf '[{"a":"x","state_dir":"/tmp/sd"}]\n'
    output: my_preflight
    parse_json: true
```

```text
ENV MY_PREFLIGHT=[{"a":"x","state_dir":"/tmp/sd"}]
```

### The preflight-gate pattern

This is the shape used by the amplihack auto-drive recipes: one step emits a
JSON preflight verdict, the next reads a field out of it and refuses to proceed
if the field is missing.

```yaml
name: preflight-gate
version: "1.0"
description: Gate a step on a field of a JSON preflight output

steps:
  - id: crusty-loop-preflight
    command: |
      printf '{"should_run":true,"state_dir":"%s"}\n' /tmp/crusty-1468
    output: crusty_loop_preflight
    parse_json: true

  - id: crusty-loop
    command: |
      DIR="$(printf '%s' "${CRUSTY_LOOP_PREFLIGHT:-}" \
        | sed -n 's/.*"state_dir"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
      [ -n "$DIR" ] || {
        echo "ERROR: preflight produced no state_dir; refusing to run an unrecorded loop." >&2
        exit 1
      }
      echo "state_dir=$DIR"
```

```text
state_dir=/tmp/crusty-1468
```

Two things make this work and are worth copying:

- The value is read through a **quoted** expansion and piped into a parser. It
  is never `eval`ed, never word-split, and never placed in command position.
- The guard is **fail-closed**. If the variable is absent or the field is
  missing, the step exits non-zero with a message naming the cause, rather than
  continuing with an empty directory path.

---

## Nested object access

When a top-level value is an object, the runner additionally flattens it,
recursively, with `__` between levels. Intermediate objects are exported both as
their own compact JSON and as their flattened children. Arrays are not
flattened; an array is exported as compact JSON and nothing else.

```yaml
name: nested-access
version: "1.0"
description: Nested fields of an object output are exported individually

steps:
  - id: config
    command: printf '{"region":"eu-west-1","limits":{"rps":10}}\n'
    output: api_config
    parse_json: true

  - id: use
    command: |
      echo "whole=$API_CONFIG"
      echo "region=$RECIPE_VAR_api_config__region"
      echo "limits=$RECIPE_VAR_api_config__limits"
      echo "rps=$RECIPE_VAR_api_config__limits__rps"
```

```text
whole={"limits":{"rps":10},"region":"eu-west-1"}
region=eu-west-1
limits={"rps":10}
rps=10
```

Note the sorted keys in `whole`: the step emitted `region` first.

Flattened names are exported under the `RECIPE_VAR_` prefix only. There is no
`API_CONFIG__REGION` alias — the plain-name alias applies to top-level keys
alone.

The flattened prefix is built from the key **as written**, without the `.` and
`-` substitution that produces the canonical name. The two prefixes agree only
when the key is already `[A-Za-z0-9_]`. A key `api-config` is exported
canonically as `RECIPE_VAR_api_config` but flattens to
`RECIPE_VAR_api-config__region`; `api.thing` gives `RECIPE_VAR_api__thing`
alongside `RECIPE_VAR_api.thing__region`. Those flattened names contain
characters no shell will accept in `$NAME`, so the fields are reachable only
through `env` or the context file. Name object-valued keys with letters, digits
and underscores and the problem does not arise.

---

## When an alias is not exported

No naming rule suppresses `RECIPE_VAR_<k>`. The plain uppercase alias is skipped
in three cases.

**1. The key is not a shell identifier.** The key must be non-empty, consist
only of `[A-Za-z0-9_]`, and not start with a digit. A key such as `api.region`,
`build-mode` or `2fa` gets no alias; use the canonical name.

**2. The uppercase name is reserved.** These names are skipped so a recipe
cannot break the shell it is running in:

```text
PATH     HOME     PWD      OLDPWD   USER     LOGNAME  SHELL    TERM
TMPDIR   TMP      LANG     LC_ALL   LC_CTYPE MAIL     EDITOR   VISUAL
DISPLAY  HOSTNAME IFS      PS1      PS2      PS3      PS4
```

Any name beginning with `LC_` is also skipped.

**3. Another key already claimed the name.** Two context keys that differ only
in case — `result` and `Result` — produce the same alias. The first one inserted
wins and the second is silently dropped; the canonical `RECIPE_VAR_result` and
`RECIPE_VAR_Result` both remain and are unambiguous. Which one wins is not
deterministic, so do not create such a pair.

An alias never overwrites a `RECIPE_VAR_*` name, and never overwrites another
alias.

---

## Precedence against the inherited environment

A bash step does not start from an empty environment. The runner takes its own
environment as the base, drops `CLAUDECODE` and any single inherited value over
128 KiB, ensures `HOME` and `PATH` are non-empty, and merges the recipe's
context over the top.

Context values therefore win. A context key named `scope` shadows any `SCOPE`
the operator had set in their own shell, for that step only; the operator's
shell is untouched, and the next step rebuilds the merge from the same base.

Which names can be shadowed is fixed by the recipe rather than by what a model
happens to print: alias names come from `output:` keys, `context:` keys and `-c`
arguments, never from agent output. Agent output supplies the value, not the
name.

---

## When the value does not fit

Environment variables are not free. Both exported names count against the
measured environment budget, so a value occupies roughly twice its byte length
across `RECIPE_VAR_<k>` and `<K>`.

When the assembled environment exceeds the budget, the runner switches to
file-first context: it writes the whole context to a `0600` JSON file, exports
`AMPLIHACK_CONTEXT_FILE` pointing at it, and keeps only `task_description`,
`repo_path`, `task_type` and `workstream_count` inline, under their canonical
`RECIPE_VAR_` names and only if each is under 4 KiB.

In that mode **every** plain-name alias is absent, including `TASK_DESCRIPTION`.
A step that must tolerate large context should guard the variable and fall back
to the file:

```bash
payload="${API_CONFIG:-}"
if [ -z "$payload" ] && [ -n "${AMPLIHACK_CONTEXT_FILE:-}" ]; then
  payload="$(jq -c '.api_config' "$AMPLIHACK_CONTEXT_FILE")"
fi
[ -n "$payload" ] || { echo "ERROR: api_config unavailable" >&2; exit 1; }
```

A second trim applies independently of the first. Immediately before each spawn
the runner bounds the whole child environment — inherited variables included —
against the same budget, dropping the largest non-protected entries until it
fits. `RECIPE_VAR_*` and `AMPLIHACK_*` names are protected, as are `PATH`,
`HOME`, `TASK_DESCRIPTION`, `REPO_PATH`, `TASK_TYPE` and `WORKSTREAM_COUNT`.
Every other plain-name alias is droppable. A step under environment pressure can
lose `$API_CONFIG` while `$RECIPE_VAR_api_config` survives, which is the second
reason to prefer the canonical name.

The full rules — how the budget is measured, which variables are protected from
trimming, and the fail-loud error when protected variables alone overflow — are
in [Environment Budget & Safe Spawning](env-budget.md).

---

## Environment or template?

Both routes deliver the same characters. They differ in where those characters
end up.

| Route | What the child sees | Use for |
|-------|---------------------|---------|
| `$RECIPE_VAR_k` / `$K` | A process environment entry. The shell never parses the value as source. | Anything whose content the recipe does not control — agent output, API responses, JSON. |
| `{{k}}` outside a heredoc | Renders to `"$RECIPE_VAR_k"` — still an environment read, written for you. | Convenience; identical safety. |
| `{{k}}` in an unquoted heredoc (`<<EOF`) | Renders to `$RECIPE_VAR_k`, unquoted. | Heredoc bodies where surrounding quotes would be literal. |
| `{{k}}` in a quoted heredoc (`<<'EOF'`) | The **value itself** is spliced into the script text. | Short, recipe-controlled values only. |

The last row is the one to avoid for JSON. Inlining a value into a quoted
heredoc body puts arbitrary content into shell source: a line of that value
matching the heredoc delimiter ends the heredoc early, and what follows is
interpreted as script. Reading the same value from the environment has no such
failure mode, which is why JSON outputs are exported rather than templated into
scripts.

---

## Inspecting what a step receives

To see which context keys reached a step, add a throwaway step that lists
names only:

```yaml
  - id: dump-names
    command: env | cut -d= -f1 | grep -E '^(RECIPE_VAR_|AMPLIHACK_)' | sort
```

Every context key appears there as `RECIPE_VAR_<key>`. Its plain-name alias,
where one was exported, is that suffix upper-cased — the aliases themselves are
not matched by the pattern, precisely because they carry no distinguishing
prefix. To confirm one, echo it by name:

```yaml
  - id: dump-one
    command: echo "MY_PREFLIGHT=${MY_PREFLIGHT:-<unset or empty>}"
```

Values are deliberately absent from the name listing, and the runner's own logs
follow the same rule: it logs variable names and byte sizes, never values.

---

## Known limitations

- **Nested keys get no alias.** `RECIPE_VAR_api_config__region` has no
  `API_CONFIG__REGION` counterpart. Only top-level keys are aliased.
- **Flattened names use the raw key.** An object under a key containing `.` or
  `-` flattens to a prefix that differs from its own canonical name and is not a
  legal shell variable name. See
  [Nested object access](#nested-object-access).
- **Case-only key collisions are resolved non-deterministically.** See
  [When an alias is not exported](#when-an-alias-is-not-exported).
- **An empty variable is ambiguous** between a `null` value, an empty string,
  and a key that was spilled to the context file. Guard with `[ -n … ]` and fall
  back to `AMPLIHACK_CONTEXT_FILE`.

---

## Related

- [YAML Recipe Format](yaml-format.md#json-extraction-parse_json) — declaring
  `output:` and `parse_json:` on a step.
- [Environment Budget & Safe Spawning](env-budget.md) — the size ceiling, the
  file-first fallback, and the fail-loud path.
- [Architecture](architecture.md#json-output-extraction) — how output is parsed
  into the context in the first place.
