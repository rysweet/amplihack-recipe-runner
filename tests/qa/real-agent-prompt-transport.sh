#!/usr/bin/env bash
# Outside-in check of agent prompt transport (#205, #211) against the real
# `amplihack claude` and `amplihack copilot` wrappers, not a stub.
#
# Opt-in and not run by CI: it starts authenticated agent sessions and uses
# model tokens. Linux only (it samples /proc/<pid>/cmdline).
#
# Requires RECIPE_RUNNER_QA_BINARY, an authenticated `amplihack` on PATH (or
# QA_AMPLIHACK_LAUNCHER), and Python 3 at /usr/bin/python3.
# QA_REAL_PROVIDERS selects the providers (default: "claude copilot").
#
# For each provider it checks that:
# - the step succeeds and the reply holds a random task token that exists only
#   in the context file, so the rendered task reached the agent;
# - the reply holds a random persona token that exists only in the agent
#   definition, so the system prompt reached the agent;
# - no process's command line held either token or the persona text while the
#   step ran;
# - the private prompt file named on the wrapper's command line, under the
#   run's own TMPDIR, is gone after the step.
# For Copilot the first point also shows the model read the file it was asked
# to read: the task is not delivered any other way.
set -euo pipefail
: "${RECIPE_RUNNER_QA_BINARY:?set RECIPE_RUNNER_QA_BINARY to the candidate recipe-runner-rs binary}"
[[ -d /proc/self ]] || { echo "SKIP: needs Linux /proc" >&2; exit 2; }
runner=$(realpath "$RECIPE_RUNNER_QA_BINARY")
launcher=${QA_AMPLIHACK_LAUNCHER:-$(command -v amplihack || true)}
[[ -n "$launcher" && -x "$launcher" ]] || { echo "amplihack launcher not found" >&2; exit 2; }
providers=${QA_REAL_PROVIDERS:-claude copilot}
fixture=$(mktemp -d "${TMPDIR:-/tmp}/recipe-runner-real-agent-qa.XXXXXX")
trap 'if [[ ${QA_KEEP_FIXTURE:-0} == 1 ]]; then printf "QA fixture retained: %s\\n" "$fixture" >&2; else rm -rf -- "$fixture"; fi' EXIT

# Tokens are produced and written by builtins only, so they never sit on any
# command line, including this script's own children.
token() { local hex; hex=$(od -An -N8 -tx1 /dev/urandom); hex=${hex//[[:space:]]/}; printf '%s%s' "$1" "${hex^^}"; }
task_token=$(token QATASK)
persona_token=$(token QAPERSONA)
persona_text="QA persona marker paragraph"
export QA_TASK_TOKEN=$task_token QA_PERSONA_TOKEN=$persona_token QA_PERSONA_TEXT=$persona_text

project="$fixture/project"
mkdir -p "$project/.claude/agents/qa/core"
printf '%s\n' \
    "$persona_text. You are a QA probe for a command-line tool." \
    "Whatever the user asks, end your reply with a final line containing exactly: $persona_token" \
    > "$project/.claude/agents/qa/core/persona.md"
printf '{"task_token": "%s"}\n' "$task_token" > "$fixture/context.json"
cat > "$project/recipe.yaml" <<'YAML'
name: real-agent-prompt-transport
version: "1.0"
context: {}
steps:
  - id: probe
    type: agent
    agent: "qa:core:persona"
    prompt: |
      Reply with one line containing exactly {{task_token}} and nothing else.
      Do not run commands and do not edit files.
    timeout: 300
    auto_stage: false
    output: response
YAML

# Samples every process's command line every 20 ms until told to stop.
cat > "$fixture/sampler.py" <<'PY'
import json, os, pathlib, sys, time
out, stop = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
markers = [os.environ[k] for k in ("QA_TASK_TOKEN", "QA_PERSONA_TOKEN", "QA_PERSONA_TEXT")]
hits, prompt_paths, samples = [], set(), 0
while not stop.exists():
    samples += 1
    for entry in os.scandir("/proc"):
        if not entry.name.isdigit():
            continue
        try:
            args = (pathlib.Path(entry.path) / "cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        text = [a.decode("utf-8", "replace") for a in args]
        for marker in markers:
            if any(marker in a for a in text):
                hits.append({"pid": entry.name, "marker": marker[:12], "argv0": text[0][:80]})
        for a in text:
            for word in a.split():
                if word.endswith("agent-system-prompt.md"):
                    prompt_paths.add(word)
    time.sleep(0.02)
out.write_text(json.dumps({"hits": hits, "prompt_paths": sorted(prompt_paths), "samples": samples}))
PY

failed=0
for provider in $providers; do
    dir="$fixture/$provider"
    # A private TMPDIR, so only this run's prompt files are checked: other
    # runners on the host can have their own prompt files open.
    mkdir -p "$dir/tmp"
    /usr/bin/python3 -I "$fixture/sampler.py" "$dir/argv.json" "$dir/stop" &
    sampler=$!
    set +e
    (cd "$project" && TMPDIR="$dir/tmp" AMPLIHACK_LAUNCHER_BINARY="$launcher" \
        "$runner" recipe.yaml --agent-binary "$provider" \
            --context-file "$fixture/context.json" \
            --working-dir "$project" --no-auto-stage --no-update-check \
            --output-format json) > "$dir/result.json" 2> "$dir/stderr.log"
    status=$?
    set -e
    : > "$dir/stop"
    wait "$sampler"
    if /usr/bin/python3 -I - "$provider" "$dir" "$status" <<'PY'
import json, os, pathlib, sys
provider, directory, status = sys.argv[1], pathlib.Path(sys.argv[2]), int(sys.argv[3])
task, persona = os.environ["QA_TASK_TOKEN"], os.environ["QA_PERSONA_TOKEN"]
problems = []
try:
    result = json.loads((directory / "result.json").read_text())
except ValueError:
    result = {}
output = (result.get("step_results") or [{}])[0].get("output") or ""
if status != 0 or result.get("success") is not True:
    problems.append(f"step failed (exit {status})")
if task not in output:
    problems.append("task token missing from reply: task not delivered")
if persona not in output:
    problems.append("persona token missing from reply: system prompt not delivered")
argv = json.loads((directory / "argv.json").read_text())
own = str(directory / "tmp") + "/"
argv["prompt_paths"] = [p for p in argv["prompt_paths"] if p.startswith(own)]
if argv["hits"]:
    problems.append(f"private text on a command line: {argv['hits'][:3]}")
if not argv["prompt_paths"]:
    problems.append("no wrapper command line named the private prompt file")
for path in argv["prompt_paths"]:
    if pathlib.Path(path).exists():
        problems.append(f"private prompt file left behind: {path}")
summary = f"{argv['samples']} argv samples, prompt files {argv['prompt_paths']}"
if problems:
    print(f"FAIL {provider}: {'; '.join(problems)} ({summary})")
    print(f"  reply: {output[-400:]!r}")
    print("  stderr tail: " + (directory / "stderr.log").read_text()[-1500:])
    sys.exit(1)
print(f"PASS {provider}: task and persona tokens in reply, none on argv, prompt file removed ({summary})")
PY
    then :; else failed=1; fi
done
exit "$failed"
