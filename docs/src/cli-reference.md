# CLI Reference

Complete reference for the `recipe-runner-rs` command-line interface.

## Synopsis

```
recipe-runner-rs [OPTIONS] [RECIPE] [COMMAND]
recipe-runner-rs list [OPTIONS]
```

## Subcommands

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
recipe-runner-rs deploy.yaml --working-dir /home/user/my-project

# Short form
recipe-runner-rs deploy.yaml -C /home/user/my-project

# Combine with other options
recipe-runner-rs build.yaml -C ../other-repo --dry-run
```

### `-R, --recipe-dir <DIR>`

Add a directory to the recipe search path. Can be specified multiple times to search across several directories.

```bash
# Single directory
recipe-runner-rs my-recipe --recipe-dir ./recipes

# Multiple directories (searched in order)
recipe-runner-rs my-recipe \
  --recipe-dir ./project-recipes \
  --recipe-dir ~/.config/recipes \
  --recipe-dir /opt/shared-recipes

# Short form
recipe-runner-rs my-recipe -R ./recipes -R ../shared

# Combine with list to discover recipes across directories
recipe-runner-rs list -R ./recipes -R /opt/shared-recipes
```

### `--set <KEY=VALUE>`

Override a context variable. Can be specified multiple times to set several variables. Values are automatically typed using smart parsing (see [Smart Context Value Parsing](#smart-context-value-parsing---set)).

```bash
# String value
recipe-runner-rs deploy.yaml --set environment=production

# Integer value (auto-detected)
recipe-runner-rs scale.yaml --set replicas=5

# Float value (auto-detected)
recipe-runner-rs tune.yaml --set ratio=0.75

# Boolean value (auto-detected)
recipe-runner-rs build.yaml --set verbose=true

# JSON value (auto-detected)
recipe-runner-rs config.yaml --set data='{"host": "localhost", "port": 8080}'

# Multiple overrides
recipe-runner-rs deploy.yaml \
  --set environment=production \
  --set replicas=3 \
  --set debug=false \
  --set version=2.1.0
```

### `--dry-run`

Parse and validate the recipe without executing any steps. Useful for checking recipe correctness before committing to a run.

```bash
recipe-runner-rs deploy.yaml --dry-run

# Combine with --set to validate context overrides
recipe-runner-rs deploy.yaml --dry-run --set environment=staging

# Combine with --progress to see what steps would run
recipe-runner-rs deploy.yaml --dry-run --progress
```

### `--no-auto-stage`

Disable automatic git staging of file changes made during recipe execution.

```bash
recipe-runner-rs codegen.yaml --no-auto-stage

# Useful when you want to review changes before staging
recipe-runner-rs refactor.yaml --no-auto-stage -C /path/to/repo
```

### `--validate-only`

Parse and validate the recipe, print any warnings, then exit. Does not execute any steps. More thorough than `--dry-run` as it focuses on surfacing validation warnings.

```bash
recipe-runner-rs deploy.yaml --validate-only

# Validate a recipe in a specific directory
recipe-runner-rs my-recipe --validate-only -R ./recipes

# Validate with context overrides to check for missing variables
recipe-runner-rs deploy.yaml --validate-only --set environment=production
```

### `--explain`

Show the structure of a recipe without executing it. Displays the recipe name, version, and each step with its conditions, agents, and commands.

```bash
recipe-runner-rs deploy.yaml --explain

# Explain a recipe found via search path
recipe-runner-rs my-recipe --explain -R ./recipes
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
recipe-runner-rs deploy.yaml --progress

# Capture progress separately from output
recipe-runner-rs deploy.yaml --progress 2>progress.log

# Combine with JSON output for machine-readable progress + results
recipe-runner-rs deploy.yaml --progress --output-format json
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
recipe-runner-rs build.yaml --include-tags frontend

# Run steps tagged "test" or "lint"
recipe-runner-rs ci.yaml --include-tags test,lint

# Combine with --explain to preview filtered steps
recipe-runner-rs ci.yaml --include-tags test --explain
```

### `--exclude-tags <TAGS>`

Comma-separated list of tags. Steps whose `when_tags` match any of the specified tags will be skipped.

```bash
# Skip slow integration tests
recipe-runner-rs ci.yaml --exclude-tags slow

