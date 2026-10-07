---
title: Testing and edge-case recipes
description: Regression recipe catalog and Codex verification coverage.
doc_type: reference
last_updated: 2026-10-07
---

# Testing & Edge-Case Recipes

Recipes designed to exercise specific recipe runner features and edge cases.
Useful as regression tests and as references for condition syntax.

Source: [recipes/testing/](https://github.com/rysweet/amplihack-recipe-runner/tree/main/recipes/testing)

## Recipes

| Recipe | What It Tests |
|--------|---------------|
| [all-condition-operators](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/all-condition-operators.yaml) | Every comparison and boolean operator: `==`, `!=`, `<`, `<=`, `>`, `>=`, `and`, `or`, `not`, `in`, `not in` |
| [all-functions](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/all-functions.yaml) | All whitelisted functions: `int()`, `str()`, `len()`, `bool()`, `float()`, `min()`, `max()` |
| [all-methods](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/all-methods.yaml) | All whitelisted string methods: `strip()`, `lstrip()`, `rstrip()`, `lower()`, `upper()`, `title()`, `startswith()`, `endswith()`, `replace()`, `split()`, `join()`, `count()`, `find()` |
| [output-chaining](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/output-chaining.yaml) | Step output stored in context and referenced by subsequent steps via `{{variable}}` |
| [json-extraction-strategies](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/json-extraction-strategies.yaml) | All 3 JSON extraction strategies: direct parse, markdown fence, balanced braces |
| [step-type-inference](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/step-type-inference.yaml) | Automatic step type detection: bash (command), agent (agent field), recipe (recipe field), agent (prompt-only) |
| [continue-on-error-chain](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/continue-on-error-chain.yaml) | `continue_on_error: true` allowing subsequent steps to run after failures |
| [nested-context](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/nested-context.yaml) | Dot-notation access to nested context values: `{{config.database.host}}` |
| [large-context](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/large-context.yaml) | Many context variables and long values to test template rendering at scale |
| [empty-and-edge-cases](https://github.com/rysweet/amplihack-recipe-runner/blob/main/recipes/testing/empty-and-edge-cases.yaml) | Empty strings, missing variables, whitespace-only values, special characters |

## Codex verification

The [Codex agent-step contract](codex-exec.md) requires both deterministic
regression tests and production integration evidence. The following checks guide
acceptance; deterministic fixtures alone do not establish production completion.

Use an isolated fake launcher via `AMPLIHACK_LAUNCHER_BINARY` for deterministic
coverage. Have it record argv, stdin and cwd, emit distinct progress diagnostics,
and write controlled final files. Keep fixtures and output under a private
`mktemp -d` directory and clean it up after the checks.

| Area | Required checks |
|---|---|
| Capability probe | Exact frozen JSON and package version, empty stderr, standalone argument enforcement, stdout-write failure, and no logging/update/cache/network effects under ambient logging/update settings. |
| Prompt and dispatch | Small and large prompts, multiline/Unicode/leading-dash/NUL content, full system/persona/no-reentry/autonomy envelope, stdin EOF, effective cwd, explicit model only, no implicit add-dir, and unchanged Claude/Copilot argv/results. |
| Final response | Progress excluded; whitespace and trailing newlines preserved; existing empty file accepted; missing, directory, symlink, unsafe permissions, unreadable and invalid UTF-8 files rejected. Exact `MAX_STEP_OUTPUT_BYTES` succeeds; one byte over fails without unbounded allocation or silent truncation. |
| Lifecycle | Spawn/write failure, blocked or partially consumed stdin, immediate zero timeout, nonzero exit despite a final file, lingering descendants, owned-tree termination/reaping, I/O completion and cleanup errors. Verify private permissions and absent initial final path. |
| Retries | Fresh output resources, no stale-file reuse, complete context/model/timeout preservation for rate-limit and JSON repair attempts, bounded backoff, Codex excluded from auto-model fallback, and local delivery/file/decoding/cleanup failures not retried based on diagnostic text. |
| Runner integration | Verbatim bounded raw output survives storage in step results/context; explicit JSON extraction still transforms output; ordinary failure/continue-on-error policies and Claude/Copilot regressions pass. |
| Terminal cleanup | Cleanup-only errors stop agent/Bash dispatch, hooks, nested recovery, nonfatal continuation, and both JSON-repair routes. Optional and required JSON parsing cannot accept degraded success after cleanup failure. Last-step post-hook failures produce failed results, listener notifications and audit entries, and prevent a successful checkpoint from being saved. |
| Parallel terminality | A worker publishes cleanup failure before joins; synchronized checks prove pending dispatch stops while already admitted work finishes and every started worker is joined. Original step attribution survives aggregation. |
| Terminal lifetime | Multilevel nesting and aggregation retain terminality; independent top-level runner reuse resets it. Ordinary errors, including cleanup/cancellation-looking diagnostic text, retain recovery, nonfatal continuation, optional JSON degradation, and hook warning policies. |

Cleanup-only fixtures compose the production cleanup error and assert that its
chain contains `CleanupFailure` and excludes `Interruption`. Call counters prove
that later steps, recovery, hooks, and repair never execute. Combined
interruption/cleanup coverage alone cannot establish this contract. Parallel
fixtures acknowledge the worker's classification boundary before releasing an
in-flight agent; bounded channel waits detect deadlocks without timing sleeps.

### Test locations and discovery

`tests/codex_exec_tests.rs` remains the integration-test target and retains the
root `adapter_worker` subprocess entrypoint selected by `--exact adapter_worker`.
Its scenarios share infrastructure under `tests/codex_exec/`:

| Module | Coverage |
|---|---|
| `fixtures.rs` | Isolated launcher, worker, and shared assertion helpers. |
| `transport.rs` | Stdin envelope, cwd, model forwarding, and Claude/Copilot compatibility. |
| `final_output.rs` | Final-file validation, verbatim output, size bounds, and safe diagnostics. |
| `lifecycle.rs` | Timeout, descendants, diagnostic streams, concurrency, and rate-limit retries. |
| `cancellation.rs` | Linux SIGINT and SIGTERM, eight recipe variants per signal. |

The target and every extracted module, including fixtures, contain at most 300
physical lines. Extraction retains every case, assertion, macro expansion,
platform gate, and worker selector. Discovery comparison maps changed module
qualifications; equal test counts alone do not establish preservation.

Private cleanup-only runner regressions live in
`src/adapters/codex_exec/tests/runner_cleanup/`: shared `fixtures.rs`,
`sequencing_hooks.rs`, `nested_recovery.rs`, `json.rs`, and `parallel.rs`.
Private lifecycle tests remain in `src/adapters/codex_exec/tests/`.
`tests/codex_capabilities_tests.rs` covers the standalone probe, and
`tests/codex_retry_context_tests.rs` covers repair context. Existing adapter and
runner suites retain ordinary provider/error-policy controls. No model API call
is needed for deterministic checks.

### Lifecycle discrimination and evidence

The [lifecycle reference](codex-lifecycle.md) defines group authority and
observable cancellation retirement. Acceptance uses identical production-path
semantic assertions on an immutable baseline and corrected source. Both epochs
must compile successfully before the selected assertion is evaluated. Compile
failure, zero selection, timeout, cached artifacts or an older defect reproduction
cannot substitute for semantic RED/GREEN.

| Area | Required independently selected scenarios and assertions |
|---|---|
| Transient zombie-only observations | Model the conditional Apple explicit-group semantics. Bound the observation to the owned unreaped launcher, successful bounded reap and final absence. Preserve the primary ordinary error and exclude false `CleanupFailure`. |
| Genuine cleanup failures | Independently inject real policy failures for permission, live groups, descendants, TERM, KILL, probes, launcher/helper reaping, readers and resources. Retain typed `CleanupFailure` and primary diagnostics, including genuine earlier failure followed by final absence. EPERM or absence alone is not transient proof. |
| Group authority | Prove readiness precedes launcher reap and retained membership lasts through final destructive access. Model unrelated PGID reuse and assert no unrelated signal; also exercise normal native descendant cleanup. |
| Anchor lifetime | Cover startup acknowledgment, malformed protocol, setup failure, unexpected exit, expiry, parent-loss EOF, direct fallback and reaping. Validate SIGCHLD ignore/automatic-reap rejection, competing-reaper loss and changed policy without global disposition changes. |
| Helper isolation and budgets | Exercise concurrent attempts, high-numbered descriptors and changed limits; verify unrelated descriptors and pipe writers close before readiness. Startup/deadline exhaustion, unwind, EINTR, fallback and Drop cannot renew windows. |
| Real signal retirement | Deliver actual unblocked SIGINT and SIGTERM while queried product handlers remain owned at each restoration boundary. Include TERM after INT restoration, observable entered/in-flight handlers and both signals in one epoch. Full-adapter returns retain typed `Interruption`. |
| Concurrent retirement | Exercise concurrent owners, nonlast and last close, cross-thread in-flight entry, independent next-owner/reset races and clean new epochs. An earlier immutable result remains interrupted; fresh baselines do not clear sequences. |
| Disposition and fault handling | Preserve prior custom full actions; exercise registration rollback, each restoration failure and bounded reconciliation. Verify pending metadata, saturation and fault diagnostics prevent false success. |
| Combined errors | Assert both `Interruption` and `CleanupFailure` downcasts through full adapter composition with process, reader, resource and restoration faults. Retain primary ordinary/exit diagnostics and both signal identities. |

Private lifecycle fixtures are organized under `src/adapters/codex_exec/tests/`:
`launcher_retirement.rs`, `group_authority.rs`, `anchor_lifecycle.rs`,
`anchor_descriptors.rs`, `retirement_signals.rs`, `retirement_concurrency.rs`,
`retirement_failures.rs`, `retirement_fixtures.rs` and `lifecycle_model.rs`.
Observational adaptations are transparent and identical between source epochs;
they gate real syscall, entry-publication and retirement boundaries without
changing causal control flow or classification. Tests never simulate signal
receipt by calling the handler or setting cancellation state. Kernel selection
before observable entry is separately qualified, not advertised as synchronized.

#### Preservation inventory

Retain and execute all 835 baseline controls, at least 51 cleanup regressions,
the 29 original integration cases and 58 original assertion sites, 29-case
discovery, the unchanged root `--exact adapter_worker` selector/body and both
existing eight-variant SIGINT/SIGTERM matrices. Preserve extracted modules and
test targets. Every new source, test and helper module has at most 300 physical
lines. An aggregate pass count cannot replace identity/assertion comparison.

The 50-worker oracle checks each `admitted-0` through `admitted-47` completion
and its attributed result exactly once, plus the held and cleanup identities.
A deterministic two-worker barrier proves distinct admission. Pure cleanup
terminal publication occurs before pending work and joins; already admitted
workers finish and are joined. Ordinary-error controls retain Claude/Copilot,
recovery, hook and JSON policies.

#### Evidence bindings

Each oracle records the actual command, allowlisted nonsecret environment,
resolved compiler/tool paths, executable SHA256 and versions, genuine compile
exit, selected-test count, semantic result and process exit. Bind immutable
source, fixtures, compiled binaries and logs by path and SHA256. Preserve failed
attempts. Historical missing compiler identity remains qualified; measuring a
current compiler does not repair earlier records.

Separate modeled policy coverage, native Linux fixture execution, target
compilation and native platform runtime. Conditional Apple UNIX03 source
inference is not a deployed Darwin ABI or macOS runtime result. Ubuntu-only
runner CI and another repository's Darwin CI do not verify new runner Darwin
bindings or helper behavior. Controlled adapter-worker evidence does not prove
live model execution or actual recipe continuation.

Focused and full suites, formatting, warnings-denied all-target Clippy and
pre-commit are acceptance checks. Their results belong in external logs/CI,
not this reference. Finding closure additionally requires independent review
and complete workflow/publication evidence; passing tests alone is insufficient.

### Production verification

Later production verification must use the updated production Amplihack launcher
and an absolute real Codex executable, without a bootstrap adapter. First inspect
`codex --version`, `codex exec --help`, and `recipe-runner-rs --capabilities`.
Then run a minimal recipe with `auto_stage: false` in a disposable workspace;
verify stdin passthrough, final-response-only completion, failure propagation,
and launcher process containment. Record sanitized commands, resolved executable
paths, versions and the tested immutable runner revision. A help/probe check alone
does not prove production task completion. Use idempotent tasks: retries may
repeat side effects, and each attempt's timeout plus backoff increases total time.
