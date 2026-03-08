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

## Why Rust?

| Metric | Python | Rust |
|---|---|---|
| Startup time | ~800ms | ~5ms |
| Binary size | N/A (requires Python) | ~4MB standalone |
| Dependencies at runtime | Python 3.11+, pip | None |
| Type safety | Runtime errors | Compile-time guarantees |
| Memory safety | GC pauses | Zero-cost abstractions |

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

## Feature Parity

**100% parity** with the Python recipe runner, **plus 11 Rust-only features**.

| Python Module | Rust Module | Tests |
|---|---|---|
| `models.py` | `models.rs` | ✅ |
| `parser.py` | `parser.rs` | 11 tests |
| `context.py` | `context.rs` | 21 tests |
| `runner.py` | `runner.rs` | 10 tests |
| `agent_resolver.py` | `agent_resolver.rs` | 6 tests |
| `discovery.py` | `discovery.rs` | 10 tests |
| `adapters/cli_subprocess.py` | `adapters/cli_subprocess.rs` | ✅ |
| `__init__.py` | `lib.rs` (public API) | ✅ |

### Rust-Only Features

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

**216 tests** across 5 test suites:

| Suite | Tests | Covers |
|---|---|---|
| Unit tests | 53 | Parser, context, runner, discovery, agent resolver |
| Integration tests | 14 | Full lifecycle, fail-fast, sub-recipes |
| Recipe tests | 91 | All control flow and IO mechanisms |
| Feature tests | 23 | New features + property-based fuzzing (proptest) |
| Example tests | 35 | All 34 recipes: parse, validate, dry-run, execute |

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
