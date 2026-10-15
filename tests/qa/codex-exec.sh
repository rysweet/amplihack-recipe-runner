#!/usr/bin/env bash
# Exercise the published recipe-runner candidate through its CLI and a controlled launcher.
# Unix QA only; requires bash, realpath, and Python 3 at /usr/bin/python3.
set -euo pipefail
: "${RECIPE_RUNNER_QA_BINARY:?set RECIPE_RUNNER_QA_BINARY to the candidate recipe-runner-rs binary}"
runner=$(realpath "$RECIPE_RUNNER_QA_BINARY")
fixture=$(mktemp -d "${TMPDIR:-/tmp}/recipe-runner-codex-qa.XXXXXX")
trap 'if [[ ${QA_KEEP_FIXTURE:-0} == 1 ]]; then printf "QA fixture retained: %s\\n" "$fixture" >&2; else rm -rf -- "$fixture"; fi' EXIT
home="$fixture/home"
project="$fixture/project"
mkdir -p "$home" "$project/.claude/agents/qa/core" "$fixture/bin"
cat > "$project/.claude/agents/qa/core/persona.md" <<'PERSONA'
System/persona fixture text: SYSTEM_SENTINEL PERSONA_SENTINEL.
PERSONA

cat > "$fixture/bin/launcher" <<'LAUNCHER'
#!/usr/bin/python3
import json
import os
import pathlib
import sys

root = pathlib.Path(os.environ["QA_RECORD_DIR"])
root.mkdir(parents=True, exist_ok=True)
attempt = len(list(root.glob("attempt-*.json"))) + 1
args = sys.argv[1:]
stdin = sys.stdin.buffer.read() if os.environ.get("QA_PROVIDER") == "codex" else b""
final = None
if "--output-last-message" in args:
    final = pathlib.Path(args[args.index("--output-last-message") + 1])
record = {
    "args": args,
    "provider": os.environ["QA_PROVIDER"],
    "attempt": attempt,
    "stdin_file": f"attempt-{attempt}.stdin",
    "final_absent_before": final is None or not final.exists(),
}
(root / record["stdin_file"]).write_bytes(stdin)
(root / f"attempt-{attempt}.json").write_text(json.dumps(record))

if record["provider"] == "codex":
    print("STDOUT_PROGRESS_SENTINEL", flush=True)
    if attempt == 1:
        print("rate limit fixture", file=sys.stderr, flush=True)
        sys.exit(1)
    if final is None:
        sys.exit(90)
    final.write_bytes(b"  CODEX_FINAL\n\x00Unicode: \xce\xbb\n\n")
else:
    print("LEGACY_RESULT_SENTINEL", flush=True)
LAUNCHER
chmod 755 "$fixture/bin/launcher"

write_recipe() {
    local model=$1
    cat > "$project/recipe.yaml" <<'RECIPE'
name: codex-exec-acceptance
version: "1.0"
context: {}
steps:
  - id: controlled-agent
    type: agent
    agent: "qa:core:persona"
    prompt: |
      TASK_SENTINEL: return the controlled response exactly.
    timeout: 30
RECIPE
    if [[ -n "$model" ]]; then
        printf '    model: %s\n' "$model" >> "$project/recipe.yaml"
    fi
    cat >> "$project/recipe.yaml" <<'RECIPE'
    auto_stage: false
    output: response
RECIPE
}

run_case() {
    local provider=$1 model=$2 record_dir=$3
    write_recipe "$model"
    mkdir -p "$record_dir"
    set +e
    (cd "$project" && env -i \
        HOME="$home" PATH=/usr/bin:/bin TMPDIR="$fixture" \
        RECIPE_RUNNER_NO_UPDATE_CHECK=1 \
        AMPLIHACK_LAUNCHER_BINARY="$fixture/bin/launcher" \
        AMPLIHACK_RATELIMIT_MAX_RETRIES=1 \
        AMPLIHACK_RATELIMIT_BASE_DELAY_SECS=0 \
        AMPLIHACK_RATELIMIT_MAX_DELAY_SECS=0 \
        AMPLIHACK_RATELIMIT_FALLBACK_AUTO_MODEL=1 \
        QA_PROVIDER="$provider" QA_RECORD_DIR="$record_dir" \
        "$runner" recipe.yaml --agent-binary "$provider" \
            --working-dir "$project" --no-auto-stage --output-format json) \
        > "$record_dir/result.json" 2> "$record_dir/stderr.log"
    local status=$?
    set -e
    if [[ $status -ne 0 ]]; then
        cat "$record_dir/stderr.log" "$record_dir/result.json" >&2
        return "$status"
    fi
}

validate_codex_case() {
    local mode=$1 record_dir=$2
    /usr/bin/python3 - "$mode" "$record_dir" <<'PY'
import json
import pathlib
import sys

mode, directory = sys.argv[1:]
root = pathlib.Path(directory)
result = json.loads((root / "result.json").read_text())
assert result["success"] is True, result
expected = "  CODEX_FINAL\n\x00Unicode: λ\n\n"
assert result["step_results"][0]["output"] == expected
assert result["context"]["response"] == expected
assert "STDOUT_PROGRESS_SENTINEL" not in (root / "result.json").read_text()
attempts = sorted(root.glob("attempt-*.json"))
assert len(attempts) == 2, [p.name for p in attempts]
for path in attempts:
    record = json.loads(path.read_text())
    args = record["args"]
    assert args[:3] == ["codex", "--", "exec"], args
    assert args[-1] == "-", args
    assert "--output-last-message" in args, args
    assert record["final_absent_before"] is True, record
    for forbidden in ("-p", "--add-dir", "--system-prompt", "--json"):
        assert forbidden not in args, args
    if mode == "default":
        assert "--model" not in args, args
    else:
        index = args.index("--model")
        assert args[index + 1] == "caller-model", args
    payload = (root / record["stdin_file"]).read_bytes().decode("utf-8")
    for marker in (
        "SYSTEM_SENTINEL",
        "PERSONA_SENTINEL",
        "TASK_SENTINEL",
        "You are already inside an active",
        "Do not invoke /dev",
        "Execute the requested step directly",
        "Proceed autonomously. Do not ask questions.",
    ):
        assert marker in payload, (marker, path.name)
PY
}

validate_legacy_case() {
    local provider=$1 record_dir=$2
    /usr/bin/python3 - "$provider" "$record_dir" <<'PY'
import json
import pathlib
import sys

provider, directory = sys.argv[1:]
root = pathlib.Path(directory)
result = json.loads((root / "result.json").read_text())
assert result["success"] is True, result
assert result["step_results"][0]["output"] == "LEGACY_RESULT_SENTINEL"
records = list(root.glob("attempt-*.json"))
assert len(records) == 1, [p.name for p in records]
args = json.loads(records[0].read_text())["args"]
assert args[:2] == [provider, "--"], args
assert "exec" not in args, args
assert "--output-last-message" not in args, args
assert "-p" in args, args
assert any("TASK_SENTINEL" in arg for arg in args), args
if provider == "claude":
    index = args.index("--system-prompt")
    assert "PERSONA_SENTINEL" in args[index + 1], args
PY
}

run_case codex "" "$fixture/codex-default"
validate_codex_case default "$fixture/codex-default"
run_case codex caller-model "$fixture/codex-explicit-model"
validate_codex_case explicit "$fixture/codex-explicit-model"
run_case claude "" "$fixture/claude"
validate_legacy_case claude "$fixture/claude"
run_case copilot "" "$fixture/copilot"
validate_legacy_case copilot "$fixture/copilot"
printf 'PASS: Codex stdin/final-output/model contract and Claude/Copilot controls\n'
