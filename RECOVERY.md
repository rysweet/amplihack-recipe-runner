# Durable recovery v1

Recovery is separate from `RecipeCheckpoint`. An embedding host authorizes the
new launch and supplies `recovery::admission::Admission`, durable run storage,
a normalized work identity, and a hash of execution settings. The CLI consumes
the resulting private `--recovery-descriptor`; dry runs do not open recovery.
Unix builds advertise `durable_recovery_v1`.

Admission serializes latest-attempt selection and sequence allocation. It checks
only the latest matching attempt, requires its durable `usage_limit` outcome,
and rejects an active owner through a held advisory lock. Registering a new
attempt writes a durable running record before advancing the latest marker.
Keep the marker even when deleting historical run directories.

The existing step executor uses the recovery writer before hooks and after
finalization. Replayed results retain their original typed context deltas,
null assignments, removals, output, status and duration. A child is addressed by
parent invocation, call-site step and child recipe. Completed child records
remain available beneath incomplete parents. Invalidating a step invalidates
its ordered suffix; other members of the same parallel group remain independent.
A completed parent cannot hide unrecordable children, changed child definitions,
or missing child resources.

## Explicit persistence

```yaml
name: example
context:
  input_file: /absolute/path/to/input
recovery:
  version: 1
  persist:
    context: [input_file, output]
    outputs: [work]
steps:
  - id: work
    command: cat {{input_file}}
    output: output
    recovery:
      replay: reuse
      resources:
        - kind: file
          path: '{{input_file}}'
```

`persist.context` grants permission to save complete typed values for the listed
keys. `persist.outputs` grants permission to save complete results for the listed
step IDs. No permission is inferred from the environment. An unpermitted input
or context change makes the step unrecordable. No values are redacted and then
claimed to be equivalent. Definition and settings fingerprints contain hashes,
not raw prompts or environment values. Permitted outputs may themselves contain
sensitive information: policies must reflect the application’s persistence rules.

`replay: never` is the default. `replay: rebind` also executes the step anew;
hosts use it for current authorization, session setup and ownership. A `live`
resource (`name` required) always prevents replay. Hosts must perform admission
before replaying any dependent steps. Recovery does not invent an authorization
service or reuse old session receipts.

Resources are `file` or `repository` with `path`, `artifact` with `path` and
`context_key`, or `live` with `name`. The active Git checkout is automatically
observed. Git revisions and worktree contents are compared with the cumulative
state after completed work, allowing the workflow’s own commits and branches.
Artifacts are copied before publication, verified on import, and rebound only
at their explicit context key. A parent containing child artifacts is re-entered
so the child can rebind them. Opaque output strings are never rewritten.
Declare files and other opaque dependencies that a command may read. Dynamic
external services and undeclared effects cannot be proven compatible: leave
those steps at `never` or `rebind`. Recovery never rolls back interrupted effects.

## Storage and delegation

Recovery files live in the admitted private directory, outside disposable
runtime storage. Directories are 0700; ordinary files are 0600, owner-checked,
single-linked, bounded and opened without following symlinks. Publication uses
an exclusive same-directory temporary file, file sync, atomic rename and parent
sync. Corrupt history falls back fresh; unsafe current storage or failed
publication stops execution. JSON rejects duplicate keys at every depth.

One writer publishes the completion tree. Trusted subprocesses receive a random
local IPC capability restricted to their invocation subtree. Only the root can
inspect the report, finalize or shut down the writer. Renewing a capability
revokes its prior generation and descendants before retry. Scoped capabilities
are retired at step exit and are not inherited by unrelated runner subprocesses.
The protocol bounds frames and attachments. On Linux it uses an abstract Unix
socket with peer UID validation; elsewhere it uses a private Unix socket path.
This protects against accidental cross-run attachment, not malicious processes
running with the same account’s privileges.

The trusted host supervisor calls `Client::outcome(UsageLimit)` before waiting.
A retry renews its attachment, durably changes status to running and revokes stale
updates. Successful work and root finalization supersede old blockage. After the
runner writer exits, the admitting host can finalize through `Admission::finish`
using its trusted failure classifier. Unknown termination without current durable
limit evidence starts fresh.

Each attempt imports its inherited records and artifact copies before execution,
so later recovery survives deletion of ancestors. A deletion racing import
produces a fresh fallback, never partial reuse. Cleanup must retain latest markers
and must not delete external resources merely because a recovery declaration
references them. No retirement command or automatic retention deadline is added.

The JSON CLI result adds `recovery`: mode, reason, source run/sequence, restored
steps, invalidations and actual restart invocation/step. Diagnostics contain
bounded identifiers and reasons, never context or attachment credentials.
