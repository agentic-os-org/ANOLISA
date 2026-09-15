#!/usr/bin/env bash
# Full TOON functional verification.
# Covers three application scenarios: Tokenless CLI, Cosh-NG, OpenClaw.
#
# Run by hand (this is not a make target):
#
#   PATH="src/tokenless/target/debug:$PATH" bash src/tokenless/tests/test-toon-full.sh
#
# Prerequisites:
#   required    tokenless on PATH built from this checkout (set
#               TOKENLESS_ALLOW_VERSION_SKEW=1 to accept an installed one), jq, python3
#   scenario 2  common hooks directory, taken from the repo tree by default and
#               overridable with TOKENLESS_HOOK_DIR
#   scenario 3  OpenClaw with the tokenless plugin installed and enabled, GNU timeout
#               (coreutils), and an explicit TOKENLESS_TOON_FULL_LIVE=1
#               (3.x makes real model calls: slow and quota-consuming, off by default).
#               The OpenClaw state directory resolves as OPENCLAW_STATE_DIR ->
#               OPENCLAW_HOME -> ~/.openclaw, the same way the adapter install/detect
#               scripts do it, and OPENCLAW_BIN overrides the CLI path. With live mode
#               on, a missing scenario 3 prerequisite aborts the run instead of
#               reporting a green suite that validated nothing.
#
# A missing optional prerequisite is recorded as SKIP rather than FAIL: this script has
# to reach a trustworthy verdict on dev machines, CI containers and provisioned hosts
# alike. A permanently red case only gets ignored, and it rots together with the
# contract it was supposed to cover.

set -uo pipefail

TEST_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOKENLESS_SOURCE_DIR="$(cd "$TEST_SCRIPT_DIR/.." && pwd)"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

PASS=0
FAIL=0
SKIP=0
TOTAL=0
SCENARIOS=0

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

pass() { echo -e "${GREEN}[PASS]${NC} $1"; ((PASS++)); ((TOTAL++)); }
fail() { echo -e "${RED}[FAIL]${NC} $1"; ((FAIL++)); ((TOTAL++)); }
skip() { echo -e "${YELLOW}[SKIP]${NC} $1"; ((SKIP++)); }
info() { echo -e "${BLUE}[INFO]${NC} $1"; }
section() { echo -e "\n${YELLOW}========== $1 ==========${NC}\n"; ((SCENARIOS++)); }
scenario() { echo -e "\n${CYAN}▸ $1${NC}"; }

assert_contains() {
    local input="$1" expected="$2" test_name="$3"
    if echo "$input" | grep -qF "$expected"; then pass "$test_name"
    else fail "$test_name - expected to contain: '$expected'"; fi
}

assert_not_empty() {
    local input="$1" test_name="$2"
    if [ -n "$input" ]; then pass "$test_name"
    else fail "$test_name - empty output"; fi
}

assert_same() {
    local actual="$1" expected="$2" test_name="$3"
    if [ "$actual" = "$expected" ]; then pass "$test_name"
    else fail "$test_name - expected '$expected', got '$actual'"; fi
}

# Compare a compress-toon run against its input byte by byte.
#
# Command substitution is not usable here: $(...) strips trailing newlines before the
# assignment, so "always append a LF" and "drop the input LF" regressions both survive
# a string comparison. Write the payload to a file, redirect stdout to a second file
# and let cmp look at the bytes.
#   $1 = test label, $2 = input file, $3 = --min-toon-chars value ("" keeps the default)
assert_passthrough_bytes() {
    local label="$1" input_file="$2" min_chars="$3"
    local output_file="${input_file}.out" rc diff

    # Bash < 4.4 treats an empty "$@" as unbound under `set -u`, so the flags are
    # passed as a single value instead of a variadic tail.
    if [ -n "$min_chars" ]; then
        tokenless compress-toon --min-toon-chars "$min_chars" \
            < "$input_file" > "$output_file" 2>/dev/null
    else
        tokenless compress-toon < "$input_file" > "$output_file" 2>/dev/null
    fi
    rc=$?
    assert_same "$rc" "0" "$label - exit code"
    if diff=$(cmp "$input_file" "$output_file" 2>&1); then
        pass "$label - stdout is byte-identical to stdin"
    else
        fail "$label - stdout differs from stdin: $diff"
    fi
}

