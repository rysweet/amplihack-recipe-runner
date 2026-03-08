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
| `extends`     | string           | no       | —       | Parent recipe name (for inheritance)     |
| `recursion`   | RecursionConfig  | no       | see below | Sub-recipe recursion limits           |
| `hooks`       | RecipeHooks      | no       | —       | Lifecycle hooks                          |
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

Hook commands receive context variables via template substitution.

---

## Step Fields

| Field               | Type            | Required | Default | Description                                              |
|---------------------|-----------------|----------|---------|----------------------------------------------------------|
| `id`                | string          | **yes**  | —       | Unique step identifier                                   |
| `type`              | string          | no       | inferred | `"bash"`, `"agent"`, or `"recipe"` (see inference rules) |
| `command`           | string          | no       | —       | Shell command (bash steps)                               |
| `agent`             | string          | no       | —       | Agent reference (agent steps)                            |
| `prompt`            | string          | no       | —       | Prompt template (agent steps)                            |
| `output`            | string          | no       | —       | Variable name to store step output in context            |
| `condition`         | string          | no       | —       | Expression that must be truthy to execute                |
| `parse_json`        | bool            | no       | `false` | Extract JSON from step output                            |
| `mode`              | string          | no       | —       | Execution mode                                           |
| `working_dir`       | string          | no       | —       | Override working directory for this step                 |
| `timeout`           | int             | no       | —       | Step timeout in seconds                                  |
| `auto_stage`        | bool            | no       | `true`  | Git auto-stage after agent steps                         |
| `recipe`            | string          | no       | —       | Sub-recipe name (recipe steps)                           |
| `context`           | map             | no       | —       | Context overrides passed to sub-recipe                   |
| `continue_on_error` | bool            | no       | `false` | Continue execution if this step fails                    |
| `when_tags`         | list of strings | no       | `[]`    | Step only runs when these tags match active tag filters  |
| `parallel_group`    | string          | no       | —       | Group name for parallel execution (future)               |

> **Note:** The `context` field on a step is serialised with `#[serde(rename = "context")]` from the internal `sub_context` field. In YAML you write `context:`.

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
