#!/usr/bin/env bash
# Exercise the #130 per-value env spill and its cleanup through the built CLI.
# Linux QA only; requires bash, realpath, sha256sum, and Python 3 at /usr/bin/python3.
# Failure output reports only lengths and SHA-256 digests, never the payload.
set -euo pipefail
: "${RECIPE_RUNNER_QA_BINARY:?set RECIPE_RUNNER_QA_BINARY to the candidate recipe-runner-rs binary}"
runner=$(realpath "$RECIPE_RUNNER_QA_BINARY")
fixture=$(mktemp -d "${TMPDIR:-/tmp}/recipe-runner-env-spill-qa.XXXXXX")
trap 'if [[ ${QA_KEEP_FIXTURE:-0} == 1 ]]; then printf "QA fixture retained: %s\\n" "$fixture" >&2; else rm -rf -- "$fixture"; fi' EXIT
mkdir -p "$fixture/home" "$fixture/tmp" "$fixture/project" "$fixture/records"

/usr/bin/python3 - "$fixture" <<'PY'
import json
import pathlib
import shlex
import sys

fixture = pathlib.Path(sys.argv[1])
project = fixture / "project"
worker = fixture / "worker.py"
control = fixture / "control.py"
invalid = fixture / "invalid.py"
records = fixture / "records"
payload_size = 200_000

# Every value is read in bash: the step passes the payload's length and
# SHA-256, whether it is exported, and whether AMPLIHACK_CONTEXT_FILE is set.
# The workers only see those small summaries as arguments.
observe = (
    '"${#RECIPE_VAR_qa_payload}" '
    '"$(printf %s "$RECIPE_VAR_qa_payload" | sha256sum | cut -c1-64)" '
    '"$RECIPE_VAR_qa_marker" '
    '"$(env | grep -c \'^RECIPE_VAR_qa_payload=\' || true)" '
    '"${AMPLIHACK_CONTEXT_FILE+set}"'
)

common = """import json, os, pathlib, sys
def summary(args):
    length, digest, marker, exported, legacy = args
    assert "AMPLIHACK_CONTEXT_FILE" not in os.environ, "AMPLIHACK_CONTEXT_FILE is set"
    return {
        "payload_length": int(length), "payload_sha256": digest, "marker": marker,
        "exported": exported, "legacy_set": legacy,
    }
def spill_dirs():
    tmp = pathlib.Path(os.environ["TMPDIR"])
    return sorted(str(p) for p in tmp.glob("amplihack-env-spill-*"))
"""

worker.write_text(
    common
    + """import time
name, records = sys.argv[1], pathlib.Path(sys.argv[2])
record = summary(sys.argv[3:8])
def barrier(stage):
    (records / (name + stage)).write_text("")
    deadline = time.monotonic() + 10
    while not all((records / (other + stage)).exists() for other in ("first", "second")):
        if time.monotonic() >= deadline:
            raise SystemExit("parallel env-spill workers did not overlap")
        time.sleep(0.01)
# Both workers list TMPDIR between the two barriers, while both steps are live.
barrier(".ready")
record["spill_dirs"] = spill_dirs()
barrier(".seen")
(records / (name + ".json")).write_text(json.dumps(record))
print("env-spill worker " + name)
""",
    encoding="utf-8",
)
control.write_text(
    common
    + """records = pathlib.Path(sys.argv[1])
record = summary(sys.argv[2:7])
record["spill_dirs"] = spill_dirs()
(records / "cleanup-control.json").write_text(json.dumps(record))
print("env-spill cleanup control")
""",
    encoding="utf-8",
)
invalid.write_text(
    common
    + """records = pathlib.Path(sys.argv[1])
record = summary(sys.argv[2:7])
record["spill_dirs"] = spill_dirs()
(records / "invalid.json").write_text(json.dumps(record))
print("this is deliberately not JSON")
""",
    encoding="utf-8",
)

def yaml_quote(value: str) -> str:
    return json.dumps(value)

def command(script: pathlib.Path, *args: str) -> str:
    return shlex.join(["/usr/bin/python3", str(script), *args]) + " " + observe

steps = []
for name in ("first", "second"):
    steps.extend(
        [
            f"  - id: {name}\n",
            "    type: bash\n",
            f"    command: {yaml_quote(command(worker, name, str(records)))}\n",
            "    parallel_group: env-spill\n",
        ]
    )
