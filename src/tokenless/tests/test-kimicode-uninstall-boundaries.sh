#!/usr/bin/env bash
# Regression test for kimicode uninstall boundary handling.
# Ensures the awk fallback stops at any TOML table header, not only [[...]]
# array tables, so provider/API-key configs after a tokenless hook are preserved.
set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
UNINSTALL="$SCRIPT_DIR/../adapters/tokenless/kimicode/scripts/uninstall.sh"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

export HOME="$TEST_DIR/home"
export KIMI_SHARE_DIR=""
mkdir -p "$HOME/.kimi"

KIMI_CONFIG="$HOME/.kimi/config.toml"

# Prepare a minimal PATH for forcing the awk fallback (no python3).
FAKE_BIN="$TEST_DIR/fake-bin"
mkdir -p "$FAKE_BIN"
for cmd in bash awk cat mv rm stat; do
    real=$(command -v "$cmd" 2>/dev/null || true)
    if [ -n "$real" ]; then
        ln -sf "$real" "$FAKE_BIN/$cmd"
    fi
done

write_config() {
    cat >"$KIMI_CONFIG" "$1"
}

assert_provider_preserved() {
    grep -q '^\[providers\.kimi\]' "$KIMI_CONFIG" || {
        echo "FAIL: provider table was removed"
        exit 1
    }
    grep -q '^api_key' "$KIMI_CONFIG" || {
        echo "FAIL: api_key was removed"
        exit 1
    }
}

assert_tokenless_removed() {
    if grep -q 'tokenless-tool-ready\|tool-ready-kimi-wrapper' "$KIMI_CONFIG"; then
        echo "FAIL: tokenless hook was not removed"
        exit 1
    fi
}

run_uninstall_awk_fallback() {
    # Force the awk fallback by using a PATH that contains the commands
    # uninstall.sh needs but no python3 executable.
    PATH="$FAKE_BIN" bash "$UNINSTALL"
}

# --- Case 1: tokenless hook followed by plain [providers.kimi] table (awk) ---
write_config - <<'EOF'
[[hooks]]
event = "PreToolUse"
matcher = ""
command = "bash '/tmp/tool-ready-kimi-wrapper.sh'"
timeout = 15
# tokenless-tool-ready: Pre-checks tool environment readiness

[providers.kimi]
api_key = "secret"
EOF

run_uninstall_awk_fallback
assert_tokenless_removed
assert_provider_preserved

# --- Case 2: tokenless hook followed by array [[providers]] table (awk) ---
write_config - <<'EOF'
[[hooks]]
event = "PreToolUse"
matcher = ""
command = "bash '/tmp/tool-ready-kimi-wrapper.sh'"
timeout = 15
# tokenless-tool-ready: Pre-checks tool environment readiness

[[providers]]
name = "kimi"
api_key = "secret"
EOF

run_uninstall_awk_fallback
assert_tokenless_removed
grep -q '^\[\[providers\]\]' "$KIMI_CONFIG" || { echo "FAIL: providers array table was removed"; exit 1; }
grep -q '^name = "kimi"' "$KIMI_CONFIG" || { echo "FAIL: provider name was removed"; exit 1; }

# --- Case 3: tokenless hook at EOF (awk) ---
write_config - <<'EOF'
[providers.kimi]
api_key = "secret"

[[hooks]]
event = "PreToolUse"
matcher = ""
command = "bash '/tmp/tool-ready-kimi-wrapper.sh'"
timeout = 15
# tokenless-tool-ready: Pre-checks tool environment readiness
EOF

run_uninstall_awk_fallback
assert_tokenless_removed
# provider should still be there
[ "$(grep -c '^\[providers\.kimi\]' "$KIMI_CONFIG")" -eq 1 ]

# --- Case 4: NON-tokenless hook followed by plain table header (awk) ---
# The header must be emitted exactly once (duplicate TOML tables are invalid)
# and the unrelated hook block must be preserved untouched.
write_config - <<'EOF'
[[hooks]]
event = "Notification"
command = "bash /some/other/hook.sh"
[providers.kimi]
api_key = "secret"
EOF

run_uninstall_awk_fallback
[ "$(grep -c '^\[\[hooks\]\]' "$KIMI_CONFIG")" -eq 1 ] || { echo "FAIL: unrelated hook block was not preserved"; exit 1; }
grep -q 'other/hook\.sh' "$KIMI_CONFIG" || { echo "FAIL: unrelated hook command was removed"; exit 1; }
[ "$(grep -c '^\[providers\.kimi\]' "$KIMI_CONFIG")" -eq 1 ] || { echo "FAIL: provider table header duplicated or missing"; exit 1; }
grep -q '^api_key' "$KIMI_CONFIG" || { echo "FAIL: api_key was removed"; exit 1; }

# --- Case 5: same scenarios via Python path (when python3 is available) ---
if command -v python3 >/dev/null 2>&1; then
    write_config - <<'EOF'
[[hooks]]
event = "PreToolUse"
matcher = ""
command = "bash '/tmp/tool-ready-kimi-wrapper.sh'"
timeout = 15
# tokenless-tool-ready: Pre-checks tool environment readiness

[providers.kimi]
api_key = "secret"
EOF
    bash "$UNINSTALL"
    assert_tokenless_removed
    assert_provider_preserved
fi

echo "kimicode uninstall boundary test passed"
