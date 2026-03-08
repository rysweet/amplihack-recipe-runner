# amplihack Recipe Runner

A code-enforced workflow execution engine that reads declarative YAML recipe files and executes them step-by-step using AI agents. Unlike prompt-based workflow instructions that models can interpret loosely or skip, the Recipe Runner controls the execution loop in compiled Rust code — making it physically impossible to skip steps.

## Feature Highlights

- 216 tests across unit, integration, recipe, example, and property-based testing
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
