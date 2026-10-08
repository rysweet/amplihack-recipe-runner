---
title: Codex lifecycle reference
description: Private FIFO permissions, group authority, cleanup budgets and observable signal retirement contracts.
doc_type: reference
last_updated: 2026-10-07
---

# Codex lifecycle reference

Each Codex attempt owns its launcher, original process group, private anchor,
diagnostic readers, output resources and scoped cancellation registration.
The [Codex agent-step reference](codex-exec.md) describes usage and public APIs.
These lifecycle interfaces are private and add no recipe settings.

## Contents

- [Private interfaces](#private-interfaces)
- [Group authority](#group-authority)
- [Helper protocol and isolation](#helper-protocol-and-isolation)
- [Portable FIFO permissions](#portable-fifo-permissions)
- [Deadlines and cleanup order](#deadlines-and-cleanup-order)
- [Failure classification](#failure-classification)
- [Signal observation and retirement](#signal-observation-and-retirement)
- [Platform and evidence scope](#platform-and-evidence-scope)

## Private interfaces

| Module or interface | Contract |
|---|---|
| `OwnedProcess` in `process.rs` | Owns the launcher, attempt deadline and anchor. Establishes authority before polling can reap the launcher; stops delivery on cancellation or authority loss. |
| `GroupAnchor` in `group_anchor.rs` | Non-cloneable authority for the original group, held through the final destructive operation. |
| `anchor_io.rs` / `anchor_fifo.rs` | Own preallocated, atomically close-on-exec readiness/control endpoints and validate bounded acknowledgments. |
| `anchor_child.rs` | Joins the original group, isolates descriptors and signals, enforces helper lifetime and exits through raw `_exit`. |
| `group_cleanup.rs` | One bounded TERM/KILL/probe driver, including the preserved closure seam. |
| `launcher_cleanup.rs` | Bounded direct-launcher retirement preserving every signal and reap error. |
| `shutdown` / `reap_launcher` | Retain existing closure interfaces and operation-order assertions through one cleanup policy. |
| `signal_observations.rs` | Persistent per-SIGINT/SIGTERM saturating entry sequences and snapshots. |
| `Cancellation::install` / `close` | Shared ownership, saved full dispositions and immutable retirement results captured before ownership unlock. |
| `finish_resources(result, cancellation.close())` | Composes retirement with execution and cleanup while retaining typed terminal causes and primary diagnostics. |

Explicit close reports failures; close and Drop release each owner exactly once.
Emergency Drop cannot return an error and logs retirement failures; ordinary
completion explicitly closes resources and composes failures into the result.
The public `Adapter: Sync`, result types, capability schema and provider/model
policies follow the [existing adapter contract](codex-exec.md#rust-adapter-api).

## Group authority

The launcher creates its own process group with launcher PID equal to PGID.
Before any `try_wait` or other launcher reap, a private helper joins that same
group and acknowledges readiness. Its retained membership keeps group identity
owned through TERM and the final group KILL attempt, including when the launcher
has already exited. Destructive operations consume phase-appropriate authority.

| Anchor state | Allowed behavior |
|---|---|
| Establishing | Retain the unreaped launcher; validate helper readiness within the startup deadline. |
| Held | Deliver stdin, poll the launcher and perform authorized original-group teardown. |
| Sealed | Final destructive group access has finished, including a failed final KILL. Retire/reap the helper; perform observational probes only. |
| Released | Helper reaping and endpoint closure are complete; no group signaling is permitted. |
| Faulted | Record authority loss or protocol failure; prohibit uncertain destructive operations and clean positively owned resources within existing deadlines. |

A numeric PGID, same UID, successful earlier reap, cached PID or unprotected
membership snapshot is insufficient authority. Positive identities and checked
signed conversions exclude zero, broadcast and caller-group targets. A reused
numeric PGID cannot authorize signaling an unrelated group.

If helper establishment fails, the retained, positively owned launcher supplies
authority for bounded startup unwind only while its identity remains held.
If neither launcher nor anchor supplies authority, uncertain direct/group
signals stop and incomplete cleanup remains typed `CleanupFailure`.
Ordinary inherited descendants remain cleanup responsibilities; deliberately
detached sessions remain outside process-group containment.

The mechanism assumes cooperative exclusive reaping of owned children.
Explicit SIGCHLD `SIG_IGN`, `SA_NOCLDWAIT` and unknown competing custom reapers
are rejected without changing global signal policy. Reaping uses PID-specific
waits. Unexpected helper exit, control EOF, changed reaping policy, stolen child
or `ECHILD` invalidates authority when retention cannot be established.
Concurrent external signal-policy mutation or undetectable competing reapers
are outside the cooperating-process guarantee.

## Helper protocol and isolation

Readiness and control descriptors are allocated before fork. A fixed bounded
acknowledgment confirms group join, descriptor isolation, child-local signal
policy and lifetime setup before launcher polling or reap. Startup guards own
every child and endpoint throughout unwind. The helper performs no model work.

Every endpoint is created close-on-exec, so a concurrent launcher cannot retain
private protocol handles across exec. Linux uses `pipe2(O_CLOEXEC | O_NONBLOCK)`.
Other Unix targets use a directional FIFO. Its intended directory creation
contract is specified in [portable FIFO permissions](#portable-fifo-permissions).
Create the FIFO with mode 0600, open read first and write second with `O_CLOEXEC | O_NONBLOCK |
O_NOFOLLOW`, and verify type, owner, mode and descriptor identity. The constructor
unlinks the FIFO, explicitly closes the directory descriptor and removes the
directory before returning. Partial construction closes every acquired endpoint
and composes filesystem/close failures with the original error. No keeper writer
or read/write endpoint masks EOF on parent loss. Filesystem work finishes before
fork; the helper still performs only raw operations afterward.

Apple's [open semantics](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/man/man2/open.2)
and [FIFO implementation](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/miscfs/fifofs/fifo_vnops.c)
support this construction. It uses `mkfifo`, rather than `mkfifoat`, which Apple's
[SDK declarations](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/sys/stat.h)
restrict to macOS 13+. The other directory-relative operations are available
within Rust's macOS deployment floors (10.12 x86, 11.0 ARM); native SDK linking
and runtime remain unverified. Linux exercises the identical FIFO constructor,
including exec during construction and actual parent death on an untimed attempt.
Those controls do not establish native Darwin behavior.

Only the two protocol endpoints remain open in the helper. Every other inherited
descriptor is closed before readiness, including concurrent attempts' pipe
writers and credential-bearing handles. CLOEXEC alone is insufficient for a
helper that does not exec. On Linux, checked raw `close_range` calls use flags
zero over ranges excluding the retained endpoints; unavailable syscalls or
closure errors refuse readiness. Darwin queries its own post-fork kernel descriptor-table bound through
`proc_pidinfo(PROC_PIDLISTFDS)` and closes the complete bounded range, independent
of lowered resource limits. The child opens no descriptors during that closure.
A table exceeding 262,144 entries refuses readiness. This derives from the
[bound XNU table query](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/kern/proc_info.c)
and its [direct syscall wrapper](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/libsyscall/wrappers/libproc/libproc.c).
Other Unix close-all paths remain unverified. An unavailable isolation mechanism
is an establishment failure.

The complete post-fork child path uses validated async-signal-safe raw operations
and `_exit`, with no allocation, mutexes, logging, unwinding, standard I/O,
directory enumeration or destructors. Child-local masks/dispositions cover
SIGINT, SIGTERM and SIGPIPE without changing parent policy; the helper installs
local SIGINT/SIGTERM ignore dispositions before readiness. The helper survives
TERM while authority is needed. A timed attempt supplies a finite helper lifetime
ceiling derived from its attempt deadline plus maximum cleanup allowance.
Untimed attempts retain bounded establishment/retirement and parent-loss control
EOF; they have no fixed execution-duration ceiling. Expiry or EOF outside
intentional retirement is a typed fault when detected.
The inherited parent memory is not a memory sandbox.

## Portable FIFO permissions

The private `anchor_fifo::new() -> anyhow::Result<Pipe>` factory configures
`tempfile::Builder::permissions(Permissions::from_mode(0o700))` before `tempdir()`.
Both fixture roots passed to `construct()` use the same permission request
before `tempdir_in()`. Directory privacy holds at creation, without a later
chmod repair. Construction does not change the process umask.

The selected temporary parent follows Unix `TMPDIR` configuration, as described
in the [temporary-resource usage reference](codex-exec.md#provider-and-configuration).
The parent must be trusted and writable. Mode 0700 does not isolate malicious
same-UID or privileged processes or establish hostile-TMPDIR protection.
There is no recipe option, public FIFO API or provider-policy change. Ordinary
Linux endpoint selection retains `pipe2`; Linux mechanism controls explicitly
exercise the shared portable constructor.

Umask can remove requested permission bits but cannot add permissions. The
required behavior is:

| Child umask | Directory / FIFO contract |
|---|---|
| `000` | Exact 0700 / 0600; construction succeeds. |
| `002` | Exact 0700 / 0600; construction succeeds. |
| `022` | Exact 0700 / 0600; construction succeeds. |
| `077` | Exact 0700 / 0600; construction succeeds. |
| `0100`, `0200` | Owner bits are removed; construction refuses safely, retaining primary and genuine cleanup diagnostics. |

Exact directory/FIFO modes, types and effective-UID ownership remain mandatory,
including for effective root. Owner-bit-restricted objects are rejected rather
than widened. Other owner-bit-removing masks have no universal success guarantee.
Each opened endpoint must match the FIFO device/inode observed through
`AT_SYMLINK_NOFOLLOW`; directional, NONBLOCK, CLOEXEC and nofollow gates remain
mandatory. The private `construct(root, opened)` callback observes endpoint-open
boundaries; production supplies a no-op and performs filesystem work before fork.

Successful construction checks unlink, directory-descriptor close and directory
removal before returning endpoints. Failure closes acquired endpoints and retains
cleanup errors alongside the original diagnostic. A privacy refusal with clean
teardown preserves its primary error; genuine cleanup failures remain typed
`CleanupFailure`. Failure after launcher creation still invokes bounded owned
startup unwind. Cancellation combined with cleanup retains both terminal causes.

Verification requires independent production-factory and fixture observations
under all four common masks in disposable children, leaving the parent mask
unchanged. Observations must record requested and actual creation modes, ownership
and identity before names disappear. Controls must preserve readiness delivery,
both partial-open boundaries, unlink-error composition, concurrent-exec isolation
and actual parent-SIGKILL EOF
on an untimed attempt. Internal integration covers FIFO-to-anchor-to-launcher
ownership, natural helper exit/reaping, unrelated-launcher survival and final group
absence. These contracts do not assert completed runtime or platform acceptance;
see [verification requirements](testing-recipes.md#lifecycle-discrimination-and-evidence).
Step 13 acceptance, independent finding closure, current-head CI and publication
remain pending. Native Darwin runtime and deployed-ABI verification remain
unverified; Linux shared-constructor checks do not establish those guarantees.

## Deadlines and cleanup order

The absolute attempt deadline is computed immediately after successful launcher
spawn. Anchor setup, reader setup, delivery and execution consume that same
deadline. Overflow and zero timeout refuse before launcher entry; representable budgets
are measured again immediately after spawn. Synchronous spawn, syscall
scheduling and resource I/O have no hard real-time guarantee.

| Phase | Budget and ordering |
|---|---|
| Establishment | At most 100 ms, capped by remaining attempt time; failure cleanup is bounded. |
| Direct-launcher retirement | Stop stdin; initial nonblocking reap, direct TERM with at most 100 ms grace, direct KILL if still owned/live, then PID-specific reap all share cleanup start plus two seconds. Authority remains held. |
| Group TERM | With valid authority, attempt TERM and observe at most 100 ms grace. TERM/probe errors do not skip final KILL. |
| Final group KILL | Attempt while authority is held, then seal destructive access even on failure. |
| Helper retirement and confirmation | Direct helper fallback, reaping and final group observation share KILL-attempt start plus two seconds. Reap the anchor before confirmation to avoid helper-created zombie-only observations. Attempt final observation even after budget exhaustion; incomplete confirmation adds failure without another wait window. |
| Readers and resources | Stop/join readers before extraction; retain queued-byte drain limits of 1 MiB or 50 ms per reader, then validate/extract and explicitly close resources. |

Process polling totals at most 4.1 seconds plus existing reader/resource bounds.
EINTR, fallback and Drop do not renew budget windows. Unlimited execution removes
the attempt deadline, not helper startup/retirement bounds. Loss of authority
overrides the usual final-KILL obligation: uncertain targets are never signaled.

## Failure classification

Cleanup retains every genuine permission, direct/group TERM or KILL, probe,
launcher/helper reap, reader, resource and surviving-descendant failure. A failed
direct kill remains diagnostic even if the launcher is subsequently reaped.
Successful final absence cannot erase an earlier genuine failure.

| Scenario | Adapter result |
|---|---|
| Ordinary primary error; proven transient pre-reap zombie-only observation; bounded successful reap and final absence; no genuine cleanup failure | Preserve the ordinary primary error without adding `CleanupFailure`. |
| EPERM without causal zombie-only proof, live group, surviving descendant or genuine earlier signal/probe failure, even with later absence | Typed terminal `CleanupFailure`, retaining primary error and cleanup details. |
| Failed reap, reader/resource cleanup, anchor setup or authority retention | Typed terminal `CleanupFailure`; incomplete cleanup cannot produce success. |
| Observed SIGINT or SIGTERM during owned execution/restoration, with successful cleanup | Typed terminal `Interruption`, including its signal identity. |
| Interruption plus process, reader, resource or restoration failure | Both typed terminal causes remain discoverable, with interruption precedence and primary diagnostics retained. |

No blanket EPERM suppression exists. Neither errno nor absence alone establishes
the transient case. Linux distinguishes live members from zombies through
process metadata; other Unix targets require group absence after owned reaping.

Errors exclude prompts, credentials and raw model diagnostics. Genuine terminal
failures permit no rate-limit retry, JSON repair, recovery, nonfatal continuation
or degraded success. Ordinary errors retain their existing provider policies.
The runner publishes a worker's terminal classification before joins and pending
dispatch; already admitted work is joined and retains its original attribution.

## Signal observation and retirement

Static, target-proven lock-free per-signal sequences publish observable handler
entry using saturating atomics. Publication and snapshots use SeqCst ordering.
The handler preserves errno as required and performs no blocking, allocation,
locking or later epoch-dependent writes. The sequences are never reset.
Saturation is a permanent typed fault and refuses later installs before spawn.

The first owner captures immutable baselines before installation and saves both
complete prior `sigaction` dispositions. Concurrent owners share those baselines.
An observed change identifies cancellation and retains both signal identities
when both occurred; the mechanism is not an exact accounting of kernel deliveries.
An existing primary `Interruption` retains its representative signal; otherwise
SIGINT represents a both-signal result, with both identities kept in diagnostics.

| Ownership state | Contract |
|---|---|
| Idle | Begin an independent epoch only after successful prior retirement/reconciliation; capture fresh baselines without clearing sequences. |
| Active | Share baselines; nonlast close snapshots under the mutex without restoring handlers. |
| Retiring | Last close attempts both full prior-action restorations; samples observations and constructs immutable interruption/cleanup results before unlocking. |
| RestoreFailed | Retain original actions, pending per-signal restoration metadata and the epoch. A later install attempts each pending restoration once, snapshots the retained epoch and reconciles safely or refuses before spawn. |

Registration rollback and partial restoration follow the same retained-metadata
rule. A faulted epoch retains its last reported snapshot: a new publication
before or during reconciliation refuses independent admission rather than being
discarded with previously returned observations. Ownership overflow, poisoning and unsupported atomics fail with typed
diagnostics; they do not manufacture an idle successful state.

The guarantee includes entries published while product handlers remain owned
during restoration, even when the handler is paused in flight. SIGTERM remains
covered after SIGINT restoration while its product handler is still installed.
An independent next owner cannot clear an earlier owner's immutable result.
A post-close-only sample is insufficient.

A kernel-selected old handler that first publishes after the retirement snapshot
is outside demonstrated epoch attribution. It can publish after a new owner has
captured its baseline. Persistent atomics, the ownership mutex and `sigaction`
are not a universal pre-user-entry kernel synchronization guarantee. This
qualification does not exclude published entered/in-flight handler events.

## Platform and evidence scope

The conditional Darwin zombie-only case derives from primary Apple source:
[explicit-group filtering in XNU](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/kern/kern_sig.c),
[group departure during reaping](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/kern/kern_exit.c)
and the [UNIX03 kill wrapper](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/libsyscall/wrappers/kill.c).
That source supports the conditional inference; it verifies neither a shipped
Darwin ABI nor native macOS runtime behavior. Modeled tests establish policy
semantics, not native Darwin execution.

Runner CI is Ubuntu-only. The private lifecycle modules have been type-checked
for x86_64 Darwin with real Rust 1.97 target libraries. The full Darwin crate
check is blocked by the available C compiler's unsupported Darwin flags in
`ring`. No native Darwin runtime, deployed ABI, descriptor-closure execution or
retained-membership runtime has been verified. Other Unix helper paths remain
unverified. Capability advertisement and another repository's
Darwin CI do not supply that evidence. Unsupported proof obligations require
mechanism review rather than silent descriptor leaks or narrowed containment.

Controlled adapter-worker signal fixtures establish their recorded process and
typed-result behavior. They do not establish live Codex/model execution or
recipe continuation unless those routes are actually exercised. Compiler
identity absent from historical evidence remains absent; a current executable
hash cannot repair that history. See [verification contracts](testing-recipes.md#lifecycle-discrimination-and-evidence)
for causal assertions and evidence requirements.
