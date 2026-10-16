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
#   - `\` escaped whatever character followed it, and
#   - `"` closed the JSON string early.
# The substituted values are inside JSON strings, so the fix has to leave a
# credential that JSON-parses back to exactly what the operator typed.
#
# Each case drives the real setup.sh in a sandbox: HOME holds the whole install
# tree (the script writes $HOME/.qwenpaw and $HOME/.qwenpaw.secret) and stubs on
# PATH keep steps 1/2/6 (uv, QwenPaw install, service start) off the network.
# The stubbed service step still sleeps a few seconds per case.
#
# Usage: bash src/os-skills/ai/install-qwenpaw/scripts/test-setup-template-substitution.sh
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
for stub in uv qwenpaw; do
  printf '#!/usr/bin/env bash\nexit 0\n' > "$sandbox/bin/$stub"
  chmod +x "$sandbox/bin/$stub"
done

# read_json <file> <dotted path> — the value at that path.
read_json() {
  python3 - "$1" "$2" <<'PY'
import json
import sys

node = json.load(open(sys.argv[1], encoding="utf-8"))
for part in sys.argv[2].split("."):
    node = node[part]
print(node)
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

# check <label> <file under HOME> <dotted path> <expected> [model]
# set API_KEY/CLIENT_ID/CLIENT_SECRET before calling.
check() {
  local label="$1" file="$2" path="$3" expected="$4" model="${5:-}"
  if ! run_setup "$model"; then
    fail "$label: setup.sh must not abort — $(tail -n 3 "$sandbox/output" | tr '\n' ' ')"
    return
  fi
  local got
  got="$(read_json "$sandbox/home/$file" "$path" 2>/dev/null || true)"
  if [ "$got" != "$expected" ]; then
    fail "$label: $path is '$got', expected '$expected'"
    return
  fi
  if grep -q '{DINGTALK_\|{DASHSCOPE_\|{MODEL_NAME' "$sandbox/home/$file"; then
    fail "$label: a placeholder survived in $file"
    return
  fi
  pass "$label: $path keeps its value byte-for-byte"
}

CONFIG=".qwenpaw/config.json"
DASHSCOPE=".qwenpaw.secret/providers/builtin/dashscope.json"
ACTIVE_MODEL=".qwenpaw.secret/providers/active_model.json"

# The control: an ordinary credential behaves exactly as before.
API_KEY='sk-plain' CLIENT_ID='ding-plain' CLIENT_SECRET='plain-secret'
check 'control' "$CONFIG" channels.dingtalk.client_secret 'plain-secret'
check 'control (id)' "$CONFIG" channels.dingtalk.client_id 'ding-plain'

# `&` used to expand to the matched placeholder.
API_KEY='sk-amp' CLIENT_ID='ding-amp' CLIENT_SECRET='ab&cd'
check 'ampersand in the secret' "$CONFIG" channels.dingtalk.client_secret 'ab&cd'

# `|` used to end the s||| expression; sed then aborted the install.
API_KEY='sk-pipe' CLIENT_ID='ding-pipe' CLIENT_SECRET='ab|cd'
check 'pipe in the secret' "$CONFIG" channels.dingtalk.client_secret 'ab|cd'

# `\` used to escape the character after it.
API_KEY='sk-backslash' CLIENT_ID='ding-backslash' CLIENT_SECRET='ab\cd'
check 'backslash in the secret' "$CONFIG" channels.dingtalk.client_secret 'ab\cd'

# `"` used to close the JSON string early, so step 5 rejected the file.
API_KEY='sk-quote' CLIENT_ID='ding-quote' CLIENT_SECRET='ab"cd'
check 'quote in the secret' "$CONFIG" channels.dingtalk.client_secret 'ab"cd'

# A credential combining all three.
API_KEY='sk-mixed' CLIENT_ID='ding-mixed' CLIENT_SECRET='a&b|c\d&'
check 'all three metacharacters' "$CONFIG" channels.dingtalk.client_secret 'a&b|c\d&'

# The client id, the API key and the model name are separate expressions.
API_KEY='sk-id' CLIENT_ID='id&x' CLIENT_SECRET='plain-secret'
check 'ampersand in the client id' "$CONFIG" channels.dingtalk.client_id 'id&x'

API_KEY='sk-a&b|c' CLIENT_ID='ding-key' CLIENT_SECRET='plain-secret'
check 'metacharacters in the API key' "$DASHSCOPE" api_key 'sk-a&b|c'

API_KEY='sk-model' CLIENT_ID='ding-model' CLIENT_SECRET='plain-secret'
check 'metacharacters in the model name' "$ACTIVE_MODEL" model 'qwen&3|x' 'qwen&3|x'

echo
if [ "$failures" -ne 0 ]; then
  echo "$failures case(s) failed" >&2
  exit 1
fi
echo "all cases passed"
