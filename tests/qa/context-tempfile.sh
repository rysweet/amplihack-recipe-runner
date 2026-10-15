#!/usr/bin/env bash
# Exercise private context spill and parallel cleanup through the built CLI.
# Linux QA only; requires bash, realpath, and Python 3 at /usr/bin/python3.
set -euo pipefail
: "${RECIPE_RUNNER_QA_BINARY:?set RECIPE_RUNNER_QA_BINARY to the candidate recipe-runner-rs binary}"
runner=$(realpath "$RECIPE_RUNNER_QA_BINARY")
fixture=$(mktemp -d "${TMPDIR:-/tmp}/recipe-runner-context-qa.XXXXXX")
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

worker.write_text(
    """import json, os, pathlib, sys, time
name, records, expected = sys.argv[1], pathlib.Path(sys.argv[2]), int(sys.argv[3])
context_path = pathlib.Path(os.environ["AMPLIHACK_CONTEXT_FILE"])
(records / (name + ".ready")).write_text(str(context_path))
deadline = time.monotonic() + 10
while not all((records / (other + ".ready")).exists() for other in ("first", "second")):
    if time.monotonic() >= deadline:
        raise SystemExit("parallel context workers did not overlap")
    time.sleep(0.01)
context = json.loads(context_path.read_text())
payload = context["qa_payload"]
assert len(payload) == expected, (len(payload), expected)
assert payload == "x" * (expected - 1) + "\\n"
assert context["qa_marker"] == "context-spill-fixture"
(records / (name + ".json")).write_text(json.dumps({
    "path": str(context_path), "payload_length": len(payload),
    "payload_sha256": __import__("hashlib").sha256(payload.encode()).hexdigest(),
    "marker": context["qa_marker"],
}))
print("context worker " + name)
""",
    encoding="utf-8",
)
control.write_text(
    """import hashlib, json, os, pathlib, sys
records, expected = pathlib.Path(sys.argv[1]), int(sys.argv[2])
context_path = pathlib.Path(os.environ["AMPLIHACK_CONTEXT_FILE"])
context = json.loads(context_path.read_text())
payload = context["qa_payload"]
assert len(payload) == expected, (len(payload), expected)
assert payload == "x" * (expected - 1) + "\\n"
assert context["qa_marker"] == "context-spill-fixture"
(records / "cleanup-control.json").write_text(json.dumps({
    "path": str(context_path), "payload_length": len(payload),
    "payload_sha256": hashlib.sha256(payload.encode()).hexdigest(),
    "marker": context["qa_marker"],
}))
print("context cleanup control")
""",
    encoding="utf-8",
)
invalid.write_text(
    """import os, pathlib, sys
pathlib.Path(sys.argv[1]).write_text(os.environ["AMPLIHACK_CONTEXT_FILE"])
print("this is deliberately not JSON")
""",
    encoding="utf-8",
)

def yaml_quote(value: str) -> str:
    return json.dumps(value)

steps = []
for name in ("first", "second"):
    command = shlex.join(
        ["/usr/bin/python3", str(worker), name, str(records), str(payload_size + 1)]
    )
    steps.extend(
        [
            f"  - id: {name}\n",
            "    type: bash\n",
            f"    command: {yaml_quote(command)}\n",
            "    parallel_group: context-spill\n",
        ]
    )
invalid_command = shlex.join(
    ["/usr/bin/python3", str(invalid), str(fixture / "invalid-context-path")]
)
control_command = shlex.join(
    ["/usr/bin/python3", str(control), str(records), str(payload_size + 1)]
)
steps.extend(
    [
        "  - id: invalid-required-json\n",
        "    type: bash\n",
        f"    command: {yaml_quote(invalid_command)}\n",
        "    parse_json: true\n",
        "    parse_json_required: true\n",
        "    parallel_group: json-cleanup\n",
        "  - id: cleanup-control\n",
        "    type: bash\n",
        f"    command: {yaml_quote(control_command)}\n",
        "    parallel_group: json-cleanup\n",
    ]
)
recipe = (
    "name: private-context-spill-qa\n"
    "version: '1.0'\n"
    "context:\n"
    "  qa_marker: context-spill-fixture\n"
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
assert result["success"] is False, result
steps = {step["step_id"]: step for step in result["step_results"]}
assert steps["first"]["status"] == "completed", steps["first"]
assert steps["second"]["status"] == "completed", steps["second"]
assert steps["invalid-required-json"]["status"] == "failed", steps["invalid-required-json"]
assert steps["cleanup-control"]["status"] == "completed", steps["cleanup-control"]
assert "parse_json failed" in steps["invalid-required-json"]["error"]

expected = int((fixture / "payload-size").read_text())
records = fixture / "records"
first = json.loads((records / "first.json").read_text())
second = json.loads((records / "second.json").read_text())
control = json.loads((records / "cleanup-control.json").read_text())
assert first["path"] != second["path"], (first, second)
assert control["path"] not in (first["path"], second["path"]), control
assert first["payload_length"] == second["payload_length"] == expected
digest = hashlib.sha256(("x" * (expected - 1) + "\n").encode()).hexdigest()
assert first["payload_sha256"] == second["payload_sha256"] == digest
assert first["marker"] == second["marker"] == "context-spill-fixture"
for record in (first, second):
    assert not pathlib.Path(record["path"]).exists(), record
assert control["payload_length"] == expected
assert control["payload_sha256"] == digest
assert control["marker"] == "context-spill-fixture"
assert not pathlib.Path(control["path"]).exists(), control

pid = (fixture / "runner.pid").read_text()
legacy = fixture / "tmp" / f"amplihack-context-{pid}.json"
assert legacy.is_symlink(), legacy
assert legacy.resolve() == (fixture / "protected.json").resolve()
assert (fixture / "protected.json").read_bytes() == b"protected bytes must survive\n"
invalid_context = pathlib.Path((fixture / "invalid-context-path").read_text())
assert not invalid_context.exists(), invalid_context
assert str(invalid_context) != control["path"]
PY

printf 'PASS: private spill, concurrent file ownership, planted-path preservation, and failure cleanup\n'
