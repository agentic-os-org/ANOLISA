#!/usr/bin/env bash
# Regression tests for the template substitution in setup.sh.
#
# The substitution used to hand each credential to `sed` as part of the
# expression, so a credential containing a sed metacharacter was either
# corrupted or aborted the whole install:
#
#   - `&` expands to the matched placeholder, so the secret written to
#     config.json became the literal text "{DINGTALK_CLIENT_SECRET}" — and step
#     5 still reported the file as valid JSON, so nothing downstream noticed;
#   - `|` ends the s||| expression, so sed exited 1 and `set -e` killed the
#     install before any later step ran;
#   - `\` escaped whatever character followed it, and `"` closed the JSON string
#     early.
#
# Two further shapes were silent (review round 1): a trailing newline was
# stripped by the command substitution and written as a different value, and two
# chained `-e` expressions re-expanded one field's value as the other's
# placeholder. Values are now rendered by python3 with `json.dumps`, and the
# cases below pin all of it.
#
# Each case drives the real setup.sh in a sandbox: HOME holds the whole install
# tree (it writes $HOME/.qwenpaw and $HOME/.qwenpaw.secret) and every command the
# install steps reach for is stubbed in $sandbox/bin. The stubs are load-bearing,
# not decoration: step 6 runs `kill $(pgrep -f "qwenpaw app")`, and `pgrep -f`
# matches a whole command line — without these stubs the test would signal real
# host processes (a shell whose command line merely mentions the string).
#
# Usage: bash src/os-skills/ai/install-qwenpaw/scripts/test_setup_template_substitution.sh
#
# The name follows the discovery rule in src/os-skills/tests/run-skill-regressions.sh
# (`test_*.sh`); a dashed name would be silently skipped by the CI entry.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP="$SCRIPT_DIR/setup.sh"
failures=0

fail() {
  echo "FAIL: $1" >&2
  failures=$((failures + 1))
}

pass() { echo "PASS: $1"; }

sandbox="$(mktemp -d "${TMPDIR:-/tmp}/qwenpaw-setup-substitution.XXXXXX")"
trap 'rm -rf "$sandbox"' EXIT
mkdir -p "$sandbox/bin" "$sandbox/home"

stub() {  # <name> <body>
  printf '#!/usr/bin/env bash\n%s\n' "$2" >"$sandbox/bin/$1"
  chmod +x "$sandbox/bin/$1"
}

# Steps 1/2 install uv and QwenPaw when they are missing.
stub uv 'exit 0'
stub qwenpaw 'exit 0'
# Step 6 must not see the host process table. `pgrep` is the gate: it prints
# nothing and reports "no match", so setup.sh's kill branch is not entered — and
# even if it were, the builtin `kill` would receive no pid. (A `kill` stub would
# be dead code: `kill` is a shell builtin, and builtins win over PATH.)
stub pgrep 'exit 1'
stub nohup 'exit 0'
# Step 6 also polls the service; 200 keeps the deterministic branch.
stub curl 'echo 200'
# The real script sleeps between service steps; the stub keeps the suite fast.
stub sleep 'exit 0'

# json_text <value> — the value as it appears inside a JSON document, so control
# characters survive the shell comparison instead of being folded by $( ).
json_text() {
  python3 -c 'import json, sys; print(json.dumps(sys.argv[1], ensure_ascii=False))' "$1"
}

# read_json <file> <dotted path> — the value at that path, as JSON text.
read_json() {
  python3 - "$1" "$2" <<'PY'
import json
import sys

node = json.load(open(sys.argv[1], encoding="utf-8"))
for part in sys.argv[2].split("."):
    node = node[part]
print(json.dumps(node, ensure_ascii=False))
PY
}

# run_setup [model] — the real script, sandboxed, with the credentials in the
# file-scope variables below.
run_setup() {
  local args=("$API_KEY" "$CLIENT_ID" "$CLIENT_SECRET")
  [ -n "${1:-}" ] && args+=("$1")
  env HOME="$sandbox/home" PATH="$sandbox/bin:$PATH" \
    bash "$SETUP" "${args[@]}" >"$sandbox/output" 2>&1
}

# check <label> <file under HOME> <dotted path> <expected JSON text> [model]
# set API_KEY/CLIENT_ID/CLIENT_SECRET before calling.
check() {
  local label="$1" file="$2" path="$3" expected="$4" model="${5:-}"
  if ! run_setup "$model"; then
    fail "$label: setup.sh must not abort — $(tail -n 3 "$sandbox/output" | tr '\n' ' ')"
    return
  fi
  local got
  got="$(read_json "$sandbox/home/$file" "$path" 2>/dev/null || true)"
  if [ "$got" = "$expected" ]; then
    pass "$label: $path keeps its value"
  else
    fail "$label: $path is ${got:-<unreadable>}, expected $expected"
  fi
}