# ========== Prerequisites ==========

# This script drives the binary on PATH while its assertions follow the checkout. When
# the two disagree, failures point at the wrong file (0.7.x does not even have
# compress-toon --min-toon-chars), so compare versions up front and name the reason
# instead of dumping unrelated failures for the reader to guess about.
require_matching_cli() {
    local cmd missing=0
    for cmd in tokenless jq python3; do
        if ! command -v "$cmd" >/dev/null 2>&1; then
            echo -e "${RED}ERROR: $cmd is not installed${NC}"
            missing=1
        fi
    done
    [ "$missing" -eq 0 ] || exit 1

    local cli_version workspace_version
    cli_version=$(tokenless --version 2>/dev/null | awk '{print $2}')
    workspace_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' \
        "$TOKENLESS_SOURCE_DIR/Cargo.toml" | head -1)
    if [ -n "$workspace_version" ] && [ "$cli_version" != "$workspace_version" ]; then
        if [ "${TOKENLESS_ALLOW_VERSION_SKEW:-0}" = "1" ]; then
            info "WARNING: tokenless on PATH is ${cli_version:-unknown}, this checkout is $workspace_version (TOKENLESS_ALLOW_VERSION_SKEW=1)"
            return 0
        fi
        echo -e "${RED}ERROR: tokenless on PATH is ${cli_version:-unknown}, but this checkout is $workspace_version${NC}"
        echo "The assertions follow the checkout while the binary under test does not, so a version mismatch reports unrelated failures."
        echo "Put the current build first on PATH (or install it) and rerun:"
        echo ""
        echo "    PATH=\"src/tokenless/target/debug:\$PATH\" bash src/tokenless/tests/test-toon-full.sh"
        echo "    make -C src/tokenless build && make -C src/tokenless install"
        echo ""
        echo "To test the installed ${cli_version:-unknown} anyway, set TOKENLESS_ALLOW_VERSION_SKEW=1."
        exit 1
    fi
}

# Scenario 3 bounds every model call with GNU timeout, which is not installed
# everywhere (plain macOS, minimal containers). Without it the pipeline yields no
# session ID, the surrounding code records SKIPs and the suite still exits 0 - a caller
# who explicitly asked for live validation would get a green run that validated
# nothing. Resolve it here; require_live_prerequisites turns it into a hard error when
# live mode is on.
resolve_timeout_bin() {
    TIMEOUT_BIN=""
    TIMEOUT_DETAIL="GNU timeout (coreutils) not found on PATH"
    if command -v timeout >/dev/null 2>&1; then
        TIMEOUT_BIN="$(command -v timeout)"
        TIMEOUT_DETAIL=""
        return 0
    fi
    return 1
}

# Which hooks copy scenario 2 exercises: the repo tree first (that is the implementation
# under test), then the installed copies. An explicit TOKENLESS_HOOK_DIR is the only
# source consulted - a wrong value has to be reported instead of silently falling back
# to the repo tree, otherwise "I tested the installed copy" becomes a false statement.
resolve_hook_dir() {
    local candidate
    HOOK_DIR=""
    HOOK_DIR_DETAIL=""
    if [ -n "${TOKENLESS_HOOK_DIR:-}" ]; then
        if [ -f "$TOKENLESS_HOOK_DIR/compress_response_hook.py" ]; then
            HOOK_DIR="$TOKENLESS_HOOK_DIR"
            return 0
        fi
        HOOK_DIR_DETAIL="no compress_response_hook.py under TOKENLESS_HOOK_DIR=$TOKENLESS_HOOK_DIR"
        return 1
    fi
    for candidate in \
        "$TOKENLESS_SOURCE_DIR/adapters/tokenless/common/hooks" \
        "/usr/share/anolisa/adapters/tokenless/common/hooks" \
        "$HOME/.local/share/anolisa/adapters/tokenless/common/hooks"; do
        if [ -f "$candidate/compress_response_hook.py" ]; then
            HOOK_DIR="$candidate"
            return 0
        fi
    done
    HOOK_DIR_DETAIL="no compress_response_hook.py in the repo tree or in the installed prefixes"
    return 1
}

