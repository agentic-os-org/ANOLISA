#!/usr/bin/env bash
set -euo pipefail

readonly CODEX_PACKAGE="@agentclientprotocol/codex-acp"
readonly CODEX_VERSION="1.6.2"
readonly CLAUDE_PACKAGE="@agentclientprotocol/claude-agent-acp"
readonly CLAUDE_VERSION="0.66.0"

usage() {
  cat >&2 <<'USAGE'
usage:
  run-acp-conformance.sh fake --gateway ABSOLUTE_BINARY --workspace ABSOLUTE_DIRECTORY
  run-acp-conformance.sh real --gateway ABSOLUTE_BINARY --workspace ABSOLUTE_DIRECTORY \
    --profile codex|claude-code --adapter ABSOLUTE_BINARY --acknowledge-provider-run

The real run reads exactly one prompt from stdin. It validates JSONL in memory
and emits only event counts; prompts and Agent text are never written to an
evidence file or echoed by this harness.
USAGE
}

[[ $# -ge 1 ]] || { usage; exit 2; }
mode="$1"
shift

gateway=""
workspace=""
profile=""
adapter=""
acknowledge_provider_run=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --gateway)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      gateway="$2"
      shift 2
      ;;
    --workspace)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      workspace="$2"
      shift 2
      ;;
    --profile)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      profile="$2"
      shift 2
      ;;
    --adapter)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      adapter="$2"
      shift 2
      ;;
    --acknowledge-provider-run)
      acknowledge_provider_run=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done