# Skip multiple categories
recipe-runner-rs full-pipeline.yaml --exclude-tags slow,experimental,deprecated

# Include some, exclude others
recipe-runner-rs ci.yaml --include-tags test --exclude-tags slow
```

### `--audit-dir <DIR>`

Directory where JSONL audit log files are written. Each recipe run produces one audit log file.

```bash
# Write audit logs to a directory
recipe-runner-rs deploy.yaml --audit-dir ./audit-logs

# Combine with other options for a fully audited production run
recipe-runner-rs deploy.yaml \
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
recipe-runner-rs deploy.yaml

# JSON output for scripting / CI pipelines
recipe-runner-rs deploy.yaml --output-format json

# Pipe JSON output to jq
recipe-runner-rs deploy.yaml --output-format json | jq '.steps[] | select(.status == "failed")'

# JSON output with progress on stderr
recipe-runner-rs deploy.yaml --output-format json --progress 2>/dev/null
```

## Exit Codes

| Code | Meaning | Description |
|------|---------|-------------|
| `0` | Success | Recipe completed successfully; all steps passed |
| `1` | Failure | Recipe failed; at least one step failed during execution |
| `2` | Parse/validation error | Invalid YAML syntax, unknown fields, or other validation errors |

```bash
# Check exit code in scripts
recipe-runner-rs deploy.yaml
if [ $? -eq 0 ]; then
  echo "Deploy succeeded"
elif [ $? -eq 1 ]; then
  echo "Deploy failed — check step output"
elif [ $? -eq 2 ]; then
  echo "Recipe is invalid — check YAML syntax"
fi

# Use && / || for simple chaining
recipe-runner-rs build.yaml && recipe-runner-rs deploy.yaml

# Validate before running
recipe-runner-rs deploy.yaml --validate-only && recipe-runner-rs deploy.yaml
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
recipe-runner-rs setup.yaml --set config='{"host": "localhost", "port": 8080}'
recipe-runner-rs setup.yaml --set tags='["web", "api"]'

# Boolean — parsed as bool
recipe-runner-rs build.yaml --set release=true
recipe-runner-rs build.yaml --set skip_tests=false

# Integer — parsed as i64
recipe-runner-rs scale.yaml --set workers=8
recipe-runner-rs scale.yaml --set retries=0

# Float — parsed as f64
recipe-runner-rs tune.yaml --set threshold=0.95
recipe-runner-rs tune.yaml --set learning_rate=0.001

# String — fallback for everything else
recipe-runner-rs deploy.yaml --set branch=main
recipe-runner-rs deploy.yaml --set message="deploy to production"
```

## Environment Variables

### `RECIPE_RUNNER_RECIPE_DIRS`

Additional recipe search directories, separated by colons. These directories are searched in addition to any specified via `--recipe-dir`.

```bash
# Set via environment
export RECIPE_RUNNER_RECIPE_DIRS="/opt/recipes:/home/user/.config/recipes"
recipe-runner-rs my-recipe

# Inline for a single invocation
RECIPE_RUNNER_RECIPE_DIRS=./recipes recipe-runner-rs list

