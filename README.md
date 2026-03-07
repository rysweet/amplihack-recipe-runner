# amplihack Recipe Runner (Rust)

Rust implementation of the [amplihack](https://github.com/rysweet/amplihack) recipe runner — ported via the Oxidizer workflow (issue [#2818](https://github.com/rysweet/amplihack/issues/2818)).

## Overview

This is a standalone CLI tool that parses and executes YAML-defined amplihack recipes. It is a direct port of the Python recipe runner (`src/amplihack/recipes/`) to idiomatic Rust.

## Architecture

```
src/
├── main.rs              # CLI interface (clap)
├── lib.rs               # Module exports
├── models.rs            # Step, Recipe, StepResult, RecipeResult, etc.
├── parser.rs            # YAML → Recipe parser with validation
├── context.rs           # Template rendering + safe condition evaluation
├── runner.rs            # Recipe execution engine
└── adapters/
    ├── mod.rs           # Adapter trait
    └── cli_subprocess.rs  # CLI subprocess adapter (spawns `claude -p`)
```

## Usage

```bash
# Build
cargo build --release

# Run a recipe
./target/release/recipe-runner-rs path/to/recipe.yaml

# With context overrides
./target/release/recipe-runner-rs recipe.yaml --set task_description="Build auth module"

# Dry run
./target/release/recipe-runner-rs recipe.yaml --dry-run

# Specify recipe search directories for sub-recipes
./target/release/recipe-runner-rs recipe.yaml -R ./recipes -R ../amplihack/amplifier-bundle/recipes
```

## Feature Scorecard

See [SCORECARD.md](SCORECARD.md) for Python ↔ Rust parity tracking.

## Ported From

| Python Module | Rust Module | Status |
|---|---|---|
| `models.py` | `models.rs` | ✅ Complete |
| `parser.py` | `parser.rs` | ✅ Complete |
| `context.py` | `context.rs` | ✅ Complete |
| `runner.py` | `runner.rs` | ✅ Complete |
| `adapters/base.py` | `adapters/mod.rs` | ✅ Complete |
| `adapters/cli_subprocess.py` | `adapters/cli_subprocess.rs` | ✅ Complete |
| `__init__.py` | `lib.rs` + `main.rs` | ✅ Complete |
| `discovery.py` | Partial (find in runner) | 🔄 Basic |
| `agent_resolver.py` | Not yet ported | ⬜ TODO |

## Tests

```bash
cargo test
```

24 tests covering:
- Recipe YAML parsing and validation
- Template rendering with `{{var}}` substitution
- Shell escape injection prevention
- Condition evaluation (==, !=, in, not in, and, or, not)
- Dot-notation nested value access
- JSON extraction from LLM output (3 strategies)
- Recipe execution with mock adapter
- Conditional step skipping
- Dry run mode

## License

MIT