OPENCLAW_PLUGIN_ID="tokenless"
OPENCLAW_PROBE_TIMEOUT=60

# Scenario 3 needs an OpenClaw install with the tokenless plugin enabled. The state
# directory is configurable, so resolve it exactly like
# adapters/tokenless/openclaw/scripts/{install,detect}.sh do: OPENCLAW_STATE_DIR, else
# OPENCLAW_HOME, else ~/.openclaw. Probing only $HOME/.openclaw would mark a valid
# custom-prefix install as unready and skip every live check.
resolve_openclaw_layout() {
    local home="${OPENCLAW_HOME:-$HOME/.openclaw}"
    OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-$home}"
    OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"
    [ -n "$OPENCLAW_STATE_DIR" ] || OPENCLAW_STATE_DIR="${home%/}"
    if [ -n "${OPENCLAW_BIN:-}" ]; then
        return 0
    fi
    OPENCLAW_BIN="$(command -v openclaw 2>/dev/null || true)"
    if [ -z "$OPENCLAW_BIN" ] && [ -x "$OPENCLAW_STATE_DIR/bin/openclaw" ]; then
        OPENCLAW_BIN="$OPENCLAW_STATE_DIR/bin/openclaw"
    fi
}

# Read-only openclaw subcommand against the resolved state directory. OPENCLAW_HOME is
# unset for the call, as in detect.sh and install.sh, so a stale value cannot redirect
# the CLI to a different state root. Bounded when GNU timeout is around: a hung gateway
# must not stall the probe for hosts that only want scenarios 1 and 2.
openclaw_cli() {
    if [ -n "$TIMEOUT_BIN" ]; then
        "$TIMEOUT_BIN" "$OPENCLAW_PROBE_TIMEOUT" env -u OPENCLAW_HOME \
            OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" "$@"
    else
        env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" "$@"
    fi
}

# Live model calls can hang for minutes, so scenario 3 bounds each of them.
openclaw_live() {
    local secs="$1"
    shift
    "$TIMEOUT_BIN" "$secs" env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
        "$OPENCLAW_BIN" "$@"
}

probe_openclaw() {
    OPENCLAW_STATUS="absent"
    resolve_openclaw_layout
    if [ -z "${OPENCLAW_BIN:-}" ]; then
        OPENCLAW_DETAIL="openclaw not installed (checked PATH and $OPENCLAW_STATE_DIR/bin)"
        return 1
    fi
    OPENCLAW_STATUS="installed"

    local plugin_dir="$OPENCLAW_STATE_DIR/extensions/$OPENCLAW_PLUGIN_ID"
    local plugin_detail
    if openclaw_cli plugins list --json 2>/dev/null |
        grep -qE "\"id\"[[:space:]]*:[[:space:]]*\"$OPENCLAW_PLUGIN_ID\""; then
        plugin_detail="listed by openclaw plugins list"
    elif [ -d "$plugin_dir" ]; then
        plugin_detail="$plugin_dir"
    else
        OPENCLAW_DETAIL="plugin missing (not listed by openclaw plugins list, no $plugin_dir)"
        return 1
    fi

    local reason
    reason=$(OPENCLAW_CONFIG="$OPENCLAW_STATE_DIR/openclaw.json" \
        OPENCLAW_PLUGIN_ID="$OPENCLAW_PLUGIN_ID" \
        python3 - <<'PYCHECK' 2>/dev/null
import json, os

path = os.environ["OPENCLAW_CONFIG"]
plugin_id = os.environ["OPENCLAW_PLUGIN_ID"]
try:
    with open(path) as handle:
        cfg = json.load(handle)
except OSError as exc:
    print("cannot read %s: %s" % (path, exc))
    raise SystemExit
except ValueError as exc:
    print("%s is not valid JSON: %s" % (path, exc))
    raise SystemExit
entries = cfg.get("plugins", {}).get("entries", {})
entry = entries.get(plugin_id)
if entry is None:
    print("%s has no plugins.entries.%s (present entries: %s)"
          % (path, plugin_id, ", ".join(sorted(entries)) or "none"))
elif not entry.get("enabled"):
    print("plugins.entries.%s.enabled is false" % plugin_id)
elif not entry.get("config", {}).get("post_tool_enabled", True):
    print("plugins.entries.%s.config.post_tool_enabled is false" % plugin_id)
PYCHECK
)
    if [ -n "$reason" ]; then
        OPENCLAW_STATUS="disabled"
        OPENCLAW_DETAIL="$reason"
        return 1
    fi
    OPENCLAW_STATUS="ready"
    OPENCLAW_DETAIL="plugin enabled with PostTool configured ($plugin_detail, state dir $OPENCLAW_STATE_DIR)"
    return 0
}

