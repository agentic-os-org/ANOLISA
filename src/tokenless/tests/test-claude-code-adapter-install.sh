#!/usr/bin/env bash
# Regression tests for the claude-code adapter's ANOLISA_DRY_RUN previews.
#
# The adapter drives the official `claude plugin` CLI and, when the plugin
# manifest has not been stamped yet, writes it into the plugin tree itself; its
# uninstall additionally rewrites settings.json and deletes the plugin cache.
# None of that used to be previewed: a dry run did the whole thing.
set -uo pipefail

PASS=0
FAIL=0
pass() { echo "[PASS] $1"; PASS=$((PASS + 1)); }
fail() { echo "[FAIL] $1" >&2; FAIL=$((FAIL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless"
SANDBOX="$(mktemp -d -t tokenless-claude-code-test.XXXXXX)"
trap 'rm -rf "$SANDBOX"' EXIT

FAKE_HOME="$SANDBOX/home"
ADAPTER_DIR="$SANDBOX/adapter root"
mkdir -p "$FAKE_HOME/.claude" "$ADAPTER_DIR"
cp -R "$SOURCE_ADAPTER_DIR"/. "$ADAPTER_DIR"/

INSTALL_SH="$ADAPTER_DIR/claude-code/scripts/install.sh"
UNINSTALL_SH="$ADAPTER_DIR/claude-code/scripts/uninstall.sh"
PLUGIN_SRC="$ADAPTER_DIR/claude-code"
MANIFEST="$PLUGIN_SRC/.claude-plugin/plugin.json"
SETTINGS="$FAKE_HOME/.claude/settings.json"
CACHE="$FAKE_HOME/.claude/plugins/cache/anolisa-tokenless"
STUB_LOG="$FAKE_HOME/.claude-stub.log"

# The stamps are generated files in the real tree; remove them so the test
# exercises the dev-fallback path against the sandbox copy alone.
rm -f "$MANIFEST" "$PLUGIN_SRC/.claude-plugin/plugin.json.in.stamp" 2>/dev/null || true

# A `claude` stand-in: it logs every invocation, answers the version probes and
# the marketplace listing (empty, so the marketplace is never "already there").
# On `plugin install` it drops a marker the uninstall assertions can watch.
CLAUDE_STUB="$SANDBOX/bin/claude"
mkdir -p "$SANDBOX/bin"
cat > "$CLAUDE_STUB" <<'STUBEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$HOME/.claude-stub.log"
case "$*" in
    --version|version|--help) echo "1.0.0 (Claude Code)"; exit 0 ;;
    "plugin marketplace list") exit 0 ;;
    "plugin install"*) mkdir -p "$HOME/.claude/plugins/cache/anolisa-tokenless"; exit 0 ;;
    *) exit 0 ;;
esac
STUBEOF
chmod +x "$CLAUDE_STUB"

# The CLI is the only witness here: settings.json holds another plugin's entry
# and nothing about tokenless, which is the state an uninstall must also accept
# (the rewrite the script performs with jq has to be verified, not trusted).
cat > "$SETTINGS" <<'JSONEOF'
{
  "enabledPlugins": {"foreign@local": true},
  "hooks": {"PreToolUse": [{"hooks": [{"command": "keep-me"}]}]}
}
JSONEOF
settings_before="$(cksum "$SETTINGS")"

env_for() {
    HOME="$FAKE_HOME" CLAUDE_BIN="$CLAUDE_STUB" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" "$@"
}
calls() { grep -Fc -- "$1" "$STUB_LOG" 2>/dev/null || echo 0; }

# ── A dry-run install previews and writes nothing ───────────────────────────
dry_install_out="$(env_for env ANOLISA_DRY_RUN=1 bash "$INSTALL_SH" 2>&1)"
case "$dry_install_out" in
    *DRY-RUN:*) pass "dry-run install prints a DRY-RUN preview" ;;
    *) fail "dry-run install printed no preview: ${dry_install_out}" ;;
esac
if [ ! -f "$MANIFEST" ] && [ ! -e "$STUB_LOG" ]; then
    pass "dry-run install leaves the plugin tree and the CLI alone"
else
    fail "dry-run install stamped the manifest or called the CLI"
fi

# ── The real install still stamps the manifest and drives the CLI ───────────
if env_for bash "$INSTALL_SH" >/dev/null 2>&1; then
    pass "install succeeds"
else
    fail "install failed"
fi
if [ -f "$MANIFEST" ]; then
    pass "install stamps the plugin manifest"
else
    fail "install did not stamp the manifest"
fi
if [ "$(calls 'plugin marketplace add')" -ge 1 ] && [ "$(calls 'plugin install tokenless@anolisa-tokenless')" -ge 1 ]; then
    pass "install registers the marketplace and the plugin through the CLI"
else
    fail "install did not register with the CLI"
fi

# ── A dry-run uninstall previews and removes nothing ────────────────────────
calls_before="$(cat "$STUB_LOG")"
dry_uninstall_out="$(env_for env ANOLISA_DRY_RUN=1 bash "$UNINSTALL_SH" 2>&1)"
case "$dry_uninstall_out" in
    *DRY-RUN:*) pass "dry-run uninstall prints a DRY-RUN preview" ;;
    *) fail "dry-run uninstall printed no preview: ${dry_uninstall_out}" ;;
esac
if [ -d "$CACHE" ] && [ "$(cksum "$SETTINGS")" = "$settings_before" ] && \
        [ "$(cat "$STUB_LOG")" = "$calls_before" ]; then
    pass "dry-run uninstall leaves the cache, settings.json and the CLI alone"
else
    fail "dry-run uninstall removed host state"
fi

# ── The real uninstall still drives the CLI and cleans the cache ────────────
if env_for bash "$UNINSTALL_SH" >/dev/null 2>&1; then
    pass "uninstall succeeds"
else
    fail "uninstall failed"
fi
if [ "$(calls 'plugin uninstall tokenless@anolisa-tokenless')" -ge 1 ] && \
        [ "$(calls 'plugin marketplace remove anolisa-tokenless')" -ge 1 ]; then
    pass "uninstall deregisters the plugin and the marketplace"
else
    fail "uninstall did not deregister with the CLI"
fi
# The CLI path leaves the cache to the CLI (only the no-CLI fallback removes
# it), so the second uninstall must simply stay idempotent.
if env_for bash "$UNINSTALL_SH" >/dev/null 2>&1; then
    pass "repeated uninstall is idempotent"
else
    fail "repeated uninstall failed"
fi

echo ""
echo "Claude Code adapter tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