steps.extend(
    [
        "  - id: invalid-required-json\n",
        "    type: bash\n",
        f"    command: {yaml_quote(command(invalid, str(records)))}\n",
        "    parse_json: true\n",
        "    parse_json_required: true\n",
        "    parallel_group: json-cleanup\n",
        "  - id: cleanup-control\n",
        "    type: bash\n",
        f"    command: {yaml_quote(command(control, str(records)))}\n",
        "    parallel_group: json-cleanup\n",
    ]
)
recipe = (
    "name: env-spill-qa\n"
    "version: '1.0'\n"
    "context:\n"
    "  qa_marker: env-spill-fixture\n"
    "  qa_payload: |\n"
    + "    "
    + "x" * payload_size
    + "\nsteps:\n"
    + "".join(steps)
)
assert len(recipe.encode()) < 1_000_000, len(recipe)
(project / "recipe.yaml").write_text(recipe, encoding="utf-8")
(fixture / "payload-size").write_text(str(payload_size + 1), encoding="ascii")
PY

printf 'protected bytes must survive\n' > "$fixture/protected.json"
set +e
env -i \
    HOME="$fixture/home" PATH=/usr/bin:/bin TMPDIR="$fixture/tmp" \
    RECIPE_RUNNER_NO_UPDATE_CHECK=1 QA_PID_FILE="$fixture/runner.pid" \
    /bin/bash -c '
        ulimit -S -s 1024
        printf "%s" "$$" > "$QA_PID_FILE"
        legacy="$TMPDIR/amplihack-context-$$.json"
        ln -s "$1" "$legacy"
        exec "$2" "$3" --working-dir "$4" --no-auto-stage --output-format json
    ' _ "$fixture/protected.json" "$runner" "$fixture/project/recipe.yaml" "$fixture/project" \
    > "$fixture/result.json" 2> "$fixture/stderr.log"
status=$?
set -e
if [[ $status -eq 0 ]]; then
    printf 'expected required JSON failure, but recipe succeeded\n' >&2
    exit 1
fi

/usr/bin/python3 - "$fixture" "$status" <<'PY'
import hashlib
import json
import pathlib
import sys

fixture = pathlib.Path(sys.argv[1])
status = int(sys.argv[2])
result = json.loads((fixture / "result.json").read_text())
assert status != 0
assert result["success"] is False, "recipe unexpectedly succeeded"
steps = {step["step_id"]: step for step in result["step_results"]}
for step_id, want in (
    ("first", "completed"),
    ("second", "completed"),
    ("invalid-required-json", "failed"),
    ("cleanup-control", "completed"),
):
    assert steps[step_id]["status"] == want, (step_id, steps[step_id]["status"], want)
assert "parse_json failed" in steps["invalid-required-json"]["error"]

expected = int((fixture / "payload-size").read_text())
digest = hashlib.sha256(("x" * (expected - 1) + "\n").encode()).hexdigest()
records = fixture / "records"
seen = {
    name: json.loads((records / f"{name}.json").read_text())
    for name in ("first", "second", "cleanup-control", "invalid")
}
for name, record in seen.items():
    assert record["payload_length"] == expected, (name, record["payload_length"], expected)
    assert record["payload_sha256"] == digest, (name, record["payload_sha256"], digest)
    assert record["marker"] == "env-spill-fixture", (name, record["marker"])
    assert record["exported"] == "0", (name, "payload must be shell-local, not exported")
    assert record["legacy_set"] == "", (name, "AMPLIHACK_CONTEXT_FILE must not be set")
    assert record["spill_dirs"], (name, "no spill directory under TMPDIR during the step")

# Both parallel workers were inside their steps at once, each with its own
# spill directory.
overlap = set(seen["first"]["spill_dirs"]) & set(seen["second"]["spill_dirs"])
assert len(overlap) >= 2, ("parallel steps must each get a spill directory", len(overlap))

tmp = fixture / "tmp"
leftover = sorted(p.name for p in tmp.glob("amplihack-env-spill-*"))
assert not leftover, ("spill directories left in TMPDIR", leftover)
for record in seen.values():
    for path in record["spill_dirs"]:
        assert not pathlib.Path(path).exists(), ("spill directory not removed", path)

pid = (fixture / "runner.pid").read_text()
legacy = tmp / f"amplihack-context-{pid}.json"
assert legacy.is_symlink(), legacy
assert legacy.resolve() == (fixture / "protected.json").resolve()
assert (fixture / "protected.json").read_bytes() == b"protected bytes must survive\n"
assert [p.name for p in tmp.glob("amplihack-context-*")] == [legacy.name], "legacy context path was written"
PY

printf 'PASS: per-value env spill, parallel spill directories, cleanup, and planted-path preservation\n'
