---
title: Step context contract
description: The environment variables the runner sets on an agent child so a launcher can start a step lightweight.
doc_type: reference
last_updated: 2026-11-16
---

# Step context contract

Every agent step of a recipe run starts a full agent session by default. A step
that only needs its own rendered prompt — a classification, a one-line
extraction, a yes-or-no judgement — pays for context it never reads.

The runner does not implement a lightweight start. It **declares** one. A recipe
marks a step with `context_mode: minimal`, and the runner passes that mark, the
step's identifier and an optional effort hint to the agent child through three
environment variables. The launcher on the other side of those variables decides
what a minimal start means for its harness. A launcher that reads none of them
starts the step exactly as it does today.

## The three variables

| Variable | Value | Set when |
|----------|-------|----------|
| `AMPLIHACK_STEP_CONTEXT` | `minimal` | The step declared `context_mode: minimal`. Absent otherwise. |
| `AMPLIHACK_STEP_EFFORT` | The step's rendered `effort`, verbatim | The rendered value is non-empty. Absent otherwise. |
| `AMPLIHACK_STEP_ID` | The step's `id`, exactly as written in the recipe | Every agent launch the runner makes on a step's behalf. |

They are set on the agent child's environment only. No launcher command-line
flag is added and no argv token changes on account of any of them, so a launcher
that has never heard of the contract is unaffected.

### The YAML key and the variable name differ

The recipe field is `context_mode`. The variable is `AMPLIHACK_STEP_CONTEXT`.
They are deliberately not the same word: `context:` is already a step field,
carrying the input map passed to a sub-recipe, and it keeps that meaning
untouched. The variable name is the launcher-facing half of the contract and is
the name amplihack-rs reads; the YAML key is the recipe-facing half. Neither
name constrains the other.

### Which launches carry the contract

A bash step receives none of the three. A recipe step does not itself launch an
agent, so its own dispatch receives none either.

Three launches do carry the contract, and they are not identical:

| Launch | `AMPLIHACK_STEP_ID` | `AMPLIHACK_STEP_EFFORT` | `AMPLIHACK_STEP_CONTEXT` |
|--------|---------------------|-------------------------|--------------------------|
| Normal agent dispatch | yes | where declared | where `minimal` |
| JSON-repair retry | yes | where declared | where `minimal` |
| Sub-recipe recovery launch | yes | where declared | never |

The **JSON-repair retry** is the second call the runner makes when a step with
`parse_json: true` returns something that is not JSON. It is the same step's
work, so it inherits the same declaration, including the minimal mark.

The **recovery launch** is the agent that runs when a step with
`recovery_on_failure: true` fails. It keeps the failing step's `model` and
`effort`, because it works on that step's behalf, and it receives that step's
identifier. It never carries `AMPLIHACK_STEP_CONTEXT`, and two independent
things make that so.

The launch site passes the minimal mark as a literal "none declared" rather than
reading it from the step, so the omission is stated at the one place it could go
wrong instead of being inferred. The mark is also unrepresentable here: `context_mode`
is accepted only on an agent step and is a parse error anywhere else, while
`recovery_on_failure` is a recipe-step field, so a step that can trigger a
recovery launch cannot have declared a minimal start in the first place.

The explicit argument is what carries the guarantee today; the parse-time
restriction is why a reader does not have to take that argument on trust. The outcome is right on its own
terms — a launch that exists to reason about a failure wants whatever context
its harness would normally give it.

### `AMPLIHACK_STEP_CONTEXT`

`minimal` is the only value the runner writes, and `minimal` is the only value
`context_mode` accepts. **Presence means minimal; absence means the normal
start.** There is no value that spells "full" — not in the recipe schema, and
not as a second variant inside the runner. A recipe asks for the default by
omitting the field, and the runner asks for the default by setting nothing.
Absence is the one way to say it, so there is nothing to keep consistent
between a `full` spelling and a missing field. A consumer that meets a value it
does not recognise — left behind by some other tool — should treat it as
absent, which errs towards more context rather than less.

