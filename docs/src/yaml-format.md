# YAML Recipe Format Reference

Complete schema reference for amplihack recipe runner YAML files.

## Top-Level Fields

| Field         | Type             | Required | Default | Description                              |
|---------------|------------------|----------|---------|------------------------------------------|
| `name`        | string           | **yes**  | —       | Recipe name                              |
| `version`     | string           | no       | `"1.0"` | Semantic version                         |
| `description` | string           | no       | `""`    | Human-readable description               |
| `author`      | string           | no       | `""`    | Author name                              |
| `tags`        | list of strings  | no       | `[]`    | Recipe tags for categorisation           |
| `context`     | map              | no       | `{}`    | Default variable values for templates    |
| `extends`     | string           | no       | —       | Parent recipe name (for inheritance). Note: only single-level inheritance is supported; extended recipes cannot themselves use `extends`. |
| `recursion`   | RecursionConfig  | no       | see below | Sub-recipe recursion limits           |
| `hooks`       | RecipeHooks      | no       | —       | Lifecycle hooks — bash commands, run by the resolved [bash interpreter](cli-reference.md#amplihack_bash) |
| `steps`       | list of Step     | **yes**  | —       | Ordered list of steps to execute         |

### RecursionConfig

Controls sub-recipe nesting limits.

| Field             | Type | Default | Description                                    |
|-------------------|------|---------|------------------------------------------------|
| `max_depth`       | int  | `6`     | Maximum sub-recipe recursion depth             |
| `max_total_steps` | int  | `200`   | Maximum total steps across all sub-recipes     |

### RecipeHooks

Shell commands executed at lifecycle boundaries.

| Field       | Type   | Description                            |
|-------------|--------|----------------------------------------|
| `pre_step`  | string | Shell command to run before each step  |
| `post_step` | string | Shell command to run after each step   |
| `on_error`  | string | Shell command to run on step failure   |

Hook commands receive context variables via template substitution. Each runs as
a bash step with a fixed 30-second timeout, under the same
[resolved bash interpreter](cli-reference.md#amplihack_bash) as ordinary
`command:` steps. A failing hook is logged as a warning and does not fail the
step.

---

## Step Fields

| Field               | Type            | Required | Default | Description                                              |
|---------------------|-----------------|----------|---------|----------------------------------------------------------|
| `id`                | string          | **yes**  | —       | Unique step identifier                                   |
| `type`              | string          | no       | inferred | `"bash"`, `"agent"`, or `"recipe"` (see inference rules) |
| `command`           | string          | no       | —       | Shell command (bash steps), run by the resolved [bash interpreter](cli-reference.md#amplihack_bash) |
| `agent`             | string          | no       | —       | Agent reference (agent steps)                            |
| `prompt`            | string          | no       | —       | Prompt template (agent steps)                            |
| `output`            | string          | no       | —       | Variable name to store step output in context            |
| `condition`         | string          | no       | —       | Expression that must be truthy to execute                |
| `parse_json`          | bool            | no       | `false` | Extract JSON from step output                            |
| `parse_json_required` | bool            | no       | `false` | Fail the step if JSON extraction fails (see below)       |
| `mode`              | string          | no       | —       | Execution mode                                           |
| `working_dir`       | string          | no       | —       | Override working directory for this step                 |
| `timeout`           | int             | no       | —       | Step timeout in seconds                                  |
| `auto_stage`        | bool            | no       | `true`  | Git auto-stage after agent steps; `false` opts the step out, and `--no-auto-stage` turns staging off for every step |
| `model`             | string          | no       | —       | Model override for agent steps (e.g. `haiku`, `sonnet`), rendered as a template |
| `context_mode`      | string          | no       | —       | Agent steps only; the one accepted value is `minimal`, which asks the launcher to start the step lightweight (see below) |
| `effort`            | string          | no       | —       | Reasoning-effort hint, rendered as a template and passed to the launcher verbatim (see below) |
| `recipe`            | string          | no       | —       | Sub-recipe name (recipe steps)                           |
| `recovery_on_failure` | bool            | no       | `false` | Attempt agentic recovery if sub-recipe fails (see below) |
| `context`           | map             | no       | —       | Context overrides passed to sub-recipe                   |
| `continue_on_error` | bool            | no       | `false` | Continue execution if this step fails                    |
| `fatal`             | bool            | no       | `true`  | `fatal: false` is shorthand for `continue_on_error: true` |
| `nonfatal`          | bool            | no       | `false` | Alias for `continue_on_error`                            |
| `when_tags`         | list of strings | no       | `[]`    | Step only runs when these tags match active tag filters  |
| `parallel_group`    | string          | no       | —       | Group name for parallel execution                        |

> **Note:** The `context` field provides step-specific variables that are passed as overrides to sub-recipes. In YAML you write `context:`.

### Unknown and retired step fields

A step key that isn't in the table above is **ignored, with a warning**. The
recipe still parses and runs, so an existing recipe never breaks because of an
unknown key, but a typo or an unsupported field can't silently do nothing.

The runner prints one warning per unknown key when it parses the recipe:

```text
recipe 'deploy': step 'build' has unknown field 'comand' (ignored)
```

A field that used to mean something but is now retired gets a specific
message that says what to do. The only retired field is `max_env_value_bytes`:

```text
recipe 'workflow-tdd': step 'checkpoint-3': field 'max_env_value_bytes' is retired and ignored — the runner now spills oversized env values to private files automatically; remove it from the recipe
```

`max_env_value_bytes` was a per-step setting to avoid `Argument list too long`
errors. The runner now handles oversized values automatically without
truncating them (see
[Per-value env spill](env-budget.md#per-value-env-spill-bash-steps)), so you
can remove the field.

Details:

- A step without an `id` is shown as `<index N>` (zero-based). A recipe
  without a `name` is shown as `<unnamed>`. A non-string key is shown as
  `<non-string key>`.
- Recipe names, step ids and keys are escaped (`\n`, `\u{1b}`, ...) and cut
  to 128 characters, so a warning is always a single line.
- A recipe gives at most 100 field warnings, followed by
  `… and N more unknown step fields`.
- Warnings go through the logger at `warn` level when it's enabled (for
  example `RUST_LOG=warn`). Otherwise they go to stderr prefixed with
  `warning: `.
- Each distinct warning is printed once per process, so a sub-recipe that
  runs several times in one recipe run is reported once.
- `--validate-only` lists the same messages in its own report and doesn't
  print them twice. Unknown fields are warnings, so it still exits `0`. See
  [`--validate-only`](cli-reference.md#--validate-only).

Library users who parse recipes themselves can turn the parse-time warnings
off:

```rust
use std::path::Path;
use recipe_runner_rs::parser::RecipeParser;

let parser = RecipeParser::new().without_field_warnings();
let recipe = parser.parse_file(Path::new("recipes/deploy.yaml"))?;
```

`RecipeParser::new()` (and `RecipeParser::default()`) print the warnings.
`RecipeParser` is a struct with a private field, so build it with `new()` or
`default()`; the unit-struct expression `RecipeParser` doesn't compile.

---

## Type Inference Rules

When `type` is omitted, the effective step type is inferred in this order:

1. **`recipe` field present** → `recipe` type
2. **`agent` field present** → `agent` type
3. **`prompt` present without `command`** → `agent` type
4. **Otherwise** → `bash` type (default)

An explicit `type` value always takes precedence.

```yaml
# Inferred as bash (has command, no agent/recipe/prompt)
- id: build
  command: cargo build --release

# Inferred as agent (agent field present)
- id: review
  agent: code-reviewer
  prompt: "Review {{file}}"

# Inferred as agent (prompt without command)
- id: summarise
  prompt: "Summarise the changes in {{diff}}"

# Inferred as recipe (recipe field present)
- id: deploy
  recipe: deploy-production
  context:
    env: staging

# Explicit type overrides inference
- id: special
  type: bash
  prompt: "This prompt is ignored because type is bash"
  command: echo "explicit wins"
```

---

## Template Syntax

Variables are substituted using `{{variable_name}}` syntax. Variable names may
contain letters, digits, underscores, hyphens, and dots.

```yaml
context:
  project: my-app
  branch: main

steps:
  - id: greet
    command: echo "Building {{project}} on {{branch}}"
```

### Dot Notation

Nested context values are accessed with dot notation:

```yaml
context:
  deploy:
    target: production
    region: us-east-1

steps:
  - id: deploy
    command: ./deploy.sh --target {{deploy.target}} --region {{deploy.region}}
```

### Shell Escaping

When templates are rendered for shell commands, values are shell-escaped
automatically via `shell_escape` to prevent injection. Undefined variables
resolve to an empty string.

---

## Condition Syntax

The `condition` field accepts an expression that is evaluated against the
current context. Steps with a falsy condition are skipped.

See [conditions.md](conditions.md) for the full reference. Supported operators
and built-in functions include:

- **Comparisons:** `==`, `!=`, `<`, `<=`, `>`, `>=`
- **Logical:** `and`, `or`, `not`
- **Membership:** `in`, `not in`
- **Functions:** `int()`, `str()`, `len()`, `bool()`, `float()`, `min()`, `max()`
- **Methods:** `strip()`, `lower()`, `upper()`, `startswith()`, `endswith()`, `replace()`, `split()`, `join()`, `count()`, `find()`

```yaml
- id: deploy
  condition: "branch == 'main' and tests_passed == 'true'"
  command: ./deploy.sh
```

---

## JSON Extraction (`parse_json`)

When `parse_json: true`, the runner attempts to extract structured JSON from step
output using three strategies in order:

1. **Direct parse** — the entire trimmed output is valid JSON.
2. **Markdown fence extraction** — JSON inside `` ```json ... ``` `` fences.
3. **Balanced bracket detection** — locates the first `{`…`}` or `[`…`]` block with proper depth tracking, string awareness, and escape handling.

If all strategies fail a warning is logged and the raw output is stored.

```yaml
- id: get-config
  command: curl -s https://api.example.com/config
  output: api_config
  parse_json: true

- id: use-config
  command: echo "Region is {{api_config.region}}"
```

### Strict Mode (`parse_json_required`)

By default, JSON extraction failure degrades the step (status becomes `degraded`)
but the recipe continues. Set `parse_json_required: true` to make extraction
failure a hard error that stops the recipe.

```yaml
- id: must-be-json
  command: curl -s https://api.example.com/data
  parse_json: true
  parse_json_required: true  # fails the recipe if output isn't valid JSON
  output: api_data
```

| `parse_json_required` | On extraction failure        |
|-----------------------|------------------------------|
| `false` (default)     | Step marked `degraded`, raw output stored, recipe continues |
| `true`                | Step marked `failed`, recipe stops immediately |

---

## Sub-Recipe Recovery (`recovery_on_failure`)

When a sub-recipe step fails, set `recovery_on_failure: true` to trigger an
agentic recovery attempt. The runner sends the failure details to an agent,
which attempts to complete the remaining work.

```yaml
- id: deploy
  recipe: deploy-to-staging
  recovery_on_failure: true  # agent attempts recovery if deploy fails
```

If the agent's recovery output contains "STATUS: COMPLETE" or "recovered",
the step is marked as recovered and the recipe continues. Otherwise, the
original failure propagates.

---

## Model Override (`model`)

Agent steps can override the default model using the `model` field. The value
is passed to the selected provider adapter. For Codex, it is forwarded explicitly
as `--model`; omission leaves selection to native Codex configuration. Codex
never uses an implicit `auto` fallback. Choose a provider-supported identifier;
the Claude examples below are not portable Codex model names. See
[Codex agent steps](codex-exec.md).

```yaml
- id: quick-check
  agent: reviewer
  prompt: "Quick lint check on {{file_path}}"
  model: haiku  # fast, cheap model for simple tasks

- id: deep-review
  agent: reviewer
  prompt: "Thorough security review of {{file_path}}"
  model: sonnet  # more capable model for complex analysis
```

`model` is rendered with the step's context before it is passed on, so a recipe
can choose a model from earlier output, from a `context:` block or from `--set`:

```yaml
context:
  review_model: sonnet

steps:
  - id: review
    agent: reviewer
    prompt: "Review {{file_path}}"
    model: "{{review_model}}"
```

The rendered value is trimmed, and a value that renders to nothing is treated as
absent: no `--model` argument is passed and the launcher's own default stands.
A template referring to a variable that does not exist renders to nothing, so a
misspelled variable name falls back to the default rather than failing. Watch
the render warning in the log if a model override appears not to take effect.

The runner does not check that the model exists. It checks only that the string
is safe to place on a command line: characters from `A-Z a-z 0-9 . _ : / + -`,
no leading `-`, at most 128 bytes. A value failing that check fails the step
rather than quietly falling back, and the message names the step and the field
without echoing the value. Because `model` becomes an argv token, and argv is
readable by other local processes, do not render a secret into it.

Where amplihack's launcher accepts tier words such as `fast`, `standard` and
`strong`, they travel in this same `model` field. The runner passes them through
byte for byte; the launcher resolves them. There is one field for choosing a
step's model, whether the value names a model or names a tier.

---

## Lightweight Agent Steps (`context_mode`)

An agent step that only needs its own rendered prompt can ask to start without
the rest of the session's context:

```yaml
- id: classify
  agent: analyzer
  prompt: "Classify this diff as feature, fix or chore. Answer with one word.\n\n{{diff}}"
  context_mode: minimal
  model: fast
  output: category
```

`context_mode` accepts exactly one value, `minimal`. Omitting the field is how a
step asks for the normal start, and that is what every step does today. There is
no value that spells "full".

`minimal` sets `AMPLIHACK_STEP_CONTEXT=minimal` on the agent child, and the
launcher decides what a minimal start means for its harness. The runner itself
trims no prompt, drops no agent system prompt and changes no argument on account
of the field, so a launcher that ignores the variable runs the step exactly as
before. The field is a cost hint: it asks for less context, never for more
access.

An unrecognised value, and the field on a step that is not an agent step, are
both parse errors rather than warnings: a typo should not silently cost the
saving, and a misplaced field should not look as if it worked.

```text
Error: Failed to parse recipe: Invalid recipe: Step 'classify': context_mode 'mininal' is not recognised; the only accepted value is 'minimal'
```

```text
Error: Failed to parse recipe: Invalid recipe: Step 'build': context_mode is only valid on an agent step
```

Both exit `2`, from a normal run and from
[`--validate-only`](cli-reference.md#--validate-only) alike, and no step runs.
Each message names the step that owns the mistake, and the step id and the
rejected value are escaped for display exactly as every other parse diagnostic
escapes them. A misspelled *key* — `context_mdoe` — is still an unknown field
and still a warning, as
[unknown step fields](#unknown-and-retired-step-fields) have always been.

`context_mode` has no bearing on a bash step or a recipe step, which is why
declaring it on one is rejected outright. It is also the only field of the three
restricted this way: `effort` and `model` are accepted wherever either makes
sense.

Changing `context_mode` on a step changes the step definition that recovery
fingerprints, so a step whose `context_mode` was added, altered or removed
re-executes on the next resume rather than reusing its stored record.

---

## Per-Step Effort (`effort`)

`effort` is a free-form hint passed to the launcher alongside the model:

```yaml
- id: classify
  agent: analyzer
  prompt: "Classify this diff. Answer with one word.\n\n{{diff}}"
  context_mode: minimal
  effort: low

- id: design
  agent: architect
  prompt: "Design the migration described in {{spec_path}}"
  effort: "{{design_effort}}"
```

Like `model`, it is rendered with the step's context, trimmed, and treated as
absent when it renders to nothing. It reaches the agent as
`AMPLIHACK_STEP_EFFORT`.

The runner holds no vocabulary for the field, because accepted values differ per
harness. It checks only that the value is at most 128 bytes and free of NUL and
control characters, then passes it through. A value failing that check fails the
step, as an unusable `model` does, rather than being dropped. A value the
launcher does not recognise is the launcher's concern, not a recipe error.

`effort` is accepted on any step, exactly as `model` is, and ignored where no
agent is launched. A recipe step with `recovery_on_failure: true` does launch an
agent when its sub-recipe fails, and that launch carries the step's `effort`,
just as it already carries the step's `model`.

There is **no step-type check on `effort`** and no warning when a bash step or a
recipe step declares one: it is silently unused, the way `model` on a bash step
has always been. `effort` and `model` are the same kind of field — a hint for
whatever agent a step launches — and they behave the same way on a step that
launches none. The restriction belongs to `context_mode` alone, which names a
start that only an agent launch can have.

As with `context_mode`, `effort` is part of the step definition that recovery
fingerprints, so editing it invalidates that step's stored progress and the step
re-executes on the next resume.

For the launch contract in full — the step identifier the runner also passes,
which launches carry what, and what step records show — see the
[Step context contract](step-context-contract.md).

---

## Complete Examples

### 1. Simple Bash-Only Recipe

```yaml
name: build-and-test
version: "1.0"
description: Build the project and run tests
author: dev-team
tags: [ci, build]

context:
  build_mode: release

steps:
  - id: clean
    command: cargo clean

  - id: build
    command: cargo build --{{build_mode}}

  - id: test
    command: cargo test --{{build_mode}}
    output: test_results

  - id: report
    command: echo "Tests complete. Results {{test_results}}"
```

### 2. Agent-Based Workflow

```yaml
name: code-review-workflow
version: "1.0"
description: Automated code review with AI agents

context:
  target_branch: main

steps:
  - id: get-diff
    command: git diff {{target_branch}} --stat
    output: diff_summary

  - id: review
    agent: code-reviewer
    prompt: |
      Review the following changes against {{target_branch}}:
      {{diff_summary}}
      Focus on correctness, security, and performance.
    output: review_result
    parse_json: true

  - id: check-approved
    condition: "review_result.approved == true"
    command: echo "Review passed"

  - id: request-changes
    condition: "review_result.approved != true"
    command: echo "Changes requested — see review_result.comments"
```

### 3. Sub-Recipe Composition

```yaml
name: full-pipeline
version: "2.0"
description: End-to-end pipeline composing smaller recipes

context:
  environment: staging

steps:
  - id: lint
    recipe: lint-check

  - id: build
    recipe: build-project
    context:
      build_mode: release
      target: "{{environment}}"

  - id: deploy
    recipe: deploy-service
    context:
      env: "{{environment}}"
      version: "{{build.version}}"
    condition: "environment != 'local'"
```

### 4. Recipe with Hooks, Tags, and Recursion Limits

```yaml
name: guarded-pipeline
version: "1.0"
description: Pipeline with lifecycle hooks and safety limits
author: platform-team
tags: [production, safe]

recursion:
  max_depth: 3
  max_total_steps: 50

hooks:
  pre_step: echo "[$(date -Iseconds)] Starting step"
  post_step: echo "[$(date -Iseconds)] Finished step"
  on_error: |
    echo "FAILED — sending alert"
    curl -s -X POST https://alerts.example.com/hook \
      -d '{"step": "failed", "recipe": "guarded-pipeline"}'

context:
  notify: true

steps:
  - id: preflight
    command: ./scripts/preflight-check.sh

  - id: migrate
    command: ./scripts/migrate.sh
    when_tags: [database]

  - id: deploy
    command: ./scripts/deploy.sh
    when_tags: [deploy]

  - id: smoke-test
    command: ./scripts/smoke-test.sh
    timeout: 120
    when_tags: [deploy]

  - id: notify
    condition: "notify == 'true'"
    command: echo "Pipeline complete"
```

### 5. Recipe with `continue_on_error` and Conditions

```yaml
name: resilient-checks
version: "1.0"
description: Run multiple checks, collecting results even on failures

context:
  strict: false

steps:
  - id: lint
    command: cargo clippy -- -D warnings
    output: lint_result
    continue_on_error: true

  - id: test
    command: cargo test 2>&1
    output: test_result
    continue_on_error: true

  - id: audit
    command: cargo audit
    output: audit_result
    continue_on_error: true

  - id: gate
    condition: "strict == 'true'"
    command: |
      echo "Lint: {{lint_result}}"
      echo "Test: {{test_result}}"
      echo "Audit: {{audit_result}}"
      # Fail the pipeline in strict mode if any check failed
      exit 1

  - id: summary
    condition: "strict != 'true'"
    command: |
      echo "=== Check Summary ==="
      echo "Lint:  {{lint_result}}"
      echo "Test:  {{test_result}}"
      echo "Audit: {{audit_result}}"
      echo "Non-strict mode — pipeline continues"
```
