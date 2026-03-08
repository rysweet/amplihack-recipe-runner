# amplihack Recipe Runner (Rust)

A code-enforced workflow execution engine that reads declarative YAML recipe files and executes them step-by-step using AI agents. Unlike prompt-based workflow instructions that models can interpret loosely or skip, the Recipe Runner controls the execution loop in compiled Rust code — making it physically impossible to skip steps.

Ported from the [amplihack](https://github.com/rysweet/amplihack) Python recipe runner via the Oxidizer workflow ([#2818](https://github.com/rysweet/amplihack/issues/2818)).

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

- **[Architecture](docs/architecture.md)** — Module design, data flow, adapter pattern
- **[YAML Format Reference](docs/yaml-format.md)** — Complete recipe schema
- **[CLI Reference](docs/cli-reference.md)** — All commands, flags, exit codes
- **[Condition Language](docs/conditions.md)** — Safe expression evaluator reference

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

**100% parity** with the Python recipe runner. See [SCORECARD.md](SCORECARD.md) for the full 24-feature comparison.

| Python Module | Rust Module | Tests |
|---|---|---|
| `models.py` | `models.rs` | ✅ |
| `parser.py` | `parser.rs` | 8 tests |
| `context.py` | `context.rs` | 21 tests |
| `runner.py` | `runner.rs` | 7 tests |
| `agent_resolver.py` | `agent_resolver.rs` | 6 tests |
| `discovery.py` | `discovery.rs` | 6 tests |
| `adapters/cli_subprocess.py` | `adapters/cli_subprocess.rs` | ✅ |
| `__init__.py` | `lib.rs` (public API) | ✅ |

## Tests

```bash
cargo test
```

**60 tests** across unit + integration:
- Recipe YAML parsing and validation with typo detection
- Template rendering with `{{var}}` substitution
- Shell escape injection prevention
- Condition evaluation (==, !=, <, <=, >, >=, in, not in, and, or, not)
- Safe function calls (int, str, len, bool, float, min, max)
- Safe method calls (strip, lower, upper, startswith, endswith, replace, split, join, count, find)
- Dot-notation nested value access
- JSON extraction from LLM output (3 strategies) with retry
- Sub-recipe execution with context merge and depth guard
- Agent resolver with path traversal protection
- Recipe discovery with SHA-256 manifest
- Full lifecycle integration tests
- Fail-fast behavior verification

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
