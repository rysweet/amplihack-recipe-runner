# Codex transport fixtures

The deterministic fixtures exercise the production adapter with synthetic
launchers. They never call a live provider. Issue [#196](https://github.com/rysweet/amplihack-recipe-runner/issues/196)
is separate from issue #194 and PR #195.

## Source identity

The assigned implementation worktree is
`/Users/ryan/src/bb/workers/runner-codex-fixture-196/source`, on
`fix/issue-196-parallel-codex-fixtures`, in
`https://github.com/rysweet/amplihack-recipe-runner.git`. Its assigned base is
`0e3606a1913820f6b3d58bae68589f2fdc631694`. The caller is separately recorded as
`/Users/ryan/src/bb/repos/runner-reference-validation`.

## Startup investigation

The unchanged assigned-base module failed under default parallelism on Darwin:
16 passed and 14 failed in the TDD control. A fresh execution of its preserved
binary failed with 15 passed and 15 failed. These are executed controls, rather
than conclusions drawn from unchanged source. Historical candidate
`0638114ebcaa968ee42906a5922b636b66d8d4bc` supplied comparison evidence only.
Its serial result was diagnostic and does not qualify parallel execution.

Temporary measurements recorded worker-observed publication times, adapter
elapsed time and Python-local phase durations. Parent-observed markers are
publication-time upper bounds. Rust and Python clock origins were never
compared. Python imports took about 7–10 ms in the instrumented module. Most
failed attempts never published the first Python marker before the unchanged
one-second deadline.

A separate 24-process experiment held the Python code constant and changed how
it was started. Direct `/usr/bin/python3` invocation took 47–74 ms; executing
newly written Python shebang scripts took 240 ms–3.47 s. A source-bound shell
script that explicitly invoked Python still failed before most entry markers.
The delay therefore concerns activation of newly written executable scripts,
not the measured import work. The operating system service responsible for that
delay was not identified.

The correction makes the fake launcher a symlink named `python3` to the trusted
`/usr/bin/python3` interpreter. The adapter's first argument, the provider name,
selects a private Python source file in the synthetic working directory.
The fixture restores that provider name in its argument record, preserving
`codex -- exec --output-last-message PATH [--model MODEL] -` and the existing
Claude/Copilot contracts. Apple's interpreter dispatcher requires the basename
`python3`; a probe with the basename `launcher` failed and remains retained.
The source-bound corrected activation experiment reached Python in 25–85 ms for
all executed attempts. No deadline was extended and no tests were serialised.
Temporary measurements were removed from the implementation.

## Fixture contracts

`tests/codex_exec/fixtures.rs` installs the same launcher for adapter workers and
Linux cancellation tests. The provider sources have mode `0600`, inside the
private fixture root. Both adapter workers and cancellation runners call
`env_clear()` before supplying an explicit synthetic environment. The shared
allowlist sets `PATH=/usr/bin:/bin`, `LC_ALL=C`, private `HOME`, `TMPDIR`,
`CODEX_HOME` and `XDG_CONFIG_HOME` paths, `PYTHONNOUSERSITE=1`, the fixture root,
synthetic session-depth limits and rate-limit settings. Each caller adds its
scenario, private launcher path and required test configuration. The cancellation
runner also disables update checks. No ambient credential, Python path, dynamic
loader setting or provider override is required. Worker executables use
`current_exe()`, runners use Cargo's compiled binary path, Git uses
`/usr/bin/git`, and launchers use the trusted Python interpreter.
Each adapter still runs in a separate process. Prompts and stdin remain
synthetic, including large, multiline, leading-dash, Unicode and NUL inputs.

Each launcher entry adds a unique `attempt_id` to `record.json` and
`attempts.jsonl`. After its selected operation it atomically publishes
`scenario-<attempt_id>.json` with mode `0600`. Its four fields are
`schema_version`, `attempt_id`, `scenario` and `operation`. Retry attempts keep
separate receipts. Receipts contain no prompt, stdin or raw diagnostic text.
Readers reject missing, malformed, non-private or mismatched receipts.

Negative-output tests require their intended filesystem operation or exit path
and reject incidental timeouts. The blocked-stdin test requires its intentional
blocking receipt and the adapter deadline failure. The partial-stdin test
requires its early-exit receipt and incomplete stdin delivery. Immediate
zero-timeout rejection remains separate because no launcher execution is
expected. Authentication, trusted-directory errors, redaction, empty output,
output limits and resource-deletion failures retain their existing checks.
Four simultaneous attempts retain distinct private final paths and receipts.

## Process observation

`tests/codex_exec/process_observation.rs` uses targeted Linux `/proc` reads or
Darwin `proc_pidinfo(PROC_PIDTBSDINFO)`. It records PID, user, process group and
birth identity. Missing interfaces, read errors, malformed data and PID reuse
fail verification; none implies termination. Zombies are distinguished from
running processes and do not establish successful reaping.

The spawning parent captures the worker PID from `Child::id()` and immediately
observes its birth identity and parent relationship. A parent-side observer then
checks the live worker-to-launcher and launcher-to-descendant relationships,
including user and process group, and repeats the identity and relationship
queries before retaining ownership in memory. The launcher waits for atomic
publication of `descendant-live.json` inside the existing adapter deadline.
There is no readiness wait before that deadline. The worker captures
`descendant-return.json` immediately after adapter return. The parent checks both
receipts against its retained descendant identity before accepting the run.
Tests require confirmed absence or a matching stopped identity at that boundary.
Later termination cannot repair a failed boundary assertion.

Both PID records and observation receipts pass through `PrivateRecords::read`.
It opens one private, owned directory descriptor and reads single-component
names with `openat(O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)`. Validation uses the
opened file descriptor: records must be regular files owned by the current user,
with mode `0600` and one link. Reads are limited to 4,096 bytes; PID parsing
accepts at most ten decimal digits and a value greater than one. JSON parsing
uses the same bounded bytes. Reads and ownership capture check the observation
deadline. A symlink, FIFO, malformed record, excessive size, unsafe mode,
missing record or observation error causes explicit failure. The reader never
signals a process and never reopens a validated pathname.

Fallback signals require retained in-memory ownership evidence. Records cannot
create that authority after exit or reparenting. Before any fallback signal,
the parent checks every still-present identity against that evidence, then
rechecks each individual target before signalling. Missing authority, changed
birth identities or contradictory evidence prevent signalling. Targets are
individual verified processes, never a group inferred from a stale PID.
Birth checks and signalling remain separate system calls, so they do not provide
an atomic kernel process handle. Linux cancellation retains both SIGINT and
SIGTERM, all eight variants, readiness, process groups, retry counts and resource
checks. Its live processes are captured before cancellation and observed again
at runner return.

## Verification and custody

The implemented Darwin module passed all 30 tests under default parallelism
after the synthetic-environment and observation checks were added. Documentation
tests, formatting and all-target Clippy also passed. The required workspace run
passed 601 library tests and the Codex module, then failed two unrelated
`failed_bash_stdout_tests` cases, including an `Operation not permitted` error.
The full hook command failed its large-file check on the pre-existing staged
`.amplihack/graph_db` and `.amplihack/indexes/rust.scip` outputs. Those files were
preserved. A supplementary run with `--no-fail-fast` records remaining targets
without excluding either failure. These results do not establish workspace or
final-commit acceptance. The local Docker probe found no daemon socket; native
Linux execution remains a parent dependency. Complete logs,
source hashes, preserved executable hashes, commands, exits and sampled memory
receipts are retained under
`/Users/ryan/src/bb/worker-logs/runner-codex-fixture-196/implementation/`.
The TDD baseline and strengthened-test failures remain under the adjacent `tdd/`
directory. No previous evidence or Cargo outputs were deleted.

The implementation uses its exclusively created task-owned
`target-implementation` directory. Builds run sequentially after preceding jobs
have terminal receipts. Relevant executable versions are preserved before
replacement. Ordinary QA runs detached, with sampled four-GiB observation and
no aggregate command timeout. Sampling can miss short-lived descendants and
work outside observed ancestry; it is not a kernel-enforced memory cap.
Separate native qualification and proof jobs retain 595 seconds before TERM and
five seconds for cleanup. No such job was retried as ordinary QA.

Acceptance requires the same final commit on native Darwin and Linux, with
`cargo test --test codex_exec_tests`, `cargo test --all-targets`,
`cargo test --doc`, `cargo fmt --all --check`,
`cargo clippy --all-targets -- -D warnings` and
`pre-commit run --all-files`. Neither `RUST_TEST_THREADS` nor `--test-threads`
is used for acceptance. Unrelated workspace failures remain failures.

The parent must arrange unavailable Linux execution, independent exact-commit
reviews and the supported canonical acceptance facility. This implementation
step does not launch nested workers, models or recipe runners. Parent review
is required before any production-adapter change; none was needed for this fix.
A separate substantive open pull request must reference #196 and link the
retained baseline and final Darwin/Linux evidence. Neither that pull request
nor PR #195 may be merged. Missing facilities prevent a merge-ready claim.

## Review feedback and remaining acceptance

Acceptance is incomplete. The review fixes retain the trusted-interpreter
correction and attempt-specific completion receipts. The two graph-related
`.gitignore` additions have been removed. Graph outputs, manifests, logs and
failed-run evidence remain preserved; the graph files remain untracked.

Focused security tests inject harmless credential and provider-override
sentinels into a disposable parent process and exercise both real launch paths.
The worker checks its own environment before invoking the adapter; the synthetic
launcher checks for leakage while allowing the adapter's deliberate synthetic
provider selection. Record tests cover symlinks, FIFOs, directories, unsafe
permissions, hard links, excessive sizes, malformed data, missing records,
directory-path replacement and expired deadlines. Disposable process trees test
unrelated launcher and descendant records, stale worker and target births,
contradictory ownership, missing evidence and attempted reconstruction after
worker exit. Invalid evidence must leave the disposable processes running.
Verified evidence also exercises successful fallback cleanup after reparenting.

Review-fix execution evidence is retained under
`/Users/ryan/src/bb/worker-logs/runner-codex-fixture-196/implementation/review-feedback/`.
Each job records its command, Darwin platform, current HEAD, working-tree source
hashes, exit status, preserved Codex executable hash and separate sampled-memory
receipt. These runs use uncommitted changes over
`00850a030ea2217579499b4d99bcf7213c52e263` and cannot establish final-commit
acceptance. The first security-test run, the initial Clippy failure and the
workspace module-length failure remain intact. Private record-reading,
ownership and regression-test submodules keep every Codex integration source
file within the repository's 300-line limit.

The final source checks on Darwin recorded these results:

| Command | Result | Evidence directory |
| --- | --- | --- |
| `cargo test --test codex_exec_tests` | 39 passed, default parallelism | `module-scoped` |
| `cargo fmt --all --check` | Passed | `format-scoped` |
| `cargo clippy --all-targets -- -D warnings` | Passed | `lint-scoped` |
| `cargo test --doc` | One documentation test passed | `doc-tests-scoped` |
| `cargo test --all-targets` | Failed: the same two `failed_bash_stdout_tests` cases | `workspace-scoped` |
| `pre-commit run --all-files` | Passed on the current tracked-file set | `hooks-scoped` |

The workspace run passed all 601 library tests and all 39 Codex integration
tests before reaching two failures outside the authored diff. The supplied base
control covers only the Codex module, with 16 passes and 14 failures. Base
workspace behaviour for the filesystem cases is unverified.
`retention_problem_identifies_non_utf8_directory_losslessly` failed while
creating its non-UTF-8 test path with `Operation not permitted`.
`non_utf8_pointer_round_trips_every_original_path_byte` found a retention error
where it expected a retained path. Their complete failure streams and private
fixtures remain under the review-feedback evidence directory. The passing
pre-commit command leaves the protected graph outputs untracked; it does not
supersede the earlier rejection when those files were staged. No hook was
excluded or bypassed.

The two `failed_bash_stdout_tests` failures require an authorised Darwin
filesystem investigation and a concrete runtime or separate scope decision.
The parent must resolve those failures and the
protected large-file hook rejection through authorised work, preserving original
evidence. It must commit the final revision, execute every required command on
that exact commit on Darwin and Linux, including both Linux cancellation signal
cases and normal workspace and hook checks, and obtain renewed independent
reviews and canonical acceptance. Evidence must distinguish test execution from
sampled memory and include commands, platform, commit identity and relevant
artifact hashes. Any later change requires renewed affected validation. The
separate open PR must reference #196 and link accessible baseline and acceptance
evidence. Neither it nor PR #195 may be merged.

## Linux fixture correction after review

The qualified Linux runtime measured umask `0022`, directory mode `0755`
and effective UID `1000` for `tempfile::tempdir()`. The default directory mode
is `0777` reduced by umask. This violates the observation reader's exact
`0700` requirement. Fixture roots now request `0700` at creation, before
writing records or spawning children. Replacement record directories also
request `0700`. The reader still checks the exact mode and effective UID.
An isolated child sets ordinary umask `0022` and checks both acceptance of a
private root and rejection after changing its mode to `0755`; the parallel
test harness never changes its own umask.

Direct-child guards now exist before identity capture or record validation can
fail. Observers retain verified ownership before fallible observation and
publication. The adapter watchdog joins its observer and releases retained
cleanup authority before returning its error. A synthetic launcher supervises
its own unreaped child handles until the observer retains ownership; failed
setup therefore requires no signalling authority from rejected PID records.
Regression controls inject directory and ancestry failures, observation and
publication errors, and a watchdog expiry. They check descendant termination,
direct-child reaping and survival of an unrelated tree.

These corrections do not establish a passing Darwin workspace. The exact
final Darwin receipt at `cde5b5db` remains a failure with 601 library passes,
39 Codex passes in 4.80 seconds, and the two filesystem failures described
above. No base-workspace run was supplied. The original failed parallel
control and diagnostic-only serial control remain evidence for issue #196.

## References

[Codex agent steps](src/codex-exec.md), [Codex lifecycle](src/codex-lifecycle.md)
and [testing recipes](src/testing-recipes.md) describe the production contracts
and wider test requirements.
