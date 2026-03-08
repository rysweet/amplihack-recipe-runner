# Quick Start

Get up and running with the amplihack Recipe Runner in minutes.

## Install

```bash
# Clone the repository
git clone https://github.com/rysweet/amplihack-recipe-runner-rs.git
cd amplihack-recipe-runner-rs

# Build in release mode
cargo build --release

# The binary is at target/release/recipe-runner-rs
# Optionally copy it to your PATH:
cp target/release/recipe-runner-rs ~/.local/bin/
```

## Your First Recipe

Create a file called `hello.yaml`:

```yaml
name: "hello-world"
description: "A minimal recipe to verify your setup"
version: "1.0.0"
context:
  greeting: "Hello from the Recipe Runner!"
steps:
  - id: "greet"
    command: "echo '{{greeting}}'"
```

## Run It

```bash
recipe-runner-rs hello.yaml
```

You should see the greeting printed to stdout.

## Override Context

Pass `--set` to override context variables at runtime:

```bash
recipe-runner-rs hello.yaml --set greeting="Howdy, partner!"
```

## Dry Run

Use `--dry-run` to see what would execute without actually running anything:

```bash
recipe-runner-rs hello.yaml --dry-run
```

## Using Agent Steps

Agent steps invoke an AI agent instead of a shell command:

```yaml
name: "analyze-project"
description: "Analyze a codebase with an AI agent"
version: "1.0.0"
context:
  repo_path: "."
steps:
  - id: "analyze"
    agent: "amplihack:core:architect"
    prompt: "Analyze the project at {{repo_path}} and summarize its structure"
    output: "analysis"
    parse_json: true

  - id: "report"
    command: "echo 'Analysis complete'"
    condition: "analysis"
```

## Next Steps

- [YAML Recipe Format](yaml-format.md) — Full schema reference
- [CLI Reference](cli-reference.md) — All flags and subcommands
- [Condition Language](conditions.md) — Conditional step execution
- [Architecture](architecture.md) — How it works under the hood