# TOKENLESS_TOON_FULL_LIVE=1 is an explicit request for the real model calls. Refuse to
# start when a prerequisite for them is missing: skipping scenario 3 and exiting 0 would
# report success for a validation that never ran.
require_live_prerequisites() {
    local missing=0
    if [ -z "$TIMEOUT_BIN" ]; then
        echo -e "${RED}ERROR: $TIMEOUT_DETAIL${NC}"
        echo "Scenario 3 bounds every model call with timeout. Install coreutils, or unset TOKENLESS_TOON_FULL_LIVE to skip the live calls."
        missing=1
    fi
    if [ "$OPENCLAW_STATUS" != "ready" ]; then
        echo -e "${RED}ERROR: OpenClaw is not ready: $OPENCLAW_DETAIL${NC}"
        echo "Install and enable the plugin (src/tokenless/adapters/tokenless/openclaw/scripts/install.sh), or unset TOKENLESS_TOON_FULL_LIVE to skip the live calls."
        missing=1
    fi
    [ "$missing" -eq 0 ] || exit 1
}

require_matching_cli
resolve_timeout_bin || true
resolve_hook_dir || true
probe_openclaw || true
if [ "${TOKENLESS_TOON_FULL_LIVE:-0}" = "1" ]; then
    require_live_prerequisites
fi

# ========== Environment check ==========
section "Environment check"

info "tokenless $(tokenless --version 2>/dev/null | awk '{print $2}') @ $(command -v tokenless)"
pass "tokenless available and built from this checkout"
pass "jq available ($(jq --version 2>/dev/null))"

if [ -n "$HOOK_DIR" ]; then
    pass "common hooks directory available ($HOOK_DIR)"
else
    skip "common hooks directory unavailable: $HOOK_DIR_DETAIL - scenario 2 will be skipped"
fi

if [ -n "$TIMEOUT_BIN" ]; then
    pass "GNU timeout available for the scenario 3 live calls ($TIMEOUT_BIN)"
else
    skip "$TIMEOUT_DETAIL - scenario 3 live calls cannot be bounded"
fi

if [ "$OPENCLAW_STATUS" = "ready" ]; then
    pass "OpenClaw $OPENCLAW_DETAIL"
    if [ "${TOKENLESS_TOON_FULL_LIVE:-0}" = "1" ]; then
        info "TOKENLESS_TOON_FULL_LIVE=1 - scenario 3 will make real model calls"
    else
        skip "TOKENLESS_TOON_FULL_LIVE=1 not set - scenario 3 live model calls will not run"
    fi
else
    skip "OpenClaw not ready: $OPENCLAW_DETAIL - scenario 3 will be skipped"
fi

# ========== Scenario 1: Tokenless CLI ==========
section "Scenario 1: Tokenless CLI"

# compress-toon applies a 500-character gate by default (MIN_TOON_CHARS, the same
# threshold the hook layer uses) and passes shorter payloads through byte for byte; a
# payload whose estimated token count does not drop is passed through even with the gate
# off. The small fixtures below therefore pass --min-toon-chars 0 explicitly so their
# assertions actually reach the encoder. The gate and the passthrough contract itself
# are covered by 1.8.
TOON_FORCE=(--min-toon-chars 0)

scenario "1.1 basic encode/decode"

