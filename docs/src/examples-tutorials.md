# Tutorial Examples

Progressive tutorials that teach one recipe runner feature at a time.
Each tutorial is a self-contained YAML recipe you can run directly.

## Tutorials

| # | Recipe | Feature | Run it |
|---|--------|---------|--------|
| 01 | [hello-world](../../examples/tutorials/01-hello-world.yaml) | Simplest recipe — one bash step | `recipe-runner-rs examples/tutorials/01-hello-world.yaml` |
| 02 | [variables](../../examples/tutorials/02-variables.yaml) | Template `{{variables}}` and context | `recipe-runner-rs examples/tutorials/02-variables.yaml` |
| 03 | [conditions](../../examples/tutorials/03-conditions.yaml) | Conditional step execution | `recipe-runner-rs examples/tutorials/03-conditions.yaml` |
| 04 | [multi-step-pipeline](../../examples/tutorials/04-multi-step-pipeline.yaml) | Sequential steps with output chaining | `recipe-runner-rs examples/tutorials/04-multi-step-pipeline.yaml` |
| 05 | [working-directories](../../examples/tutorials/05-working-directories.yaml) | Per-step `working_dir` | `recipe-runner-rs examples/tutorials/05-working-directories.yaml` |
| 06 | [parse-json](../../examples/tutorials/06-parse-json.yaml) | JSON extraction from output | `recipe-runner-rs examples/tutorials/06-parse-json.yaml` |
| 07 | [error-handling](../../examples/tutorials/07-error-handling.yaml) | `continue_on_error` | `recipe-runner-rs examples/tutorials/07-error-handling.yaml` |
| 08 | [hooks](../../examples/tutorials/08-hooks.yaml) | Pre/post/on_error hooks | `recipe-runner-rs examples/tutorials/08-hooks.yaml` |
| 09 | [tags](../../examples/tutorials/09-tags.yaml) | `when_tags` + `--include-tags` | `recipe-runner-rs examples/tutorials/09-tags.yaml --include-tags fast` |
| 10 | [parallel-groups](../../examples/tutorials/10-parallel-groups.yaml) | `parallel_group` concurrent execution | `recipe-runner-rs examples/tutorials/10-parallel-groups.yaml` |
| 11 | [extends](../../examples/tutorials/11-extends.yaml) | Recipe inheritance via `extends` | `recipe-runner-rs examples/tutorials/11-extends.yaml` |
| 12 | [recursion-limits](../../examples/tutorials/12-recursion-limits.yaml) | `recursion` config | `recipe-runner-rs examples/tutorials/12-recursion-limits.yaml` |
| 13 | [timeouts](../../examples/tutorials/13-timeouts.yaml) | Step-level `timeout` | `recipe-runner-rs examples/tutorials/13-timeouts.yaml` |
| 14 | [dry-run](../../examples/tutorials/14-dry-run.yaml) | `--dry-run` mode | `recipe-runner-rs examples/tutorials/14-dry-run.yaml --dry-run` |

## Recommended Order

Start with **01-hello-world** and work through sequentially.
Each tutorial builds on concepts from previous ones.
