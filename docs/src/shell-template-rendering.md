# Shell Template Rendering

How `{{variable}}` substitution works inside bash step commands.

## Contents

- [Overview](#overview)
- [The Three Rendering Modes](#the-three-rendering-modes)
- [Outside Heredocs](#outside-heredocs)
- [Inside Unquoted Heredocs](#inside-unquoted-heredocs)
- [Inside Single-Quoted Heredocs](#inside-single-quoted-heredocs)
- [How Variables Are Exported](#how-variables-are-exported)
- [Security Model](#security-model)
- [Known Limitations](#known-limitations)

---

## Overview

When the recipe runner executes a bash step, it does not insert variable values
directly into the shell command string. Instead it uses two complementary
strategies depending on context:

1. **Env-var references** — outside heredocs and inside unquoted heredoc bodies,
   `{{var}}` is replaced with `$RECIPE_VAR_var`. The value is passed via the
   process environment. Bash expands it at runtime.

2. **Inline substitution** — inside single-quoted heredoc bodies (`<<'EOF'`),
   bash suppresses variable expansion, so the runner inlines the actual value
   directly into the command string before handing it to the shell.

The env-var approach is the default because values never appear in shell source
code, making them immune to injection attacks (semicolons, backticks, `$(...)`,
and other shell metacharacters in values cannot execute as commands).

---

## The Three Rendering Modes

| Location in command              | `{{var}}` becomes             | Why                                          |
|----------------------------------|-------------------------------|----------------------------------------------|
| Outside any heredoc              | `$RECIPE_VAR_var`             | Env var ref; recipe author controls quoting  |
| Inside unquoted heredoc (`<<EOF`)| `$RECIPE_VAR_var`             | Bash expands `$VAR` in heredoc bodies        |
| Inside single-quoted heredoc (`<<'EOF'`) | actual value (inlined) | Bash will not expand `$VAR` in quoted heredocs |

---

## Outside Heredocs

`{{var}}` is replaced with an unquoted `$RECIPE_VAR_var` reference. The recipe
author controls how the reference is quoted in YAML.

```yaml
context:
  repo_path: /home/dev/my-project
  branch: feature/auth

steps:
  - id: checkout
    # Double-quote in YAML → cd "$RECIPE_VAR_repo_path" — safe for paths with spaces
    command: cd "{{repo_path}}" && git checkout {{branch}}
```

Rendered shell command:

```bash
cd "$RECIPE_VAR_repo_path" && git checkout $RECIPE_VAR_branch
```

Both references work correctly. The double-quoted `"$RECIPE_VAR_repo_path"`
handles paths with spaces. The unquoted `$RECIPE_VAR_branch` is fine for a
branch name that has no spaces.

> **Rule:** Quote in YAML when the value might contain spaces or glob characters.
> The renderer preserves whatever quoting you write; it does not add its own.

---

## Inside Unquoted Heredocs

Inside an unquoted heredoc body (`<<EOF` or `<<-EOF`), `{{var}}` becomes an
unquoted `$RECIPE_VAR_var`. Bash expands environment variables in heredoc bodies
normally.

```yaml
context:
  commit_message: "Add OAuth2 login support"
  author: "Alice Dev"

steps:
  - id: write-commit-template
    command: |
      git commit -F - <<EOF
      {{commit_message}}

      Author: {{author}}
      EOF
```

Rendered:

```bash
git commit -F - <<EOF
$RECIPE_VAR_commit_message

Author: $RECIPE_VAR_author
EOF
```

Bash expands `$RECIPE_VAR_commit_message` and `$RECIPE_VAR_author` from the
environment when it reads the heredoc body at runtime.

---

## Inside Single-Quoted Heredocs

A single-quoted heredoc (`<<'EOF'`) tells bash to suppress all variable
expansion in the body — `$VAR` appears literally in the output. Because env-var
references would not be expanded, the runner **inlines the actual value** from
the context directly into the command string.

```yaml
context:
  task_description: "Fix the login bug on the /auth/callback endpoint"

steps:
  - id: create-prompt-file
    command: |
      cat <<'EOF' > prompt.txt
      {{task_description}}
      EOF
```

Rendered:

```bash
cat <<'EOF' > prompt.txt
Fix the login bug on the /auth/callback endpoint
EOF
```

The literal value replaces `{{task_description}}`. No `$RECIPE_VAR_` reference
appears anywhere, so the single-quote quoting of the heredoc is irrelevant to
variable expansion.

This behaviour is important for passing multi-line prompts or structured text to
AI agents through a file, without the shell interpreting any special characters.

---

## How Variables Are Exported

Before running each bash step the runner exports all context variables as
`RECIPE_VAR_*` environment variables. Variable names are transformed as follows:

| Template name   | Env var name           |
|-----------------|------------------------|
| `repo_path`     | `RECIPE_VAR_repo_path` |
| `deploy.region` | `RECIPE_VAR_deploy__region` |
| `my-flag`       | `RECIPE_VAR_my_flag`   |

Rules:
- Prefix `RECIPE_VAR_` is prepended.
- Dots (`.`) become double underscores (`__`).
- Hyphens (`-`) become underscores (`_`).

Nested objects are flattened with `__` separators:

```yaml
context:
  deploy:
    region: us-east-1
    env: production

steps:
  - id: show-region
    command: echo $RECIPE_VAR_deploy__region   # prints: us-east-1
```

---

## Security Model

The env-var approach provides **shell injection immunity** for the common case:

- Values pass through the process environment, not through shell source text.
- Characters like `;`, `&&`, `$(...)`, and backticks in a value do not execute
  as shell commands — they are just data the env var holds.

```yaml
context:
  # Attacker-controlled input — injection attempt
  user_input: "hello; rm -rf /"

steps:
  - id: greet
    command: echo {{user_input}}
```

Rendered: `echo $RECIPE_VAR_user_input`

Bash expands `$RECIPE_VAR_user_input` to the string `hello; rm -rf /` as a
single argument to `echo`, printing it literally. The semicolon is data, not a
command separator.

**Inline substitution (quoted heredocs) is the exception.** When the runner
inlines values into `<<'EOF'` bodies, special characters in the value appear
literally in the heredoc content — which is the intended behaviour. Shell
commands within the heredoc body are not parsed, so injection is not possible
through that path either.

> **Operator trust boundary:** The runner is designed for trusted operators
> running trusted recipes. The `RECIPE_VAR_*` environment variables are set by
> the runner process and are not user-controlled inputs at the shell level.

---

## Known Limitations

### Word splitting on unquoted references

Outside heredocs, `$RECIPE_VAR_x` is unquoted in the shell command unless the
recipe author wrote quotes around `{{x}}` in YAML. If the value contains spaces
and the reference is unquoted, bash word-splits it:

```yaml
# YAML (no quotes around {{file}})
command: wc -l {{file}}

# Value: "my file.txt"
# Rendered: wc -l $RECIPE_VAR_file
# Shell sees: wc -l my file.txt   ← two arguments, not one
```

**Fix:** Quote in YAML when the value may contain spaces:

```yaml
command: wc -l "{{file}}"
# Rendered: wc -l "$RECIPE_VAR_file"
# Shell sees: wc -l "my file.txt"  ← one argument ✓
```

### Multi-line values in single-quoted heredocs

When a context variable holds a multi-line string and is used inside a
`<<'EOF'` body, the newlines are inlined verbatim. This is usually what you
want, but be aware that a value containing a line that is exactly the heredoc
delimiter would prematurely close the heredoc.

```yaml
context:
  # Dangerous: value contains a line that is exactly "EOF"
  content: "line one\nEOF\nline three"

steps:
  - id: write-file
    command: |
      cat <<'EOF'
      {{content}}
      EOF
```

The runner does not detect or escape this. Keep heredoc delimiters distinctive
(e.g. `RECIPE_HEREDOC_END`) when values might contain common words.

### Heredoc detection is line-based

The heredoc parser operates line-by-line. A heredoc start marker and its
delimiter must each occupy their own lines in the rendered command. Commands
that dynamically construct heredocs at runtime are not handled.