# Simple object
simple='{"name":"Alice","age":30,"active":true}'
result=$(printf '%s' "$simple" | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
assert_not_empty "$result" "simple object encodes"
assert_contains "$result" "name: Alice" "simple object - name"
assert_contains "$result" "age: 30" "simple object - age"

# Decode round-trip
roundtrip=$(printf '%s' "$result" | tokenless decompress-toon 2>/dev/null)
assert_not_empty "$roundtrip" "simple object decodes"
if printf '%s' "$roundtrip" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d['name']=='Alice' and d['age']==30" 2>/dev/null; then
    pass "round-trip preserves the data"
else
    fail "round-trip does not preserve the data"
fi

scenario "1.2 tabular data compression"

json='{"users":[{"id":1,"name":"Alice","email":"alice@example.com","role":"admin"},{"id":2,"name":"Bob","email":"bob@example.com","role":"user"},{"id":3,"name":"Charlie","email":"charlie@example.com","role":"moderator"},{"id":4,"name":"Diana","email":"diana@example.com","role":"admin"},{"id":5,"name":"Eve","email":"eve@example.com","role":"user"}]}'
toon_out=$(printf '%s' "$json" | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
json_len=${#json}
toon_len=${#toon_out}
savings=$(( (json_len - toon_len) * 100 / json_len ))
info "  JSON: $json_len chars -> TOON: $toon_len chars (${savings}% saved)"
if [ "$savings" -ge 15 ]; then
    pass "tabular data saves >= 15%"
else
    fail "tabular data saves < 15% (${savings}%)"
fi
assert_contains "$toon_out" "users[5]" "tabular array header is correct"

scenario "1.3 deeply nested data"

nested='{"data":{"users":[{"id":1,"profile":{"name":"Alice","age":30,"address":{"city":"Beijing","country":"CN"}}},{"id":2,"profile":{"name":"Bob","age":25,"address":{"city":"Shanghai","country":"CN"}}}],"meta":{"total":2,"page":1,"hasNext":false}}}'
toon_out=$(printf '%s' "$nested" | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
json_len=${#nested}
toon_len=${#toon_out}
info "  JSON: $json_len chars -> TOON: $toon_len chars"
assert_not_empty "$toon_out" "deeply nested data encodes"
if [ "$toon_out" = "$nested" ]; then
    # A non-tabular shape yields no estimated token savings, so the contract passes it
    # through byte for byte. Such output must not be fed to decompress-toon (it only
    # accepts TOON); the passthrough round-trip is covered by 1.7 and 1.8.
    pass "deeply nested data has no token savings -> byte-for-byte passthrough (non-tabular shapes are not guaranteed to shrink)"
else
    nested_rt=$(printf '%s' "$toon_out" | tokenless decompress-toon 2>/dev/null)
    assert_contains "$nested_rt" '"city"' "deeply nested round-trip decodes"
fi

scenario "1.4 large JSON compression"

# Build a larger JSON (an API-response shaped payload) that exceeds the default gate, so
# this case goes through the default arguments.
python3 -c "
import json, sys
data = {
    'results': [{'id': i, 'name': f'Item_{i}', 'value': i * 3.14, 'active': i % 2 == 0, 'tags': [f'tag_{j}' for j in range(5)]} for i in range(50)],
    'meta': {'total': 50, 'page': 1, 'per_page': 50},
    'debug_info': {'query_time': 0.123, 'cache_hit': False}
}
json.dump(data, sys.stdout)
" > "$TMP_DIR/large_test.json"

large_json=$(cat "$TMP_DIR/large_test.json")
large_json_len=${#large_json}
toon_out=$(printf '%s' "$large_json" | tokenless compress-toon 2>/dev/null)
toon_len=${#toon_out}
savings=$(( (large_json_len - toon_len) * 100 / large_json_len ))
info "  JSON: $large_json_len chars -> TOON: $toon_len chars (${savings}% saved)"
if [ "$savings" -ge 10 ]; then
    pass "large JSON saves >= 10%"
else
    fail "large JSON saves < 10% (${savings}%)"
fi
assert_contains "$toon_out" "results[50]" "large JSON tabular header is correct"

scenario "1.5 special value handling"

# Booleans
result=$(printf '%s' '{"t":true,"f":false}' | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
assert_contains "$result" "t: true" "true encodes"
assert_contains "$result" "f: false" "false encodes"

# Null (a single-key sample shows no estimated token savings and would pass through, so
# add a second key to make the encoder run)
result=$(printf '%s' '{"val":null,"keep":1}' | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
assert_contains "$result" "val: null" "null encodes"

# Floats
result=$(printf '%s' '{"pi":3.14159,"neg":-42}' | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
assert_contains "$result" "pi: 3.14159" "float encodes"
assert_contains "$result" "neg: -42" "negative number encodes"

# Empty array
result=$(printf '%s' '{"items":[],"keep":1}' | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
assert_contains "$result" "items[0]" "empty array encodes"

scenario "1.6 file input/output"

echo '{"from":"file","value":42}' > "$TMP_DIR/toon_file_test.json"
result=$(tokenless compress-toon -f "$TMP_DIR/toon_file_test.json" "${TOON_FORCE[@]}" 2>/dev/null)
assert_contains "$result" "from: file" "file input encodes"

tokenless compress-toon -f "$TMP_DIR/toon_file_test.json" "${TOON_FORCE[@]}" > "$TMP_DIR/toon_file_output.toon" 2>/dev/null
result=$(cat "$TMP_DIR/toon_file_output.toon" 2>/dev/null)
assert_contains "$result" "from: file" "file output encodes"

scenario "1.7 round-trip integrity"

if python3 -c "
import json, subprocess, sys

test_cases = [
    {'name': 'Alice', 'age': 30, 'active': True},
    {'users': [{'id': 1, 'name': 'Alice'}, {'id': 2, 'name': 'Bob'}]},
    {'data': {'users': [{'id': 1, 'name': 'test', 'tags': ['a', 'b']}], 'count': 1, 'active': True, 'meta': None}},
    {'a': {'b': {'c': {'d': {'e': 'deep'}}}}},
    # Note: TOON normalizes integer-valued floats (3.0 -> 3), so use 3.14
    {'mixed': [1, 'two', 3.14, True, None, [4, 5]]}
]

all_passed = True
for i, case in enumerate(test_cases):
    original = json.dumps(case, sort_keys=True)
    # Encode. --min-toon-chars 0 keeps the 500-character gate out of the way so
    # every fixture actually reaches the encoder.
    p1 = subprocess.run(['tokenless', 'compress-toon', '--min-toon-chars', '0'],
                        input=original, capture_output=True, text=True)
    toon_out = p1.stdout.strip()
    if p1.returncode != 0 or not toon_out:
        print(f'Case {i} ENCODE FAILED (exit {p1.returncode}): {p1.stderr.strip()}', file=sys.stderr)
        all_passed = False
        continue
    # compress-toon falls back to the original JSON when TOON offers no
    # savings; that passthrough still round-trips by definition.
    try:
        roundtrip = json.loads(toon_out)
    except ValueError:
        p2 = subprocess.run(['tokenless', 'decompress-toon'], input=toon_out, capture_output=True, text=True)
        if p2.returncode != 0 or not p2.stdout.strip():
            print(f'Case {i} DECODE FAILED (exit {p2.returncode}): {p2.stderr.strip()}', file=sys.stderr)
            all_passed = False
            continue
        roundtrip = json.loads(p2.stdout)
    roundtrip_json = json.dumps(roundtrip, sort_keys=True)
    if original != roundtrip_json:
        print(f'Case {i} MISMATCH: {original} vs {roundtrip_json}', file=sys.stderr)
        all_passed = False

sys.exit(0 if all_passed else 1)
" 2>&1; then
    pass "all 5 data shapes round-trip identically"
else
    fail "round-trip produced inconsistent data"
fi

scenario "1.8 default gate and no-savings passthrough contract"

# Under the 500-character default gate compress-toon exits 0 and echoes stdin verbatim -
# no LF appended and none removed. Both newline variants have to hold, and that is
# exactly what a command substitution cannot express, so compare the files.
short='{"name":"Alice","age":30,"active":true}'
printf '%s' "$short" > "$TMP_DIR/gate-short.json"
printf '%s\n' "$short" > "$TMP_DIR/gate-short-newline.json"
assert_passthrough_bytes "short payload (${#short} chars < 500), no trailing newline" \
    "$TMP_DIR/gate-short.json" ""
assert_passthrough_bytes "short payload (${#short} chars < 500), trailing newline" \
    "$TMP_DIR/gate-short-newline.json" ""

# Gate off but no estimated token savings: passthrough as well. TOON indents nested
# objects, so a deep single-value shape costs more tokens than its JSON spelling and is
# left alone regardless of the trailing newline.
no_savings='{"a":{"b":{"c":{"d":{"e":1}}}}}'
printf '%s' "$no_savings" > "$TMP_DIR/gate-no-savings.json"
printf '%s\n' "$no_savings" > "$TMP_DIR/gate-no-savings-newline.json"
assert_passthrough_bytes "no-savings payload with --min-toon-chars 0, no trailing newline" \
    "$TMP_DIR/gate-no-savings.json" "0"
assert_passthrough_bytes "no-savings payload with --min-toon-chars 0, trailing newline" \
    "$TMP_DIR/gate-no-savings-newline.json" "0"

# Over the gate: the default arguments encode (the fixture is the large JSON from 1.4).
long_json=$(cat "$TMP_DIR/large_test.json")
long_out=$(printf '%s' "$long_json" | tokenless compress-toon 2>/dev/null)
if [ "${#long_json}" -ge 500 ] && [ -n "$long_out" ] && [ "$long_out" != "$long_json" ]; then
    pass "long payload (${#long_json} chars >= 500) encodes by default"
else
    fail "long payload was not encoded by default"
fi

# ========== Scenario 2: Cosh-NG ==========
section "Scenario 2: Cosh-NG hooks"

scenario "2.1 response compression -> TOON pipeline"

if [ -z "$HOOK_DIR" ]; then
    skip "common hooks directory unavailable ($HOOK_DIR_DETAIL), scenario 2 skipped"
else
    payload=$(cat <<'EOF'
{
  "tool_name": "web_fetch",
  "tool_response": {
    "title": "Test API Response",
    "data": [
      {"id": 1, "name": "Item A", "price": 29.99, "in_stock": true, "category": "electronics"},
      {"id": 2, "name": "Item B", "price": 49.99, "in_stock": false, "category": "clothing"},
      {"id": 3, "name": "Item C", "price": 99.99, "in_stock": true, "category": "electronics"},
      {"id": 4, "name": "Item D", "price": 19.99, "in_stock": true, "category": "food"},
      {"id": 5, "name": "Item E", "price": 149.99, "in_stock": true, "category": "electronics"},
      {"id": 6, "name": "Item F", "price": 39.99, "in_stock": true, "category": "clothing"},
      {"id": 7, "name": "Item G", "price": 59.99, "in_stock": true, "category": "electronics"},
      {"id": 8, "name": "Item H", "price": 79.99, "in_stock": false, "category": "food"},
      {"id": 9, "name": "Item I", "price": 89.99, "in_stock": true, "category": "electronics"},
      {"id": 10, "name": "Item J", "price": 109.99, "in_stock": true, "category": "clothing"}
    ],
    "meta": {"total": 10, "page": 1, "has_next": false},
    "null_field": null,
    "empty_obj": {},
    "empty_arr": []
  }
}
EOF
)

    result=$(
        printf '%s' "$payload" |
            COSH_NG_VERSION=0.5.0 python3 "$HOOK_DIR/compress_response_hook.py" \
                --agent-id copilot-shell 2>/dev/null
    )
    assert_not_empty "$result" "response -> TOON pipeline produces output"
    context=$(printf '%s' "$result" | jq -r '.hookSpecificOutput.updatedToolResponse')
    assert_contains "$context" "data[10]" "pipeline emits TOON tabular content"
    if printf '%s' "$context" | grep -qE "\[tokenless\]|TOON format"; then
        fail "pipeline updatedToolResponse still carries the removed tag prefix"
    else
        pass "pipeline updatedToolResponse has no tag prefix"
    fi
    # Verify the JSON cleanup drops empty-valued fields
    if printf '%s' "$context" | grep -qE "null_field|empty_obj|empty_arr"; then
        fail "response compression kept empty-valued fields"
    else
        pass "response compression removed empty-valued fields"
    fi
fi

# ========== Scenario 3: OpenClaw ==========
section "Scenario 3: OpenClaw agent"

scenario "3.1 OpenClaw plugin state verification"

if [ "$OPENCLAW_STATUS" != "ready" ]; then
    skip "OpenClaw is not ready, all of scenario 3 skipped (reason in the environment check)"
elif [ "${TOKENLESS_TOON_FULL_LIVE:-0}" != "1" ]; then
    skip "scenario 3 makes real model calls; set TOKENLESS_TOON_FULL_LIVE=1 to run it"
else
    # require_live_prerequisites already refused to start without a ready OpenClaw and a
    # usable timeout, so every call below is bounded and really runs.
    SESSION_ID=$(openclaw_live 60 sessions --json 2>/dev/null | python3 -c "
import json, sys
data = json.load(sys.stdin)
sessions = data.get('sessions', [])
# Find first active session
for s in sessions:
    print(s['sessionId'])
    break
" 2>/dev/null || echo "")

    if [ -z "$SESSION_ID" ]; then
        skip "no OpenClaw session ID available (start a session with openclaw first)"
    else
        info "  using session: $SESSION_ID"

        # Check the plugin's active features
        result=$(openclaw_live 180 agent --session-id "$SESSION_ID" --message "ping" --timeout 60 2>&1 || true)
        if echo "$result" | grep -q "toon-compression"; then
            pass "OpenClaw plugin TOON compression feature is active"
        else
            fail "OpenClaw plugin TOON compression feature is not active"
        fi

        # Check all four features
        for feature in rtk-rewrite schema-compression response-compression toon-compression; do
            if echo "$result" | grep -q "$feature"; then
                pass "feature active: $feature"
            else
                fail "feature not active: $feature"
            fi
        done

        scenario "3.2 OpenClaw live call - structured data TOON compression"

        # Let the agent run a command that returns structured JSON data
        result=$(openclaw_live 300 agent --session-id "$SESSION_ID" --message "Run the 'hostname' command and return the result as JSON" --json --timeout 120 2>&1 || true)

        if echo "$result" | grep -q '"runId"'; then
            pass "OpenClaw agent call executed"
        else
            fail "OpenClaw agent call did not execute"
        fi

        # Verify the plugin's log output
        if echo "$result" | grep -q "\[tokenless"; then
            pass "OpenClaw plugin log output present"
        else
            info "  (OpenClaw plugin log output not shown in the current output)"
        fi

        scenario "3.3 OpenClaw call - command rewrite plus response compression/TOON chain"

        result=$(openclaw_live 300 agent --session-id "$SESSION_ID" --message "Run the 'ls /tmp' command and return the result" --json --timeout 120 2>&1 || true)

        if echo "$result" | grep -q '"runId"'; then
            pass "multi-tool chain test succeeded"
        else
            fail "multi-tool chain test failed"
        fi
    fi
fi

# ========== Summary ==========
echo ""
echo "============================================"
echo -e "  Summary: ${GREEN}${PASS}/${TOTAL} passed${NC}, ${RED}${FAIL} failed${NC}, ${YELLOW}${SKIP} skipped${NC}"
echo -e "  Scenarios covered: ${SCENARIOS}"
echo "============================================"
echo ""
echo -e "  ${CYAN}Scenario 1: Tokenless CLI${NC} - encode/decode/round-trip/large JSON/gate and passthrough contract"
echo -e "  ${CYAN}Scenario 2: COSH hooks${NC} - response -> TOON pipeline/tag prefix/empty-value cleanup"
echo -e "  ${CYAN}Scenario 3: OpenClaw${NC} - plugin state/agent call/multi-tool chain (needs TOKENLESS_TOON_FULL_LIVE=1)"
echo ""

[ "$FAIL" -gt 0 ] && exit 1
if [ "$SKIP" -gt 0 ]; then
    echo -e "${GREEN}Every executed case passed (${SKIP} skipped for missing prerequisites)${NC}"
else
    echo -e "${GREEN}All tests passed${NC}"
fi