# Combine with --recipe-dir (both are searched)
export RECIPE_RUNNER_RECIPE_DIRS="/opt/shared-recipes"
recipe-runner-rs my-recipe --recipe-dir ./local-recipes
```

### `AMPLIHACK_BASH`

Overrides the interpreter used to run **bash steps**. Must be an **absolute
path** to an executable file.

Bash steps do not hardcode `/bin/bash`. The runner resolves one interpreter in
the parent process and uses that same absolute path for every bash step —
inline (`-c`) or file-backed, with or without a `timeout` wrapper.

#### What it covers

| Runs under the resolved interpreter | Does not |
|---|---|
| Every step with a `command:` field (`type: bash`) | Agent steps — they spawn the agent binary directly (`AMPLIHACK_AGENT_BINARY` selects that one) |
| Every [lifecycle hook](yaml-format.md#recipehooks) — `hooks.pre_step`, `hooks.post_step`, `hooks.on_error` — which the runner executes as bash steps with a fixed 30-second timeout | Sub-recipe (`type: recipe`) steps, which run in-process |

#### Resolution order

| # | Rule | Condition | Result |
|---|---|---|---|
| 1 | `AMPLIHACK_BASH` | Set and non-empty | That path, after validation. **Invalid values abort the step — they never fall through to rule 2.** |
| 2 | `PATH` | Any absolute `PATH` entry contains an executable `bash` | The **first** match, in `PATH` order. |
| 3 | Last resort | Nothing above matched | `/bin/bash` |

An empty `AMPLIHACK_BASH` (`AMPLIHACK_BASH=`) is treated as unset and falls
through to rule 2. There is no trimming: a whitespace-only value is a path, and
it fails loudly like any other bad one.

Non-absolute and empty `PATH` entries are skipped during the rule-2 scan. An
empty entry means "current directory" to POSIX, and the current directory of a
bash step is the recipe's `working_dir` — recipe-controlled data has no business
selecting an interpreter.

Rule 2 honours `PATH` order exactly. There is no version detection: if an older
`bash` comes first on your `PATH`, it wins, because `PATH` order is your stated
preference. Use `AMPLIHACK_BASH` when you want a specific build regardless of
`PATH`.

Resolution reads the **parent** process environment only. A step's `env:` block
or a `RECIPE_VAR_*` value named `AMPLIHACK_BASH` is merged into the *child*
environment and cannot change which interpreter the runner picks.

#### Invalid values fail loudly

A set-but-unusable `AMPLIHACK_BASH` is a hard error. The runner does **not**
silently fall back to `PATH` or `/bin/bash`, because a step that quietly runs
under a different interpreter than the operator asked for is worse than a step
that refuses to run.

One message covers all three rejections:

```text
AMPLIHACK_BASH is set to "<path>" but <reason>. Unset AMPLIHACK_BASH to resolve
bash from PATH, or point it at an absolute path to an executable bash.
```

`<reason>` is one of three fixed strings:

| Reason | Triggered by | Example value |
|---|---|---|
| not an absolute path | anything without a leading `/` | `bash`, `./bash`, `~/bin/bash` |
| not a regular file | missing paths, dangling symlinks, directories | `/nonexistent/bash`, `/opt/bash/` |
| not executable | present and a regular file, but no execute bit set | `/opt/bin/bash` at mode `0644` |

Two properties of that message are worth knowing before you paste it into a bug
report:

- It echoes **your `AMPLIHACK_BASH` value and nothing else**. It never contains
  `PATH`, the directories searched, or the candidates tried — `PATH` routinely
  leaks project, customer, and username identifiers through directory names.
- Symlinks are followed when validating, so `/bin/bash` pointing at
  `/usr/bin/bash` is accepted. The path you supplied is the path that gets
  executed; it is never rewritten to its symlink target.

The absolute-path requirement is not style. In a `timeout`-wrapped step the
interpreter is passed as an *argument* to `timeout`, so a value beginning with
`-` would be swallowed by `timeout`'s option parser instead of being treated as
the command. Requiring a leading `/` makes that unrepresentable.

Conversely, shell metacharacters in the value are inert and are **not** rejected.
There is no shell between the resolved path and `exec`, so `;`, spaces, `$(…)`
and newlines are ordinary filename bytes. `AMPLIHACK_BASH=/tmp/x;evil` names a
file literally called `x;evil`.

#### Resolution is logged

Each bash step logs the interpreter it resolved and which rule chose it, at
`info`. Resolution is per step and uncached, so a recipe with N bash steps emits
N lines.

The binary initialises `env_logger` with no default filter, so **nothing below
`error` is shown until you set `RUST_LOG`**:

```console
$ RUST_LOG=info recipe-runner-rs build
[2026-09-22T17:04:11Z INFO  recipe_runner_rs::adapters::cli_subprocess] bash interpreter: /opt/homebrew/bin/bash (source: PATH)
```

The message body — the part after `env_logger`'s timestamp/level/target prefix —
takes one of three shapes:

```text
bash interpreter: /usr/local/bin/bash (source: AMPLIHACK_BASH)
bash interpreter: /opt/homebrew/bin/bash (source: PATH)
bash interpreter: /bin/bash (source: default)
```

Like the error text, the log line carries the resolved path and the rule tag
only. It never prints `PATH` or the candidates that were tried.

#### It propagates to child processes

`AMPLIHACK_BASH` matches the protected `AMPLIHACK_` prefix, so it is never
dropped by [env-budget trimming](env-budget.md) and is inherited by every
subprocess the runner spawns — bash steps and agent steps alike. A bash step or
an agent that goes on to invoke `recipe-runner-rs` itself therefore sees the
same pin. Exporting it once covers a whole tree of runs, not just one process.

#### Examples

```bash
# Pin the exact interpreter, ignoring PATH entirely.
AMPLIHACK_BASH=/opt/homebrew/bin/bash recipe-runner-rs build