CONFIG=".qwenpaw/config.json"
DASHSCOPE=".qwenpaw.secret/providers/builtin/dashscope.json"
ACTIVE_MODEL=".qwenpaw.secret/providers/active_model.json"

# The control: an ordinary credential behaves exactly as before.
API_KEY='sk-plain' CLIENT_ID='ding-plain' CLIENT_SECRET='plain-secret'
check 'control' "$CONFIG" channels.dingtalk.client_secret "$(json_text 'plain-secret')"
check 'control (id)' "$CONFIG" channels.dingtalk.client_id "$(json_text 'ding-plain')"

# `&` used to expand to the matched placeholder.
API_KEY='sk-amp' CLIENT_ID='ding-amp' CLIENT_SECRET='ab&cd'
check 'ampersand in the secret' "$CONFIG" channels.dingtalk.client_secret "$(json_text 'ab&cd')"

# `|` used to end the s||| expression; sed then aborted the install.
API_KEY='sk-pipe' CLIENT_ID='ding-pipe' CLIENT_SECRET='ab|cd'
check 'pipe in the secret' "$CONFIG" channels.dingtalk.client_secret "$(json_text 'ab|cd')"

# `\` used to escape the character after it.
API_KEY='sk-backslash' CLIENT_ID='ding-backslash' CLIENT_SECRET='ab\cd'
check 'backslash in the secret' "$CONFIG" channels.dingtalk.client_secret "$(json_text 'ab\cd')"

# `"` used to close the JSON string early, so step 5 rejected the file.
API_KEY='sk-quote' CLIENT_ID='ding-quote' CLIENT_SECRET='ab"cd'
check 'quote in the secret' "$CONFIG" channels.dingtalk.client_secret "$(json_text 'ab"cd')"

# A credential combining all four.
API_KEY='sk-mixed' CLIENT_ID='ding-mixed' CLIENT_SECRET='a&b|c\d"&'
check 'all four metacharacters' "$CONFIG" channels.dingtalk.client_secret "$(json_text 'a&b|c\d"&')"

# Control characters: a trailing newline used to be stripped by the command
# substitution and written as a *different* value that still passed step 5.
API_KEY='sk-newline' CLIENT_ID='ding-newline' CLIENT_SECRET='trailing-newline'$'\n'
check 'trailing newline in the secret' "$CONFIG" channels.dingtalk.client_secret \
  "$(json_text 'trailing-newline'$'\n')"

API_KEY='sk-tab' CLIENT_ID='ding-tab' CLIENT_SECRET='tab'$'\t''separated'
check 'tab in the secret' "$CONFIG" channels.dingtalk.client_secret "$(json_text 'tab'$'\t''separated')"

API_KEY='sk-crlf' CLIENT_ID='ding-crlf' CLIENT_SECRET='cr'$'\r''lf'
check 'carriage return in the secret' "$CONFIG" channels.dingtalk.client_secret \
  "$(json_text 'cr'$'\r''lf')"

# One field's value must not be read as the other field's placeholder: the two
# `-e` expressions used to run in sequence over the same document.
API_KEY='sk-second' CLIENT_ID='{DINGTALK_CLIENT_SECRET}' CLIENT_SECRET='s3cret'
check 'placeholder text as the client id stays literal' "$CONFIG" channels.dingtalk.client_id \
  "$(json_text '{DINGTALK_CLIENT_SECRET}')"
check 'the secret beside it is untouched' "$CONFIG" channels.dingtalk.client_secret "$(json_text 's3cret')"

# The client id, the API key and the model name are separate render calls.
API_KEY='sk-id' CLIENT_ID='id&x' CLIENT_SECRET='plain-secret'
check 'ampersand in the client id' "$CONFIG" channels.dingtalk.client_id "$(json_text 'id&x')"

API_KEY='sk-a&b|c' CLIENT_ID='ding-key' CLIENT_SECRET='plain-secret'
check 'metacharacters in the API key' "$DASHSCOPE" api_key "$(json_text 'sk-a&b|c')"

API_KEY='sk-model' CLIENT_ID='ding-model' CLIENT_SECRET='plain-secret'
check 'metacharacters in the model name' "$ACTIVE_MODEL" model "$(json_text 'qwen&3|x')" 'qwen&3|x'

echo
if [ "$failures" -ne 0 ]; then
  echo "$failures case(s) failed" >&2
  exit 1
fi
echo "all cases passed"
