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
private fixture root. Workers use synthetic home/configuration directories,
disable Python user-site loading and remove inherited Python path overrides.
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

A worker observes each ready synthetic descendant while the launcher keeps it
alive. The launcher waits for atomic publication of `descendant-live.json`
inside the existing adapter deadline. There is no readiness wait before that
deadline. The worker captures `descendant-return.json` immediately after adapter
return, before joining its observer or attempting fallback cleanup. Tests require
confirmed absence or a matching stopped identity at that boundary. Later
termination cannot repair a failed boundary assertion.

Fallback signals require a fresh matching birth, user and process group. They
target individual verified processes, never a group inferred from a stale PID.
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

## References

[Codex agent steps](src/codex-exec.md), [Codex lifecycle](src/codex-lifecycle.md)
and [testing recipes](src/testing-recipes.md) describe the production contracts
and wider test requirements.
