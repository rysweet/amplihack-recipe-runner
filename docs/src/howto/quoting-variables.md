# How to Quote Variables in Bash Commands

Control word splitting and glob expansion when using `{{variables}}` in bash
step commands.

## The Rule in One Line

> Quote in YAML when the value might contain spaces or glob characters.

---

## Background

When a bash step runs, `{{var}}` is replaced with `$RECIPE_VAR_var` — an
environment variable reference, not the value itself. The recipe author's YAML
quoting passes through unchanged; the runner does not add its own quotes.

```yaml
command: echo {{message}}
# Rendered: echo $RECIPE_VAR_message
```

```yaml
command: echo "{{message}}"
# Rendered: echo "$RECIPE_VAR_message"
```

This means **you control quoting**, and you only need quotes when the value
might contain characters that bash would misinterpret (spaces, globs, etc.).

---

## When to Quote

### Values with spaces (paths, sentences, descriptions)

```yaml
context:
  repo_path: /home/dev/my project   # space in path

steps:
  - id: enter-repo
    # Without quotes: bash splits into two words
    command: cd {{repo_path}}        # BAD  → cd /home/dev/my project → error

  - id: enter-repo-fixed
    # With quotes: space is safe
    command: cd "{{repo_path}}"      # GOOD → cd "$RECIPE_VAR_repo_path"
```

### Paths passed to flags

```yaml
context:
  output_dir: /tmp/build output

steps:
  - id: build
    command: cargo build --target-dir "{{output_dir}}"
    # Rendered: cargo build --target-dir "$RECIPE_VAR_output_dir"
```

### Values with glob characters (`*`, `?`, `[`)

```yaml
context:
  pattern: "*.log"

steps:
  - id: count-logs
    # Without quotes: bash would expand *.log before passing to wc
    command: wc -l {{pattern}}        # risky

  - id: count-logs-safe
    command: wc -l "{{pattern}}"      # safe — pattern treated as literal
```

---

## When Quotes Are Not Needed

### Single-word values with no special characters

```yaml
context:
  branch: main
  mode: release

steps:
  - id: build
    command: cargo build --{{mode}} --branch {{branch}}
    # Fine: neither value has spaces or metacharacters
```

### Variables used inside heredoc bodies

Inside unquoted heredoc bodies (`<<EOF`), bash expands `$VAR` automatically.
Quotes inside heredoc bodies become literal characters in the output, so do not
wrap heredoc-body variables in quotes.

```yaml
steps:
  - id: write-file
    command: |
      cat <<EOF > output.txt
      Branch: {{branch}}
      Mode:   {{mode}}
      EOF
    # Rendered body:
    #   Branch: $RECIPE_VAR_branch
    #   Mode:   $RECIPE_VAR_mode
    # Bash expands both — correct output
```

---

## Single-Quoted Heredocs (`<<'EOF'`)

In a single-quoted heredoc, bash does **not** expand `$VAR`. The runner
handles this by inlining the actual value directly. Quoting inside the body has
no effect on expansion — the value is already literal text.

```yaml
context:
  task_description: "Refactor the auth module to use JWT tokens"

steps:
  - id: write-prompt
    command: |
      cat <<'EOF' > task.txt
      {{task_description}}
      EOF
    # Rendered:
    #   cat <<'EOF' > task.txt
    #   Refactor the auth module to use JWT tokens
    #   EOF
    # The value is inlined before bash ever sees the command.
```

Use `<<'EOF'` when the value contains shell metacharacters you want preserved
literally (dollar signs, backticks, backslashes) that would otherwise be
interpreted inside an unquoted heredoc.

---

## Quick Reference

| Scenario                          | YAML to write              | What bash sees                          |
|-----------------------------------|----------------------------|-----------------------------------------|
| Simple word (no spaces)           | `{{branch}}`               | `$RECIPE_VAR_branch`                   |
| Path or string with spaces        | `"{{repo_path}}"`          | `"$RECIPE_VAR_repo_path"`              |
| Unquoted heredoc body             | `{{value}}`                | `$RECIPE_VAR_value` (bash expands)     |
| Single-quoted heredoc body        | `{{value}}`                | actual value inlined                    |
| Value with glob chars             | `"{{pattern}}"`            | `"$RECIPE_VAR_pattern"`                |

---

## Related

- [Shell Template Rendering](../shell-template-rendering.md) — how the renderer
  works internally
- [YAML Recipe Format](../yaml-format.md) — full template syntax reference
- [Workflow Patterns](../examples-patterns.md) — real-world recipe examples
