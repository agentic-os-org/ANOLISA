#!/usr/bin/env bash
# Regression test: kimicode install must not create/modify ~/.kimi/config.toml
# when the kimi CLI is not installed (detect.sh exits 2).
set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
INSTALL="$SCRIPT_DIR/../adapters/tokenless/kimicode/scripts/install.sh"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

export HOME="$TEST_DIR/home"
export KIMI_SHARE_DIR=""
mkdir -p "$HOME/.kimi"

# Ensure kimi is not on PATH.
export PATH="/usr/bin:/bin"
if command -v kimi >/dev/null 2>&1; then
    echo "SKIP: real kimi CLI is present on PATH; cannot test missing-kimi path"
    exit 0
fi

KIMI_CONFIG="$HOME/.kimi/config.toml"

# Pre-create some existing content to prove it is not touched.
echo "[existing]" >"$KIMI_CONFIG"
original_mtime=$(stat -c '%Y' "$KIMI_CONFIG" 2>/dev/null || stat -f '%m' "$KIMI_CONFIG")

# Run install with a detect script that reports missing kimi.
ANOLISA_ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless" bash "$INSTALL" || true

# The config file must not have been rewritten by the install script.
[ "$(stat -c '%Y' "$KIMI_CONFIG" 2>/dev/null || stat -f '%m' "$KIMI_CONFIG")" -eq "$original_mtime" ]
grep -q '^\[existing\]$' "$KIMI_CONFIG"

# No tokenless hooks should have been injected.
if grep -qE 'tokenless-tool-ready|\[\[hooks\]\]' "$KIMI_CONFIG"; then
    echo "FAIL: config was modified despite missing kimi CLI"
    exit 1
fi

echo "kimicode install skip-when-missing test passed"
