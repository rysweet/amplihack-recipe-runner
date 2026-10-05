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
| Runner integration | Verbatim bounded raw output survives storage in step results/context; explicit JSON extraction still transforms output; existing failure/continue-on-error policies and Claude/Copilot regressions pass. |

Test locations are `tests/codex_capabilities_tests.rs`,
`tests/codex_exec_tests.rs`, private lifecycle unit tests in
`src/adapters/codex_exec/tests/`, and existing adapter/runner regression suites.
No model API call is needed for deterministic checks.

Later production verification must use the updated production Amplihack launcher
and an absolute real Codex executable, without a bootstrap adapter. First inspect
`codex --version`, `codex exec --help`, and `recipe-runner-rs --capabilities`.
Then run a minimal recipe with `auto_stage: false` in a disposable workspace;
verify stdin passthrough, final-response-only completion, failure propagation,
and launcher process containment. Record sanitized commands, resolved executable
paths, versions and the tested immutable runner revision. A help/probe check alone
does not prove production task completion. Use idempotent tasks: retries may
repeat side effects, and each attempt's timeout plus backoff increases total time.
