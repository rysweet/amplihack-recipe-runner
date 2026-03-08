# amplihack Recipe Runner

[![CI](https://github.com/rysweet/amplihack-recipe-runner/actions/workflows/ci.yml/badge.svg)](https://github.com/rysweet/amplihack-recipe-runner/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-rysweet.github.io-blue)](https://rysweet.github.io/amplihack-recipe-runner/)

A code-enforced workflow execution engine that reads declarative YAML recipe files and executes them step-by-step using AI agents. Unlike prompt-based workflow instructions that models can interpret loosely or skip, the Recipe Runner controls the execution loop in compiled Rust code — making it physically impossible to skip steps.

## Quick Start

```bash
# Build
cargo build --release

# Run a recipe
recipe-runner-rs path/to/recipe.yaml

# With context overrides
recipe-runner-rs recipe.yaml --set task_description="Add auth" --set repo_path="."

# Dry run (see what would execute without running anything)
recipe-runner-rs recipe.yaml --dry-run

# Specify search directories for sub-recipes
recipe-runner-rs recipe.yaml -R ./recipes -R ../amplihack/amplifier-bundle/recipes
```

## Documentation

📖 **Full docs: [rysweet.github.io/amplihack-recipe-runner](https://rysweet.github.io/amplihack-recipe-runner/)**

- **[Architecture](docs/src/architecture.md)** — Module design, data flow, adapter pattern
- **[YAML Format Reference](docs/src/yaml-format.md)** — Complete recipe schema
- **[CLI Reference](docs/src/cli-reference.md)** — All commands, flags, exit codes
- **[Condition Language](docs/src/conditions.md)** — Safe expression evaluator reference

## Architecture

```
src/
├── main.rs              # CLI interface (clap)
├── lib.rs               # Public API: parse_recipe, run_recipe, run_recipe_by_name
├── models.rs            # Step, Recipe, StepResult, RecipeResult
├── parser.rs            # YAML → Recipe parser with validation + typo detection
├── context.rs           # Template rendering + safe condition evaluation
├── runner.rs            # Recipe execution engine with JSON retry + sub-recipes
├── agent_resolver.rs    # Agent reference resolution with path traversal protection
├── discovery.rs         # Multi-dir recipe discovery, SHA-256 manifest, upstream sync
└── adapters/
    ├── mod.rs           # Adapter trait
    └── cli_subprocess.rs  # CLI subprocess adapter (temp dir, session tree, heartbeat)
```

## Features

| Module | Purpose |
|---|---|
| `models.rs` | Core data structures (Recipe, Step, StepResult) |
| `parser.rs` | YAML parsing, validation, typo detection |
| `context.rs` | Template engine + safe condition evaluator |
| `runner.rs` | Execution engine, hooks, parallel groups |
| `agent_resolver.rs` | Agent ref resolution with path traversal protection |
| `discovery.rs` | Multi-dir discovery with SHA-256 manifest |
| `cli_subprocess.rs` | CLI subprocess adapter with timeout enforcement |
| `lib.rs` | Public API |

Run `cargo test` for current test counts.

| Feature | Description |
|---|---|
| Recipe-level recursion limits | `max_depth` + `max_total_steps` |
| Timeout enforcement | SIGTERM/SIGKILL on agent step deadline |
| `continue_on_error` | Per-step failure tolerance |
| CLI subcommands | `list`, `--validate-only`, `--explain`, `--progress` |
| Tag filtering | `when_tags` + `--include-tags`/`--exclude-tags` |
| JSONL audit log | Structured per-run execution audit |
| Pre/post/on_error hooks | Shell commands at step lifecycle events |
| Discovery caching | TTL-based cache for recipe discovery |
| Parallel step execution | `parallel_group` with `std::thread::scope` |
| Recipe composition | `extends` for single-level recipe inheritance |
| Property-based testing | proptest fuzz for conditions, templates, parser |

## Examples & Recipes

**34 example recipes** organized by purpose:

| Directory | Count | Purpose |
|---|---|---|
| [`examples/tutorials/`](examples/tutorials/) | 15 | Progressive feature tutorials (hello-world → extends) |
| [`examples/patterns/`](examples/patterns/) | 8 | Real-world workflow patterns (CI, review, deploy, consensus) |
| [`recipes/testing/`](recipes/testing/) | 10 | Edge-case and stress-test recipes |
| [`recipes/`](recipes/) | 1 | Self-building recipe |

Every recipe has corresponding tests. Start with [`01-hello-world.yaml`](examples/tutorials/01-hello-world.yaml).

## Tests

```bash
cargo test
```

Comprehensive test suite covering unit, integration, recipe, feature, and example tests.

## Creating a Recipe

```yaml
name: "my-workflow"
description: "Example workflow"
version: "1.0.0"
context:
  project_name: "my-project"
  repo_path: "."
steps:
  - id: "analyze"
    agent: "amplihack:core:architect"
    prompt: "Analyze {{project_name}} at {{repo_path}}"
    output: "analysis"
    parse_json: true

  - id: "implement"
    agent: "amplihack:core:builder"
    prompt: "Based on this analysis: {{analysis}}, implement the changes"
    condition: "analysis"

  - id: "verify"
    command: "cargo test"
    working_dir: "{{repo_path}}"
```

## License

MIT