# Restore the pre-resolution behaviour (always /bin/bash) for one run.
AMPLIHACK_BASH=/bin/bash recipe-runner-rs build

# Pin it for a whole session, including nested sub-recipes.
export AMPLIHACK_BASH=/usr/local/bin/bash
recipe-runner-rs deploy.yaml --set environment=production

# Confirm which bash a recipe will use, without running it.
command -v bash          # what rule 2 would pick
bash --version | head -1
```

On macOS, `/bin/bash` is bash 3.2 and lacks `mapfile`, `declare -A`, and `${x@Q}`.
A recipe that uses bash 4+ syntax works as long as a newer `bash` is on `PATH`
ahead of `/bin/bash` (Homebrew installs one), or `AMPLIHACK_BASH` names it.

> **Behaviour note.** Earlier versions always ran `/bin/bash`. Bash steps now
> prefer the first `bash` on `PATH`. Set `AMPLIHACK_BASH=/bin/bash` to get the
> old behaviour back exactly.

See [Bash interpreter resolution](architecture.md#bash-interpreter-resolution)
for why the parent process resolves the path instead of letting `execvp` search
for it.

### Rate-Limit Retry Knobs

Agent steps automatically retry on transient provider rate limits with bounded
exponential backoff. Tune the behavior with these variables (see
[Rate-Limit Handling](rate-limit-handling.md) for full details):

| Variable | Default | Description |
|---|---|---|
| `AMPLIHACK_RATELIMIT_MAX_RETRIES` | `5` | Max retries after the initial attempt. |
| `AMPLIHACK_RATELIMIT_BASE_DELAY_SECS` | `60` | Base backoff window; `0` = instant. |
| `AMPLIHACK_RATELIMIT_MAX_DELAY_SECS` | `600` | Cap on any single backoff delay. |
| `AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL` | _unset_ | Force `--model auto` on the final retry. |
| `AMPLIHACK_LAUNCHER_BINARY` | `amplihack` | Test-only launcher override. |

## Usage Examples

### Basic Usage

```bash
# Run a recipe by file path
recipe-runner-rs ./recipes/build.yaml

# Run a recipe by name (searched in recipe directories)
recipe-runner-rs build

# List all discoverable recipes
recipe-runner-rs list
```

### CI/CD Pipeline

```bash
# Validate, then run with JSON output and auditing
recipe-runner-rs deploy.yaml --validate-only \
  && recipe-runner-rs deploy.yaml \
    --set environment=production \
    --set version="$(git describe --tags)" \
    --output-format json \
    --audit-dir /var/log/deploys \
    --progress
```

### Development Workflow

```bash
# Preview what a recipe will do
recipe-runner-rs refactor.yaml --explain

# Dry-run with overrides to test logic
recipe-runner-rs refactor.yaml --dry-run \
  --set target_module=auth \
  --set aggressive=true

# Run without auto-staging to review changes manually
recipe-runner-rs refactor.yaml \
  --set target_module=auth \
  --no-auto-stage
```

### Selective Step Execution

```bash
# Run only unit tests
recipe-runner-rs ci.yaml --include-tags unit

# Run everything except slow tests
recipe-runner-rs ci.yaml --exclude-tags slow,integration

# Explain which steps match the filter
recipe-runner-rs ci.yaml --include-tags unit --explain
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
output=$(recipe-runner-rs analyze.yaml --output-format json)
echo "$output" | jq '.summary'

# Run with full observability
recipe-runner-rs deploy.yaml \
  --output-format json \
  --progress \
  --audit-dir ./audit \
  --set environment=production \
  2>progress.log \
  1>result.json
```
