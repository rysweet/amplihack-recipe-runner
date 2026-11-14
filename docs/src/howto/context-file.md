---
title: Pass context through a file
description: Supply recipe context as JSON without putting its values on the runner command line.
doc_type: howto
last_updated: 2026-11-14
---

# Pass context through a file

Use `--context-file` to keep task text and structured context off the runner's
command line. This example needs a Unix shell, `jq` and a runner advertising
[`context_file_v1`](../codex-exec.md#compatibility-probe-api).

## Check support

```bash
recipe-runner-rs --capabilities | jq -e '.schema_version == 1 and (.capabilities | index("context_file_v1") != null)'
# Output: true
```

## Create private inputs and run

Run this block in one shell. It creates a private directory and owner-only
files, runs a bash-only recipe and removes the example files on exit.

```bash
(
  umask 077
  context_example_dir=$(mktemp -d)
  trap 'rm -f "$context_example_dir/context.json" "$context_example_dir/recipe.yaml" "$context_example_dir/result.json"; rmdir "$context_example_dir"' EXIT

  cat > "$context_example_dir/context.json" <<'JSON'
{"task_description":"Fix the widget","is_critical_code":"true","count":"42"}
JSON

  cat > "$context_example_dir/recipe.yaml" <<'YAML'
name: file-context-example
version: "1.0"
context:
  count: 1
steps:
  - id: show-context
    type: bash
    command: |
      printf '%s\n' "$RECIPE_VAR_task_description"
      printf 'critical=%s count=%s\n' "$RECIPE_VAR_is_critical_code" "$RECIPE_VAR_count"
    output: observed
YAML

  recipe-runner-rs "$context_example_dir/recipe.yaml" --no-update-check \
    --context-file "$context_example_dir/context.json" --set count=7 \
    --output-format json > "$context_example_dir/result.json"
  jq -r '.step_results[0].output' "$context_example_dir/result.json"
)
# Output:
# Fix the widget
# critical=true count=7
```

The file converts `"true"` to a boolean and `"42"` to a number. File context
overrides the recipe's `count: 1`, then `--set count=7` overrides the file.
Putting `--set` before `--context-file` gives the same result. Both context
sources remain available to `{{placeholder}}` substitution, including in
[agent step prompts](../codex-exec.md#run-a-recipe).

## Manage the file lifetime

Keep the file available until startup loading finishes. The runner reads it
once and never modifies or removes it; the caller removes it after the run.
Changing it after loading does not change the running recipe.

Use file transport for values that must stay off the runner's command line.
Values supplied with `--set` still appear there. Intentional step output and
JSON results can contain file-loaded values, as this example demonstrates.
Bash steps execute from owner-only temporary script files, including when
quoted heredocs inline context. These scripts are removed when the step returns.
Agent instructions travel through private prompt files (Codex uses stdin).
Agent working directories are set as the child process cwd; directory grants use
`--add-dir .`, keeping context-rendered directory paths off launcher argv.

Runner diagnostics omit rendered step working directories and sub-recipe lookup
names, including at DEBUG level. Directory and lookup failures remain visible
through operation categories without the rendered values. Sub-recipe parse and
inheritance failures also omit nested error details that could disclose selected
filenames or recipe content.

See the [context-file reference](../cli-reference.md#--context-file-path) for
all JSON types, secure opening behavior and exit-code `4` diagnostics.
