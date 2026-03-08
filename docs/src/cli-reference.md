# CLI Reference

Complete reference for the `recipe-runner-rs` command-line interface.

## Synopsis

```
recipe-runner-rs [OPTIONS] [RECIPE] [COMMAND]
recipe-runner-rs run [OPTIONS] <RECIPE>
recipe-runner-rs list [OPTIONS]
```

## Subcommands

### `run`

Execute a recipe. This is the default subcommand when a positional `RECIPE` argument is provided.

```bash
# Explicit subcommand
recipe-runner-rs run my-recipe.yaml

# Implicit (positional RECIPE triggers run)
recipe-runner-rs my-recipe.yaml
```

**Arguments:**

| Argument | Required | Description |
|----------|----------|-------------|
| `RECIPE` | Yes | Path or name of the recipe to execute |

### `list`

Discover and display all available recipes found in the configured search directories.

```bash
recipe-runner-rs list
recipe-runner-rs list --recipe-dir ./custom-recipes
recipe-runner-rs list --recipe-dir ./team-recipes --recipe-dir ./personal-recipes
```

## Global Options

### `-C, --working-dir <DIR>`

Set the working directory for recipe execution.

**Default:** `.` (current directory)

```bash
# Run a recipe from a different directory
recipe-runner-rs run deploy.yaml --working-dir /home/user/my-project

# Short form
recipe-runner-rs run deploy.yaml -C /home/user/my-project

# Combine with other options
recipe-runner-rs run build.yaml -C ../other-repo --dry-run
```

### `-R, --recipe-dir <DIR>`

Add a directory to the recipe search path. Can be specified multiple times to search across several directories.

```bash
# Single directory
recipe-runner-rs run my-recipe --recipe-dir ./recipes

# Multiple directories (searched in order)
recipe-runner-rs run my-recipe \
  --recipe-dir ./project-recipes \
  --recipe-dir ~/.config/recipes \
  --recipe-dir /opt/shared-recipes

# Short form
recipe-runner-rs run my-recipe -R ./recipes -R ../shared

# Combine with list to discover recipes across directories
recipe-runner-rs list -R ./recipes -R /opt/shared-recipes
```

### `--set <KEY=VALUE>`

