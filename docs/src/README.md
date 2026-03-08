# amplihack Recipe Runner (Rust)

A code-enforced workflow execution engine that reads declarative YAML recipe files and executes them step-by-step using AI agents. Unlike prompt-based workflow instructions that models can interpret loosely or skip, the Recipe Runner controls the execution loop in compiled Rust code — making it physically impossible to skip steps.

Ported from the [amplihack](https://github.com/rysweet/amplihack) Python recipe runner via the Oxidizer workflow.

## Why Rust?

| Metric | Python | Rust |
|---|---|---|
| Startup time | ~800ms | ~5ms |
| Binary size | N/A (requires Python) | ~4MB standalone |
| Dependencies at runtime | Python 3.11+, pip | None |
| Type safety | Runtime errors | Compile-time guarantees |
| Memory safety | GC pauses | Zero-cost abstractions |

## Feature Highlights

- **100% parity** with the Python recipe runner, **plus 12 Rust-only features**
- 183 tests across unit, integration, recipe, and property-based testing
- Parallel step execution, tag filtering, JSONL audit logs
- Recipe composition via `extends`, pre/post/on_error hooks
- Safe condition language with recursive descent parser

## Quick Start

```bash
# Build
cargo build --release

# Run a recipe
recipe-runner-rs path/to/recipe.yaml

# With context overrides
recipe-runner-rs recipe.yaml --set task_description="Add auth" --set repo_path="."

# Dry run
recipe-runner-rs recipe.yaml --dry-run
```

See the [Quick Start guide](quickstart.md) for a more detailed walkthrough.