[[ "$mode" == fake || "$mode" == real ]] || { usage; exit 2; }
[[ "$gateway" = /* && -f "$gateway" && -x "$gateway" ]] || {
  echo "--gateway must be an absolute executable file" >&2
  exit 2
}
[[ "$workspace" = /* && -d "$workspace" ]] || {
  echo "--workspace must be an absolute existing directory" >&2
  exit 2
}
command -v python3 >/dev/null 2>&1 || { echo "python3 is required" >&2; exit 1; }

validate_events() {
  local scenario="$1"
  python3 -c '
import json
import sys

scenario = sys.argv[1]
events = []

def terminal_safe(value):
    normalized = " ".join(value.split())[:240]
    return "".join(
        character if character.isprintable() else f"\\u{ord(character):04x}"
        for character in normalized
    )

for line in sys.stdin:
    record = json.loads(line)
    event = record.get("event")
    if not isinstance(event, str):
        raise SystemExit("COSH emitted a JSONL record without an event")
    if event == "error":
        code = record.get("code", "unknown_error")
        message = record.get("message", "no diagnostic available")
        if not isinstance(code, str):
            code = "unknown_error"
        if not isinstance(message, str):
            message = "no diagnostic available"
        raise SystemExit(
            f"COSH error [{terminal_safe(code)}]: {terminal_safe(message)}"
        )
    events.append(event)

required = {
    "doctor": ["initialized", "session_opened", "terminal", "doctor_ok"],
    "run": ["initialized", "session_opened", "session_update",
            "prompt_finished", "terminal"],
}[scenario]
cursor = 0
for expected in required:
    try:
        cursor = events.index(expected, cursor) + 1
    except ValueError:
        raise SystemExit(f"missing ordered event {expected}") from None
if events.count("terminal") != 1:
    raise SystemExit("expected exactly one terminal event")
if scenario == "run" and events.count("session_update") < 2:
    raise SystemExit("expected at least two streamed text updates")

counts = {name: events.count(name) for name in required}
print(json.dumps({"scenario": scenario, "status": "pass", "events": counts},
                 sort_keys=True, separators=(",", ":")))
' "$scenario"
}

validate_failure_events() {
  local scenario="$1"
  local expected_code="$2"
  python3 -c '
import json
import sys

scenario = sys.argv[1]
expected_code = int(sys.argv[2])
events = []
request_failed_codes = []
for line in sys.stdin:
    record = json.loads(line)
    event = record.get("event")
    if not isinstance(event, str):
        raise SystemExit("COSH emitted a JSONL record without an event")
    events.append(event)
    if event == "request_failed":
        request_failed_codes.append(record.get("code"))

if "prompt_finished" in events:
    raise SystemExit(f"{scenario} emitted false-success prompt_finished")
if request_failed_codes != [expected_code]:
    raise SystemExit(
        f"{scenario} expected request_failed code {expected_code}, "
        f"got {request_failed_codes}"
    )
if events.count("error") != 1:
    raise SystemExit(f"{scenario} expected exactly one terminal error event")
print(json.dumps({"scenario": scenario, "status": "pass", "non_success": True},
                 sort_keys=True, separators=(",", ":")))
' "$scenario" "$expected_code"
}

run_doctor() {
  local selected_profile="$1"
  local selected_adapter="$2"
  "$gateway" doctor \
    --profile "$selected_profile" \
    --adapter "$selected_adapter" \
    --workspace "$workspace" \
    --output jsonl 2>/dev/null | validate_events doctor
}

run_prompt() {
  local selected_profile="$1"
  local selected_adapter="$2"
  local scenario="${3:-real}"
  COSH_ACP_FAKE_SCENARIO="$scenario" "$gateway" run \
    --profile "$selected_profile" \
    --adapter "$selected_adapter" \
    --workspace "$workspace" \
    --output jsonl 2>/dev/null | validate_events run
}

run_prompt_failure() {
  local selected_profile="$1"
  local selected_adapter="$2"
  local scenario="$3"
  local expected_code="$4"
  local output status
  set +e
  output=$(COSH_ACP_FAKE_SCENARIO="$scenario" "$gateway" run \
    --profile "$selected_profile" \
    --adapter "$selected_adapter" \
    --workspace "$workspace" \
    --output jsonl 2>/dev/null)
  status=$?
  set -e
  [[ "$status" != 0 ]] || {
    echo "$scenario unexpectedly exited successfully" >&2
    exit 1
  }
  printf '%s\n' "$output" | validate_failure_events "$scenario" "$expected_code"
}

verify_real_adapter() {
  local selected_profile="$1"
  local candidate="$2"
  local package_name expected_version command_name
  case "$selected_profile" in
    codex)
      package_name="$CODEX_PACKAGE"
      expected_version="$CODEX_VERSION"
      command_name="codex-acp"
      ;;
    claude-code)
      package_name="$CLAUDE_PACKAGE"
      expected_version="$CLAUDE_VERSION"
      command_name="claude-agent-acp"
      ;;
    *)
      echo "real mode requires --profile codex or claude-code" >&2
      exit 2
      ;;
  esac
  [[ "$candidate" = /* && "$(basename -- "$candidate")" == "$command_name" ]] || {
    echo "--adapter must be an absolute profile-matching executable" >&2
    exit 2
  }
  [[ -x "$candidate" ]] || { echo "adapter is not executable" >&2; exit 2; }

  node - "$candidate" "$package_name" "$expected_version" "$command_name" <<'NODE'
const fs = require("fs");
const path = require("path");

const [candidate, expectedName, expectedVersion, commandName] = process.argv.slice(2);
const target = fs.realpathSync(candidate);
const marker = `${path.sep}node_modules${path.sep}${expectedName}${path.sep}`;
const markerIndex = target.lastIndexOf(marker);
if (markerIndex < 0) throw new Error("adapter is not from the pinned npm package");
const packageDir = target.slice(0, markerIndex + marker.length - 1);
const manifest = JSON.parse(fs.readFileSync(path.join(packageDir, "package.json"), "utf8"));
const bin = typeof manifest.bin === "string" ? manifest.bin : manifest.bin?.[commandName];
if (manifest.name !== expectedName || manifest.version !== expectedVersion ||
    typeof bin !== "string" || fs.realpathSync(path.join(packageDir, bin)) !== target) {
  throw new Error("adapter package provenance mismatch");
}
NODE
}

if [[ "$mode" == fake ]]; then
  [[ -z "$profile" && -z "$adapter" && "$acknowledge_provider_run" == false ]] || {
    echo "fake mode does not accept real-adapter options" >&2
    exit 2
  }
  temp_root=$(mktemp -d)
  trap 'rm -rf -- "$temp_root"' EXIT
  fake_adapter="$temp_root/codex-acp"
  python_path=$(command -v python3)
  write_fake_adapter() {
    local scenario="$1"
    printf '#!%s\n' "$python_path" >"$fake_adapter"
    printf 'scenario = "%s"\n' "$scenario" >>"$fake_adapter"
    cat >>"$fake_adapter" <<'PY'
import json
import sys

session_id = "cosh-conformance-session"
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    request_id = request.get("id")
    if method == "initialize":
        result = {
            "protocolVersion": 1,
            "agentCapabilities": {},
            "agentInfo": {
                "name": "@agentclientprotocol/codex-acp",
                "title": "Codex",
                "version": "1.6.2",
            },
            "_meta": {"jetbrains": {"air": {
                "version": 1,
                "capabilities": ["sessionFailure", "agentFileChangeReport"],
            }}},
        }
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
    elif method == "session/new":
        print(json.dumps({"jsonrpc": "2.0", "id": request_id,
                          "result": {"sessionId": session_id}}), flush=True)
    elif method == "session/prompt":
        content = request.get("params", {}).get("prompt", [])
        if len(content) != 1 or content[0].get("type") != "text" or not content[0].get("text"):
            raise SystemExit("expected one non-empty text prompt")
        texts = ("fake-first", "fake-second") if scenario == "warning_success" else ("partial",)
        for text in texts:
            update = {"sessionUpdate": "agent_message_chunk",
                      "content": {"type": "text", "text": text}}
            print(json.dumps({"jsonrpc": "2.0", "method": "session/update",
                              "params": {"sessionId": session_id, "update": update}}), flush=True)
        if scenario == "warning_success":
            warning = {
                "sessionUpdate": "session_info_update",
                "_meta": {"jetbrains": {"air": {"version": 1, "sessionFailure": {
                    "id": "turn-1:error", "revision": 1, "category": "connection",
                    "severity": "warning", "title": "Retrying connection", "actions": [],
                }}}},
            }
            print(json.dumps({"jsonrpc": "2.0", "method": "session/update",
                              "params": {"sessionId": session_id, "update": warning}}), flush=True)
            response = {"jsonrpc": "2.0", "id": request_id,
                        "result": {"stopReason": "end_turn"}}
        elif scenario == "typed_error":
            failure = {
                "id": "turn-1:error", "revision": 1, "category": "service",
                "severity": "error", "title": "Provider failed", "actions": ["retry"],
            }
            response = {"jsonrpc": "2.0", "id": request_id, "result": {
                "stopReason": "end_turn", "_meta": {"jetbrains": {"air": {
                    "version": 1, "sessionFailure": failure,
                }}},
            }}
        elif scenario == "rpc_error":
            response = {"jsonrpc": "2.0", "id": request_id,
                        "error": {"code": -32603, "message": "internal error"}}
        else:
            raise SystemExit(f"unknown fake scenario: {scenario}")
        print(json.dumps(response), flush=True)
    else:
        raise SystemExit(f"unexpected ACP method: {method}")
PY
    chmod 0700 "$fake_adapter"
  }

  write_fake_adapter warning_success
  run_doctor codex "$fake_adapter"
  printf '%s\n' "deterministic fake prompt" | run_prompt codex "$fake_adapter" warning_success
  write_fake_adapter typed_error
  printf '%s\n' "deterministic fake prompt" | \
    run_prompt_failure codex "$fake_adapter" typed_error -32000
  write_fake_adapter rpc_error
  printf '%s\n' "deterministic fake prompt" | \
    run_prompt_failure codex "$fake_adapter" rpc_error -32603
  printf '%s\n' \
    '{"profile":"fake","status":"pass","raw_output_persisted":false,"failure_cases":2}'
  exit 0
fi

[[ "$acknowledge_provider_run" == true ]] || {
  echo "real mode requires --acknowledge-provider-run" >&2
  exit 2
}
[[ ! -t 0 ]] || {
  echo "pipe one explicit prompt to stdin; interactive prompt capture is disabled" >&2
  exit 2
}
command -v node >/dev/null 2>&1 || { echo "node is required" >&2; exit 1; }
verify_real_adapter "$profile" "$adapter"
run_doctor "$profile" "$adapter"
run_prompt "$profile" "$adapter"
printf '%s\n' \
  "{\"profile\":\"$profile\",\"status\":\"pass\",\"raw_output_persisted\":false}"
