# Workflow Pattern Examples

Real-world workflow patterns that show how to compose recipe runner features
for common development scenarios.

Source: [examples/patterns/](https://github.com/rysweet/amplihack-recipe-runner/tree/main/examples/patterns)

## Patterns

| Pattern | Recipe | Description |
|---------|--------|-------------|
| **CI Pipeline** | [ci-pipeline.yaml](https://github.com/rysweet/amplihack-recipe-runner/blob/main/examples/patterns/ci-pipeline.yaml) | Gated build pipeline: checkout → deps → lint → test → build → package. Each step gates on prior success. |
| **Code Review** | [code-review.yaml](https://github.com/rysweet/amplihack-recipe-runner/blob/main/examples/patterns/code-review.yaml) | Automated review: git diff → agent analysis → issue detection → review comments. |
| **Deploy Pipeline** | [deploy-pipeline.yaml](https://github.com/rysweet/amplihack-recipe-runner/blob/main/examples/patterns/deploy-pipeline.yaml) | Full deployment: pre-flight → build → integration test → staging → smoke test → promote. |
| **Investigation** | [investigation.yaml](https://github.com/rysweet/amplihack-recipe-runner/blob/main/examples/patterns/investigation.yaml) | Systematic research: scope → explore (find/grep) → analyze → synthesize → document. |
| **Migration** | [migration.yaml](https://github.com/rysweet/amplihack-recipe-runner/blob/main/examples/patterns/migration.yaml) | Fail-fast migration: backup → validate → migrate → smoke test → verify. |
| **Multi-Agent Consensus** | [multi-agent-consensus.yaml](https://github.com/rysweet/amplihack-recipe-runner/blob/main/examples/patterns/multi-agent-consensus.yaml) | Multiple agents analyze independently → synthesize votes → apply decision. |
| **Quality Audit** | [quality-audit.yaml](https://github.com/rysweet/amplihack-recipe-runner/blob/main/examples/patterns/quality-audit.yaml) | Audit loop: lint → analyze → fix → re-lint → verify improvement. |
| **Self-Improvement** | [self-improvement.yaml](https://github.com/rysweet/amplihack-recipe-runner/blob/main/examples/patterns/self-improvement.yaml) | Closed loop: eval → analyze errors → research → apply → re-eval → compare. |

## Combining Patterns

Patterns compose via sub-recipe steps, hooks, tags, and parallel groups. Here's a
full deployment recipe that chains three patterns together — CI first, then review,
then deploy — with quality audit as a gate between stages:

```yaml
name: "ship-release"
description: "CI → Review → Quality Gate → Deploy"
version: "1.0"

context:
  repo_path: "."
  environment: "staging"

hooks:
  on_error: "echo 'Pipeline failed at step: $STEP_ID' >> pipeline.log"

steps:
  # ── Stage 1: Build & Test (sub-recipe) ──
  - id: "ci"
    recipe: "ci-pipeline"
    context:
      repo_path: "{{repo_path}}"
    output: "ci_result"

  # ── Stage 2: Parallel code reviews ──
  - id: "security-review"
    agent: "amplihack:security"
    parallel_group: "reviews"
    prompt: "Review {{repo_path}} for security vulnerabilities."
    output: "security_findings"

  - id: "architecture-review"
    agent: "amplihack:architect"
    parallel_group: "reviews"
    prompt: "Review {{repo_path}} for architectural issues."
    output: "arch_findings"

  # ── Stage 3: Quality gate (sub-recipe, conditional) ──
  - id: "quality-gate"
    recipe: "quality-audit"
    condition: "ci_result and 'PASS' in ci_result"
    context:
      repo_path: "{{repo_path}}"
    output: "audit_result"

  # ── Stage 4: Deploy (tagged — only runs with --include-tags release) ──
  - id: "deploy"
    recipe: "deploy-pipeline"
    when_tags: ["release"]
    condition: "'PASS' in audit_result"
    context:
      repo_path: "{{repo_path}}"
      environment: "{{environment}}"
    output: "deploy_result"

  # ── Notification ──
  - id: "notify"
    command: |
      echo "Release pipeline complete."
      echo "CI: {{ci_result}}"
      echo "Audit: {{audit_result}}"
      echo "Deploy: {{deploy_result}}"
```

This recipe demonstrates:

- **Sub-recipes** (`recipe:`) — CI, quality audit, and deploy each run as self-contained workflows
- **Parallel groups** (`parallel_group:`) — security and architecture reviews run concurrently
- **Conditional gates** (`condition:`) — quality audit only runs if CI passed; deploy only if audit passed
- **Tag filtering** (`when_tags:`) — deploy step only executes when `--include-tags release` is passed
- **Error hooks** (`hooks.on_error:`) — logs which step failed for post-mortem
- **Output chaining** — each stage's result flows into the next stage's conditions