Override a context variable. Can be specified multiple times to set several variables. Values are automatically typed using smart parsing (see [Smart Context Value Parsing](#smart-context-value-parsing---set)).

```bash
# String value
recipe-runner-rs run deploy.yaml --set environment=production

# Integer value (auto-detected)
recipe-runner-rs run scale.yaml --set replicas=5

# Float value (auto-detected)
recipe-runner-rs run tune.yaml --set ratio=0.75

# Boolean value (auto-detected)
recipe-runner-rs run build.yaml --set verbose=true

# JSON value (auto-detected)
recipe-runner-rs run config.yaml --set data='{"host": "localhost", "port": 8080}'

# Multiple overrides
recipe-runner-rs run deploy.yaml \
  --set environment=production \
  --set replicas=3 \
  --set debug=false \
  --set version=2.1.0
```

### `--dry-run`

Parse and validate the recipe without executing any steps. Useful for checking recipe correctness before committing to a run.

```bash
recipe-runner-rs run deploy.yaml --dry-run

# Combine with --set to validate context overrides
recipe-runner-rs run deploy.yaml --dry-run --set environment=staging

# Combine with --progress to see what steps would run
recipe-runner-rs run deploy.yaml --dry-run --progress
```

### `--no-auto-stage`

Disable automatic git staging of file changes made during recipe execution.

```bash
recipe-runner-rs run codegen.yaml --no-auto-stage

# Useful when you want to review changes before staging
recipe-runner-rs run refactor.yaml --no-auto-stage -C /path/to/repo
```

### `--validate-only`

Parse and validate the recipe, print any warnings, then exit. Does not execute any steps. More thorough than `--dry-run` as it focuses on surfacing validation warnings.

```bash
recipe-runner-rs run deploy.yaml --validate-only

# Validate a recipe in a specific directory
recipe-runner-rs run my-recipe --validate-only -R ./recipes

# Validate with context overrides to check for missing variables
recipe-runner-rs run deploy.yaml --validate-only --set environment=production
```

### `--explain`

Show the structure of a recipe without executing it. Displays the recipe name, version, and each step with its conditions, agents, and commands.

```bash
recipe-runner-rs run deploy.yaml --explain

# Explain a recipe found via search path
recipe-runner-rs run my-recipe --explain -R ./recipes
```

Example output:

```
Recipe: deploy
Version: 1.2.0

Steps:
  1. build
     Agent: builder
     Command: cargo build --release
  2. test
     Condition: when context.run_tests == true
     Agent: tester
     Command: cargo test
  3. deploy
     Agent: deployer
     Command: ./scripts/deploy.sh
```

### `--progress`

Print step progress events to stderr. Emits events when each step starts and completes, useful for monitoring long-running recipes.

```bash
recipe-runner-rs run deploy.yaml --progress

# Capture progress separately from output
recipe-runner-rs run deploy.yaml --progress 2>progress.log

# Combine with JSON output for machine-readable progress + results
recipe-runner-rs run deploy.yaml --progress --output-format json
```

Example stderr output:

```
[step:start] build (1/3)
[step:complete] build (1/3) — ok
[step:start] test (2/3)
[step:complete] test (2/3) — ok
[step:start] deploy (3/3)
[step:complete] deploy (3/3) — ok
```

### `--include-tags <TAGS>`

Comma-separated list of tags. Only steps whose `when_tags` match at least one of the specified tags will run. All other steps are skipped.

```bash
# Run only steps tagged "frontend"
recipe-runner-rs run build.yaml --include-tags frontend

# Run steps tagged "test" or "lint"
recipe-runner-rs run ci.yaml --include-tags test,lint

# Combine with --explain to preview filtered steps
recipe-runner-rs run ci.yaml --include-tags test --explain
```

### `--exclude-tags <TAGS>`

Comma-separated list of tags. Steps whose `when_tags` match any of the specified tags will be skipped.

```bash
# Skip slow integration tests
recipe-runner-rs run ci.yaml --exclude-tags slow

# Skip multiple categories
recipe-runner-rs run full-pipeline.yaml --exclude-tags slow,experimental,deprecated

# Include some, exclude others
recipe-runner-rs run ci.yaml --include-tags test --exclude-tags slow
```

### `--audit-dir <DIR>`

Directory where JSONL audit log files are written. Each recipe run produces one audit log file.

```bash
# Write audit logs to a directory
recipe-runner-rs run deploy.yaml --audit-dir ./audit-logs

# Combine with other options for a fully audited production run
recipe-runner-rs run deploy.yaml \
  --audit-dir /var/log/recipe-runner \
  --set environment=production \
  --progress
```

### `--output-format <FORMAT>`

Control the output format. Available formats:

| Format | Description |
|--------|-------------|
| `text` | Human-readable output (default) |
| `json` | Machine-readable JSON output |

```bash
# Default text output
recipe-runner-rs run deploy.yaml

# JSON output for scripting / CI pipelines
recipe-runner-rs run deploy.yaml --output-format json

# Pipe JSON output to jq
recipe-runner-rs run deploy.yaml --output-format json | jq '.steps[] | select(.status == "failed")'

# JSON output with progress on stderr
recipe-runner-rs run deploy.yaml --output-format json --progress 2>/dev/null
```

## Exit Codes

| Code | Meaning | Description |
|------|---------|-------------|
| `0` | Success | Recipe completed successfully; all steps passed |
| `1` | Failure | Recipe failed; at least one step failed during execution |
| `2` | Parse/validation error | Invalid YAML syntax, unknown fields, or other validation errors |

```bash
# Check exit code in scripts
recipe-runner-rs run deploy.yaml
if [ $? -eq 0 ]; then
  echo "Deploy succeeded"
elif [ $? -eq 1 ]; then
  echo "Deploy failed — check step output"
elif [ $? -eq 2 ]; then
  echo "Recipe is invalid — check YAML syntax"
fi

# Use && / || for simple chaining
recipe-runner-rs run build.yaml && recipe-runner-rs run deploy.yaml

# Validate before running
recipe-runner-rs run deploy.yaml --validate-only && recipe-runner-rs run deploy.yaml
```

## Smart Context Value Parsing (`--set`)

When using `--set KEY=VALUE`, the runner automatically determines the value type by attempting each parse strategy in order:

| Priority | Type | Detection | Example |
|----------|------|-----------|---------|
| 1 | JSON | Valid JSON object/array | `--set data='{"key": "val"}'` |
| 2 | Boolean | Literal `true` or `false` | `--set verbose=true` |
| 3 | Integer | Digits only (with optional sign) | `--set count=5` |
| 4 | Float | Numeric with decimal point | `--set ratio=0.5` |
| 5 | String | Everything else (fallback) | `--set name=hello` |

```bash
# JSON — parsed as a structured object
recipe-runner-rs run setup.yaml --set config='{"host": "localhost", "port": 8080}'
recipe-runner-rs run setup.yaml --set tags='["web", "api"]'

# Boolean — parsed as bool
recipe-runner-rs run build.yaml --set release=true
recipe-runner-rs run build.yaml --set skip_tests=false

# Integer — parsed as i64
recipe-runner-rs run scale.yaml --set workers=8
recipe-runner-rs run scale.yaml --set retries=0

# Float — parsed as f64
recipe-runner-rs run tune.yaml --set threshold=0.95
recipe-runner-rs run tune.yaml --set learning_rate=0.001

# String — fallback for everything else
recipe-runner-rs run deploy.yaml --set branch=main
recipe-runner-rs run deploy.yaml --set message="deploy to production"
```

## Environment Variables

### `RECIPE_RUNNER_RECIPE_DIRS`

Additional recipe search directories, separated by colons. These directories are searched in addition to any specified via `--recipe-dir`.

```bash
# Set via environment
export RECIPE_RUNNER_RECIPE_DIRS="/opt/recipes:/home/user/.config/recipes"
recipe-runner-rs run my-recipe

# Inline for a single invocation
RECIPE_RUNNER_RECIPE_DIRS=./recipes recipe-runner-rs list

# Combine with --recipe-dir (both are searched)
export RECIPE_RUNNER_RECIPE_DIRS="/opt/shared-recipes"
recipe-runner-rs run my-recipe --recipe-dir ./local-recipes
```

## Usage Examples

### Basic Usage

```bash
# Run a recipe by file path
recipe-runner-rs run ./recipes/build.yaml

# Run a recipe by name (searched in recipe directories)
recipe-runner-rs build

# List all discoverable recipes
recipe-runner-rs list
```

### CI/CD Pipeline

```bash
# Validate, then run with JSON output and auditing
recipe-runner-rs run deploy.yaml --validate-only \
  && recipe-runner-rs run deploy.yaml \
    --set environment=production \
    --set version="$(git describe --tags)" \
    --output-format json \
    --audit-dir /var/log/deploys \
    --progress
```

### Development Workflow

```bash
# Preview what a recipe will do
recipe-runner-rs run refactor.yaml --explain

# Dry-run with overrides to test logic
recipe-runner-rs run refactor.yaml --dry-run \
  --set target_module=auth \
  --set aggressive=true

# Run without auto-staging to review changes manually
recipe-runner-rs run refactor.yaml \
  --set target_module=auth \
  --no-auto-stage
```

### Selective Step Execution

```bash
# Run only unit tests
recipe-runner-rs run ci.yaml --include-tags unit

# Run everything except slow tests
recipe-runner-rs run ci.yaml --exclude-tags slow,integration

# Explain which steps match the filter
recipe-runner-rs run ci.yaml --include-tags unit --explain
```

### Multi-Directory Recipe Management

```bash
# Search across project, team, and global recipes
recipe-runner-rs list \
  -R ./recipes \
  -R ~/team-recipes \
  -R /opt/global-recipes

# Or use the environment variable
export RECIPE_RUNNER_RECIPE_DIRS="./recipes:~/team-recipes:/opt/global-recipes"
recipe-runner-rs list
```

### Scripting and Automation

```bash
# Capture JSON output for downstream processing
output=$(recipe-runner-rs run analyze.yaml --output-format json)
echo "$output" | jq '.summary'

# Run with full observability
recipe-runner-rs run deploy.yaml \
  --output-format json \
  --progress \
  --audit-dir ./audit \
  --set environment=production \
  2>progress.log \
  1>result.json
```
