#!/usr/bin/env bash
# Regression tests for the Codex adapter's ANOLISA_DRY_RUN previews.
#
# The adapter is the only one of the tokenless family whose install and
# uninstall did the full job whatever ANOLISA_DRY_RUN said: install created the
# marketplace tree, the plugin symlink and the manifest and drove `codex plugin
# marketplace add` / `codex plugin add`, uninstall drove the removals and then
# deleted the marketplace directory and the component binary.
set -uo pipefail

PASS=0
FAIL=0
pass() { echo "[PASS] $1"; PASS=$((PASS + 1)); }
fail() { echo "[FAIL] $1" >&2; FAIL=$((FAIL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless"
SANDBOX="$(mktemp -d -t tokenless-codex-install-test.XXXXXX)"
trap 'rm -rf "$SANDBOX"' EXIT

FAKE_HOME="$SANDBOX/home"
PREFIX="$SANDBOX/prefix"
XDG="$SANDBOX/xdg"
mkdir -p "$FAKE_HOME" "$PREFIX/bin" "$SANDBOX/bin" "$XDG"

INSTALL_SH="$ADAPTER_DIR/codex/scripts/install.sh"
UNINSTALL_SH="$ADAPTER_DIR/codex/scripts/uninstall.sh"
MARKETPLACE_DIR="$XDG/anolisa/codex-marketplace"
AGENTS_DIR="$MARKETPLACE_DIR/.agents/plugins"
PLUGIN_LINK="$MARKETPLACE_DIR/tokenless"
PLUGIN_SRC="$(cd "$ADAPTER_DIR/codex" && pwd)"
STUB_LOG="$FAKE_HOME/.codex-stub.log"

# The tokenless CLI phase 1 looks for.
cat > "$PREFIX/bin/tokenless" <<'STUBEOF'
#!/usr/bin/env bash
[ "${1:-}" = "--version" ] && echo "tokenless 0.0.0-test"
exit 0
STUBEOF
chmod +x "$PREFIX/bin/tokenless"

# The codex CLI stand-in: it logs every call and answers both listings with an
# empty inventory, so only the file operations under test are observable.
CODEX_STUB="$SANDBOX/bin/codex"
cat > "$CODEX_STUB" <<'STUBEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$HOME/.codex-stub.log"
exit 0
STUBEOF
chmod +x "$CODEX_STUB"

env_for() {
    HOME="$FAKE_HOME" TOKENLESS_INSTALL_PREFIX="$PREFIX" XDG_DATA_HOME="$XDG" \
        CODEX_BIN="$CODEX_STUB" "$@"
}
calls() { grep -Fc -- "$1" "$STUB_LOG" 2>/dev/null || echo 0; }

# ── A dry-run install previews and writes nothing ───────────────────────────
dry_install_out="$(env_for env ANOLISA_DRY_RUN=1 bash "$INSTALL_SH" 2>&1)"
case "$dry_install_out" in
    *DRY-RUN:*) pass "dry-run install prints a DRY-RUN preview" ;;
    *) fail "dry-run install printed no preview: ${dry_install_out}" ;;
esac
if [ ! -e "$MARKETPLACE_DIR" ] && [ ! -e "$STUB_LOG" ]; then
    pass "dry-run install leaves the marketplace tree, the manifest and the CLI alone"
else
    fail "dry-run install wrote to the host"
fi

# ── The real install still builds the layout and drives the CLI ─────────────
if env_for bash "$INSTALL_SH" >/dev/null 2>&1; then
    pass "install succeeds"
else
    fail "install failed"
fi
if [ -L "$PLUGIN_LINK" ] && [ "$(readlink "$PLUGIN_LINK")" = "$PLUGIN_SRC" ] && \
        [ -f "$AGENTS_DIR/marketplace.json" ]; then
    pass "install links the plugin in and writes the marketplace manifest"
else
    fail "install did not create the marketplace layout"
fi
if [ "$(calls 'plugin marketplace add')" -ge 1 ] && \
        [ "$(calls 'plugin add tokenless@anolisa-tokenless')" -ge 1 ]; then
    pass "install registers the marketplace and the plugin through the CLI"
else
    fail "install did not register with the CLI"
fi

# ── A dry-run uninstall previews and removes nothing ────────────────────────
calls_before="$(cat "$STUB_LOG")"
dry_uninstall_out="$(env_for env ANOLISA_DRY_RUN=1 bash "$UNINSTALL_SH" --non-interactive 2>&1)"
case "$dry_uninstall_out" in
    *DRY-RUN:*) pass "dry-run uninstall prints a DRY-RUN preview" ;;
    *) fail "dry-run uninstall printed no preview: ${dry_uninstall_out}" ;;
esac
if [ -L "$PLUGIN_LINK" ] && [ -f "$PREFIX/bin/tokenless" ] && \
        [ "$(cat "$STUB_LOG")" = "$calls_before" ]; then
    pass "dry-run uninstall leaves the marketplace, the binary and the CLI alone"
else
    fail "dry-run uninstall removed host state"
fi

# ── The real uninstall still removes both ───────────────────────────────────
if env_for bash "$UNINSTALL_SH" --non-interactive >/dev/null 2>&1; then
    pass "uninstall succeeds"
else
    fail "uninstall failed"
fi
if [ ! -e "$MARKETPLACE_DIR" ] && [ ! -e "$PREFIX/bin/tokenless" ]; then
    pass "uninstall removes the marketplace directory and the component binary"
else
    fail "uninstall left the marketplace directory or the binary behind"
fi

echo ""
echo "Codex adapter tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
