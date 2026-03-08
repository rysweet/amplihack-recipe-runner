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

Patterns can be composed using sub-recipes. For example, a CI pipeline
could invoke the quality-audit pattern as a sub-recipe step:

```yaml
steps:
  - id: "quality-gate"
    recipe: "quality-audit"
    context:
      repo_path: "{{repo_path}}"
      language: "rust"
```
