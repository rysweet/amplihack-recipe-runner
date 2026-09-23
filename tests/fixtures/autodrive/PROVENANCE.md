# Vendored auto-drive-to-merge fixture

These files are copies taken from `rysweet/amplihack-rs` so that the tests for
issue rysweet/amplihack-rs#1468 can run the *real* `autodrive-crusty-loop`
preflight, rather than a paraphrase of it. The preflight is the step whose
`parse_json` output never reached the consuming step's environment, and the
recipes' own guard tests did not catch that the skill could not run at all.

| File | Source | Commit |
| --- | --- | --- |
| `amplifier-bundle/tools/autodrive_state.sh` | `amplifier-bundle/tools/autodrive_state.sh` | `90efd592` |
| `crusty-loop-preflight.yaml` (step bodies) | `amplifier-bundle/recipes/autodrive-crusty-loop.yaml` | `90efd592` |

`autodrive_state.sh` sha256: `9520e7f9f1fa709eb7dd87775fb09aca22485a2d1679b23c373cd99c5701255a`

## Drift

These are copies, so they can drift from upstream. They are pinned deliberately:
the test asserts a property of *this* runner (that a `parse_json` output is
exported to a later bash step), using a preflight of the exact shape the
workflow actually ships. If upstream changes the preflight, refresh the copies
and re-record the commit above; the assertion itself does not depend on the
preflight's contents beyond its emitting a `state_dir` field.

## `bin/amplihack`

`bin/amplihack` is a stand-in for the two `amplihack orch helper` subcommands
the consuming step pipes through. `amplihack` is not a dependency of this
repository and is not present in CI, and the test must not silently skip when
it is absent. The shim is put first on `PATH` unconditionally, so the test
behaves identically on a developer machine and in CI. It implements the same
contract as the real subcommands: `extract-json --require-field NAME` prints
the last JSON object in the input carrying `NAME` (or `{}`), and
`extract-field --field NAME --default D` prints that field or `D`.
