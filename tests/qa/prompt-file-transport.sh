#!/usr/bin/env bash
# Exercise the over-32KiB agent prompt transport (#159) through the built CLI.
# Unix QA only; requires bash, realpath, and Python 3 at /usr/bin/python3.
set -euo pipefail
: "${RECIPE_RUNNER_QA_BINARY:?set RECIPE_RUNNER_QA_BINARY to the candidate recipe-runner-rs binary}"
runner=$(realpath "$RECIPE_RUNNER_QA_BINARY")
fixture=$(mktemp -d "${TMPDIR:-/tmp}/recipe-runner-prompt-file-qa.XXXXXX")
trap 'if [[ ${QA_KEEP_FIXTURE:-0} == 1 ]]; then printf "QA fixture retained: %s\\n" "$fixture" >&2; else rm -rf -- "$fixture"; fi' EXIT
home="$fixture/home"
project="$fixture/project"
mkdir -p "$home" "$project/.claude/agents/qa/core" "$fixture/bin"
cat > "$project/.claude/agents/qa/core/persona.md" <<'PERSONA'
System/persona fixture text: PERSONA_SENTINEL.
PERSONA

# Fake launcher: records argv and snapshots any prompt file it was handed,
# while the step is still running (the runner removes it afterwards).
cat > "$fixture/bin/launcher" <<'LAUNCHER'
#!/usr/bin/python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ["QA_RECORD_DIR"])
root.mkdir(parents=True, exist_ok=True)
args = sys.argv[1:]
snapshots = {}
for flag in ("--append-system-prompt-file", "--append-system-prompt", "--add-dir"):
    if flag in args:
        value = args[args.index(flag) + 1]
        path = pathlib.Path(value)
        if flag == "--add-dir":
            path = path / "agent-system-prompt.md"
        snapshots[flag] = {
            "value_len": len(value),
            "path": str(path) if len(value) < 4096 else None,
            "is_file": len(value) < 4096 and path.is_file(),
            "content": path.read_text() if len(value) < 4096 and path.is_file() else None,
        }
(root / "record.json").write_text(json.dumps({"args": args, "snapshots": snapshots}))
print("FAKE_AGENT_OK", flush=True)
LAUNCHER
chmod 755 "$fixture/bin/launcher"

# Prompt: > 32 KiB, with a sentinel only in the part past the 8 KiB inline head.
/usr/bin/python3 - "$project/recipe.yaml" <<'PY'
import sys
filler = "\n".join(f"      filler line {i:05d} " + "x" * 60 for i in range(600))
recipe = f"""name: prompt-file-transport
version: "1.0"
context: {{}}
steps:
  - id: big-agent
    type: agent
    agent: "qa:core:persona"
    prompt: |
      HEAD_SENTINEL: start of the task.
{filler}
      OVERFLOW_TAIL_SENTINEL_159: the end of the task.
    timeout: 30
    auto_stage: false
    output: response
"""
open(sys.argv[1], "w").write(recipe)
PY

run_case() {
    local provider=$1 record_dir=$2
    mkdir -p "$record_dir"
    set +e
    (cd "$project" && env -i \
        HOME="$home" PATH=/usr/bin:/bin TMPDIR="$fixture" \
        RECIPE_RUNNER_NO_UPDATE_CHECK=1 \
        AMPLIHACK_LAUNCHER_BINARY="$fixture/bin/launcher" \
        QA_RECORD_DIR="$record_dir" \
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

validate() {
    local provider=$1 record_dir=$2
    /usr/bin/python3 - "$provider" "$record_dir" <<'PY'
import json, pathlib, sys
provider, directory = sys.argv[1:]
root = pathlib.Path(directory)
result = json.loads((root / "result.json").read_text())
assert result["success"] is True, result
assert result["step_results"][0]["output"] == "FAKE_AGENT_OK", result
rec = json.loads((root / "record.json").read_text())
args, snaps = rec["args"], rec["snapshots"]
p_value = args[args.index("-p") + 1]
assert "HEAD_SENTINEL" in p_value
assert "OVERFLOW_TAIL_SENTINEL_159" not in p_value, "tail must not be inline"
assert len(p_value.encode()) < 32 * 1024, len(p_value)
assert "--system-prompt" not in args, args[:6]
if provider == "claude":
    assert "--append-system-prompt" not in args, "text flag must never carry the path (#159)"
    snap = snaps["--append-system-prompt-file"]
    assert snap["is_file"], snap
    content = snap["content"]
    assert content.startswith("System/persona fixture text: PERSONA_SENTINEL."), content[:200]
    assert "Do not invoke /dev" in content           # RECIPE_CHILD_NO_REENTRY guard
    assert "# Continued task instructions" in content
    assert "OVERFLOW_TAIL_SENTINEL_159: the end of the task." in content, content[-200:]
    assert content.index("# Continued task instructions") < content.index("OVERFLOW_TAIL_SENTINEL_159")
    print(f"claude: --append-system-prompt-file {snap['path']} ({len(content)} bytes, tail + no-reentry guard present)")
else:
    assert "--append-system-prompt-file" not in args and "--append-system-prompt" not in args
    snap = snaps["--add-dir"]
    assert snap["is_file"], snap
    assert snap["path"] in p_value, "inline note must name the prompt file path"
    assert "the appended system prompt" not in p_value
    assert "OVERFLOW_TAIL_SENTINEL_159" in snap["content"]
    note = p_value[p_value.index("IMPORTANT:"):]
    print(f"{provider}: --add-dir; inline note -> {note[:160]}...")
PY
}

run_case claude "$fixture/claude"
validate claude "$fixture/claude"
run_case copilot "$fixture/copilot"
validate copilot "$fixture/copilot"
printf 'PASS: over-32KiB prompt reaches claude via --append-system-prompt-file; copilot note names the file\n'
