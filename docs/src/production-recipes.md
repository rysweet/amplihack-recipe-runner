# Production Recipes

These recipes ship with [amplihack](https://github.com/rysweet/amplihack) and demonstrate
real-world workflow patterns at scale.

Source: [amplifier-bundle/recipes/](https://github.com/rysweet/amplihack/tree/main/amplifier-bundle/recipes)

## Development Workflows

| Recipe | Description |
|--------|-------------|
| [default-workflow](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/default-workflow.yaml) | Complete development lifecycle: requirements → design → implement → test → merge |
| [verification-workflow](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/verification-workflow.yaml) | Lightweight workflow for trivial changes: config edits, doc updates, single-file fixes |
| [qa-workflow](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/qa-workflow.yaml) | Minimal workflow for simple questions and informational requests |
| [investigation-workflow](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/investigation-workflow.yaml) | Systematic investigation with parallel agent deployment |
| [guide](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/guide.yaml) | Interactive guide to amplihack features |

## Quality & Reliability

| Recipe | Description |
|--------|-------------|
| [quality-audit-cycle](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/quality-audit-cycle.yaml) | Iterative audit loop: lint → analyze → fix → re-lint → verify improvement |
| [self-improvement-loop](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/self-improvement-loop.yaml) | Closed-loop eval improvement: eval → analyze → research → improve → re-eval → compare |
| [domain-agent-eval](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/domain-agent-eval.yaml) | Evaluate domain agents: eval harness + teaching evaluation + combined report |
| [long-horizon-memory-eval](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/long-horizon-memory-eval.yaml) | 1000-turn memory stress test with self-improvement loop |
| [sdk-comparison](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/sdk-comparison.yaml) | Run L1-L12 eval on all 4 SDKs and generate comparative report |

## Multi-Agent Decision Making

| Recipe | Description |
|--------|-------------|
| [consensus-workflow](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/consensus-workflow.yaml) | Multi-agent consensus at critical decision points with structured checkpoints |
| [debate-workflow](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/debate-workflow.yaml) | Multi-agent structured debate for complex decisions requiring diverse perspectives |
| [n-version-workflow](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/n-version-workflow.yaml) | N-version programming: generate multiple independent implementations, pick best |
| [cascade-workflow](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/cascade-workflow.yaml) | 3-level fallback cascade: primary → secondary → tertiary |

## Orchestration

| Recipe | Description |
|--------|-------------|
| [smart-orchestrator](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/smart-orchestrator.yaml) | Task classifier + goal-seeking loop with up to 3 execution rounds |
| [auto-workflow](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/auto-workflow.yaml) | Autonomous multi-turn workflow — continues until task complete or max iterations |

## Migration

| Recipe | Description |
|--------|-------------|
| [oxidizer-workflow](https://github.com/rysweet/amplihack/blob/main/amplifier-bundle/recipes/oxidizer-workflow.yaml) | Automated Python-to-Rust migration with quality audit cycles and degradation checks |