`AMPLIHACK_STEP_CONTEXT` is a cost hint and nothing else. Nothing in the runner
branches on it. The runner trims no prompt, drops no agent system prompt, and
changes no argument because of it. See
[Rules for launcher authors](#rules-for-launcher-authors) for what a consumer
must not do with it.

### `AMPLIHACK_STEP_EFFORT`

The runner holds no effort vocabulary. The accepted values differ per harness,
and amplihack owns the mapping, so the runner renders the step's `effort` field,
trims it, treats an empty result as absent, and passes the rest through
unchanged.

**Precedence.** amplihack-rs
[#2722](https://github.com/rysweet/amplihack-rs/pull/2722) derives a reasoning
effort from a tier word in the step's `model:` field. Where both a tier-derived
effort and `AMPLIHACK_STEP_EFFORT` are available, the environment variable is an
explicit per-step request and should win; absent the variable, the tier's effort
stands. The runner sets the variable only when the step declares a non-empty
rendered `effort`, so the two sources never both carry a value by accident. The
precedence itself is implemented in amplihack's launcher, not here.

### `AMPLIHACK_STEP_ID`

The value is `step.id` exactly as the recipe writes it, after no rendering and
no rewriting. It is set on every agent launch the runner makes on a step's
behalf, with one exception: an id holding a control character, or one too
large to place in an environment — its `AMPLIHACK_STEP_ID=<id>` string
exceeding the kernel's per-string limit, which would otherwise fail the whole
spawn — is omitted rather than placed there, and the omission is logged at
debug level. An otherwise valid recipe is not stopped over the identifier of a
step that is about to run. Treat the variable as **usually present**, not as an
invariant, and correlate on the launch rather than requiring the name.

That exception is where the whole of the id's handling lives. The contract adds
**no parse-time check** on `id`: no length cap, no character rejection, nothing
beyond the non-empty and uniqueness checks the parser already made before this
feature existed. Carrying an id to the environment is not a reason to narrow
what a recipe may call its steps, and an id that cannot travel costs one
variable on one launch rather than a rejected recipe.

Step ids are **recipe-local**. A sub-recipe's step id is unique within that
sub-recipe, not within the run, so two steps in different recipes of one nested
run can share an id. A consumer must not key a global lookup on this value
alone.

This answers the upstream follow-up that
[#2722](https://github.com/rysweet/amplihack-rs/pull/2722) names, where no step
id reached the child process.

## Inherited values are removed before the child is built

The runner removes `AMPLIHACK_STEP_ID`, `AMPLIHACK_STEP_CONTEXT` and
`AMPLIHACK_STEP_EFFORT` from the inherited environment **unconditionally**, on
every child it spawns — agent and bash alike, and whatever the step declares.
Only then does it insert the values the table above describes, which is why the
removal is universal while the insertion is agent-only: a child that should
receive nothing receives nothing, rather than inheriting it.

This is a security control, not housekeeping. Without it, any process that can
set an environment variable before the runner starts — a parent shell, a CI job
definition, an outer agent step that spawns a nested run — could set
`AMPLIHACK_STEP_CONTEXT=minimal` once and silently degrade every agent step of
every recipe beneath it, including a review or verification step, with nothing
in any recipe to show for it. The removal also keeps the absence of a variable
meaning what this page says it means.

## Interaction with `--env-allow`

The three variables are inserted **after** the
[`--env-allow`](cli-reference.md#--env-allow-name) filter runs, so they are
present in an agent child regardless of what the option names, in the same way
as `AMPLIHACK_AGENT_BINARY` and the session-tree variables.

That is not a hole in the allowlist. `--env-allow` restricts what is
**inherited**; the values inserted here are generated by the runner or taken
from the recipe in front of it, never inherited — the removal above guarantees
that. No inherited secret escapes confinement through them. What follows is that
`--env-allow` does not describe an agent child's environment exhaustively.

The three names also carry the `AMPLIHACK_` prefix, so they are protected from
budget eviction (see
[Protected variables](env-budget.md#protected-variables)) and a step's launch
contract survives budget pressure.

## What the runner validates

The runner checks the shape of what it is about to place on a command line or in
an environment, and nothing beyond that. The checks do not all happen at the
same moment, and they do not all fail the same thing.

| Field | Checked | When it fails |
|-------|---------|---------------|
| `context_mode` | Must be exactly `minimal`, and only on an agent step. | At parse, beside the existing non-empty-id and duplicate-id checks: the recipe is rejected and no step runs. |
| `model` | Rendered, trimmed, empty treated as absent. Characters limited to `A-Z a-z 0-9 . _ : / + -`, no leading `-`, at most 128 bytes. | At resolution, when the step is about to launch: the step fails, naming the step and the field. Earlier steps have already run. |
| `effort` | Rendered, trimmed, empty treated as absent. No NUL, no control characters, at most 128 bytes. | At resolution, as for `model`. |

Nothing beyond shape is checked. The runner does not know whether a model
exists, whether the harness supports it, whether a tier word is current, or
whether an effort value means anything at all. That vocabulary belongs to
amplihack, which already crosses every model string it can emit through one
validator. A second vocabulary here would contradict having one way to choose a
step's model.

`model` and `effort` are templates, so their final values exist only once a step
has its context; a check any earlier would be checking the template rather than
the value. `context_mode` is a literal with a closed set of values, so it is
checked where the parser already checks every other property of a step. A step
whose `model` or `effort` fails its check does **not** fall back to the
launcher's default — a template defect is loud rather than silent.

The error message for a rejected `model` or `effort` names the step and the
field and does **not** echo the rejected value, which may carry a secret or a
terminal escape sequence into stderr, the audit log and a CI log.

### A rejected `context_mode` names the step

The recipe is read with `context_mode` as a **plain string**, and the value is
compared against `minimal` by the same validation pass that checks ids and step
types. That is a deliberate choice over letting the deserialiser reject an
unknown value for a typed field: a deserialiser failure is raised from inside
document decoding, where no step is in hand, and reports a position in the
document rather than the step that owns the mistake. The two messages that
[YAML Recipe Format](yaml-format.md#lightweight-agent-steps-context_mode)
quotes are the contract — each names the offending step, and the first also
names the value it rejected.

Both the step id and the rejected `context_mode` value are escaped for display
in the same way as every other parse diagnostic, so a control character or an
escape sequence in a recipe cannot reach a terminal through the error. Echoing
the value is safe here, and useful, because `context_mode` is a literal written
in the recipe: unlike `model` and `effort` it is never rendered from context and
so can never hold a value a template dragged in from somewhere else.

### Why the shape rules exist

The leading-hyphen and character-set rules on `model` exist only because the
value becomes its own argv token after `--model`. A value rendering to
`--add-dir` or `--allow-all-tools` would be read as a flag by the downstream
launcher's argument parser, which this runner neither controls nor inspects.
Every real model identifier and every tier word passes the rule unchanged.

The 128-byte cap on each of the two rendered values exists because neither sink
is otherwise bounded. Argv is not measured against the environment budget at
all, so an oversized `model` surfaces as an opaque `E2BIG` from the spawn; and
because the budget's `AMPLIHACK_` prefix rule protects `AMPLIHACK_STEP_EFFORT`
from eviction, an oversized effort stops the spawn with a budget error instead
of being dropped with a warning. Since `model` and `effort` are rendered from
the recipe's context — which is fed by step output, agent output, parsed JSON,
`--context-file` and `--set` — the cap is what keeps a context value from
becoming a run-wide failure.

### A rendered model is visible to other local users

`model` becomes an argv token, and argv is readable from `/proc/<pid>/cmdline`
by other processes. Do not render a secret into `model:`. `effort` and the other
two variables travel by environment, which is readable by the owning user only.
That asymmetry is also why no `AMPLIHACK_STEP_MODEL` exists: `model` already has
exactly one route to the launcher, and adding a second would mean two ways to
say the same thing.

## Step records

A step's record gains three optional fields, written for agent launches only and
omitted entirely when they do not apply:

| Field | Written when |
|-------|--------------|
| `context_mode` | The step declared `minimal` (only an agent step may). |
| `model` | A non-empty rendered model resolved, and the step is not a bash step. |
| `effort` | A non-empty rendered effort resolved, and the step is not a bash step. |

The recorded values are the **effective, rendered** ones — the launch the step
resolved, not the template the recipe wrote. They are written for both
successful and failed launches, so a failed lightweight step is still visible
as one. A step that never ran carries none of them, and neither does a bash
step, which resolves no launch at all.

Recording what each step was launched with is the point of the feature: without
it there is no way to measure what a lightweight step saves against the steps
that did not opt in.

### Where a record is observed

The record is one structure, `StepResult`, serialised to three places. The three
new fields appear in all of them, under the same conditions:

| Surface | Shape |
|---------|-------|
| [`--output-format json`](cli-reference.md#--output-format-format) | One object per step in the top-level `step_results` array. |
| [`--audit-dir <DIR>`](cli-reference.md#--audit-dir-dir) | One JSON object per line, one line per completed step. |
| Recovery store records | Embedded as the `result` field of each stored record. |

The default text output prints a step's status, id, duration and error, and does
not show these fields.

The audit log is written with owner-only permissions, but
`--output-format json` goes to standard output and typically into a CI log.
Both now carry the rendered `model` and `effort`, so treat them as disclosed
wherever that output lands.

`model` records the model the **runner resolved and launched with**. Under
rate-limit handling, `fallback_auto_model` can replace the model on the
adapter's final attempt, inside the adapter. The record does not show that
override. Reading the records tells you what each step was launched with, not
what every attempt used.

Because the fields are omitted when absent, the record of a bash step, or of a
step of any type that declares none of the three, is byte-identical to the
record the same step produced before this feature existed. An agent step that
already declares `model:` gains one key.

A bash step records none of the three even when it declares them. It is the one
step type that can never launch an agent, so there is no launch to describe;
`model:` and `effort:` on a bash step remain the inert, unread fields they were
before this feature existed, and are not validated either.

A **recipe step** does record the values it resolved, whether or not anything
was launched with them. A sub-recipe's steps resolve their own launches; the
parent recipe step's resolved values are the ones its *recovery* launch would
use, and recovery fires only when the sub-recipe fails. So read a recipe step's
`model` and `effort` as the launch policy in force for that step, not as proof
that a process was started with them.

### Adding a field to a step invalidates its stored progress

Recovery fingerprints the serialised step definition to decide whether a stored
record still describes the step in front of it. `context_mode` and `effort` are
part of that definition, so adding, changing or removing either on a step
changes its fingerprint: on the next resume the stored record no longer matches
and the step re-executes. Make these edits between runs, not between a failure
and its resume.

A step that declares neither field serialises exactly as it did before, so every
recovery record stored by an earlier build stays valid.

## Rules for launcher authors

The runner can check the shape of what it hands over. It cannot check what is
done with it. These six rules are the launcher's share of the contract.

1. **`AMPLIHACK_STEP_CONTEXT` is a declaration, not a capability.** It says how
   much context to assemble, never how much trust to extend. Do not use it to
   relax tool permissions, approval prompts, sandboxing or auditing, and do not
   read it as evidence that secrets are absent from the agent's context.
   Presence means *less context*, never *more access*.
2. **Treat `AMPLIHACK_STEP_ID` as untrusted recipe data.** Never interpolate it
   unquoted into a shell command, and never use it as an unsanitised path
   component — `../` is a legal step id today. Because ids are recipe-local,
   never key a global lookup on it alone.
3. **Treat `AMPLIHACK_STEP_EFFORT` as untrusted.** It is rendered from recipe
   context, which can be fed by agent output. Match it against the harness's own
   vocabulary and ignore anything unrecognised; do not forward it unexamined to
   another program.
4. **Absence is the safe state.** A launcher that cannot read a variable, or
   finds a value it does not recognise, must start the step the way it does
   today — more context, not less. Never infer a restriction from a missing
   variable.
5. **Do not widen the disclosure.** `model` is already world-readable on argv,
   which is why no secret belongs in it; do not copy the environment values onto
   a command line, into a log shared more widely than the harness's own, or into
   a remote request.
6. **Do not withhold security-relevant context.** A minimal start trims context
   the step does not need, not context a reviewer or a failure analysis depends
   on. The recovery launch never carries the mark for exactly this reason;
   apply the same judgement to anything a harness treats as a review.

## Worked example

```yaml
name: triage
version: "1.0"

steps:
  - id: classify
    agent: analyzer
    prompt: "Classify this diff as feature, fix or chore. Answer with one word.\n\n{{diff}}"
    context_mode: minimal
    model: fast
    effort: low
    output: category

  - id: review
    agent: reviewer
    prompt: "Review this {{category}} change in full.\n\n{{diff}}"
    model: strong
```

The `classify` step's agent child receives:

```text
AMPLIHACK_STEP_CONTEXT=minimal
AMPLIHACK_STEP_EFFORT=low
AMPLIHACK_STEP_ID=classify
```

and `--model fast` on the launcher argv. Its record contains
`"context_mode": "minimal"`, `"model": "fast"` and `"effort": "low"`.

The `review` step's child receives `AMPLIHACK_STEP_ID=review` and nothing else
from the contract, and `--model strong` on argv. Its record contains
`"model": "strong"` and neither of the other two keys.

`fast` and `strong` are tier words resolved by amplihack's launcher, not by the
runner. The runner renders the field, trims it, and passes the result through
byte for byte, so a literal model identifier such as `haiku` behaves the same
way.

## Related

- [YAML Recipe Format](yaml-format.md#lightweight-agent-steps-context_mode):
  the `context_mode` and `effort` recipe fields and what a bad value looks like.
- [Environment Budget & Safe Spawning](env-budget.md#protected-variables): why
  these variables survive budget trimming.
- [`--env-allow`](cli-reference.md#--env-allow-name): what the allowlist does
  and does not cover.
- [Rate-Limit Handling](rate-limit-handling.md): the adapter-side model
  override the record does not report.
