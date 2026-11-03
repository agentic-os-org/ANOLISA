#!/usr/bin/env bash
# Regression tests for the claude-code adapter's ANOLISA_DRY_RUN previews.
#
# The adapter drives the official `claude plugin` CLI and, when the plugin
# manifest has not been stamped yet, writes it into the plugin tree itself; its
# uninstall additionally rewrites settings.json and deletes the plugin cache in
# the no-CLI fallback. The two uninstall branches differ, so the preview has to
# name the one the host is on.
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
mkdir -p "$FAKE_HOME/.claude" "$ADAPTER_DIR" "$SANDBOX/bin"
cp -R "$SOURCE_ADAPTER_DIR"/. "$ADAPTER_DIR"/

INSTALL_SH="$ADAPTER_DIR/claude-code/scripts/install.sh"
UNINSTALL_SH="$ADAPTER_DIR/claude-code/scripts/uninstall.sh"
PLUGIN_SRC="$ADAPTER_DIR/claude-code"
MANIFEST="$PLUGIN_SRC/.claude-plugin/plugin.json"
TEMPLATE="$PLUGIN_SRC/.claude-plugin/plugin.json.in"
SETTINGS="$FAKE_HOME/.claude/settings.json"
CACHE="$FAKE_HOME/.claude/plugins/cache/anolisa-tokenless"
STUB_LOG="$FAKE_HOME/.claude-stub.log"

# The manifest is a generated file; drop the stamped copy so the first dry run
# meets the dev-fallback path the preview has to describe.
rm -f "$MANIFEST"

# A `claude` stand-in: it logs every invocation, answers the version probes and
# the marketplace listing (empty unless a marker says otherwise), and drops a
# cache marker on `plugin install`.
CLAUDE_STUB="$SANDBOX/bin/claude"
cat > "$CLAUDE_STUB" <<'STUBEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$HOME/.claude-stub.log"
case "$*" in
    --version|version|--help) echo "1.0.0 (Claude Code)"; exit 0 ;;
    "plugin marketplace list")
        [ -f "$HOME/.marketplace-registered" ] && echo "anolisa-tokenless"
        exit 0 ;;
    "plugin install"*) mkdir -p "$HOME/.claude/plugins/cache/anolisa-tokenless"; exit 0 ;;
    *) exit 0 ;;
esac
STUBEOF
chmod +x "$CLAUDE_STUB"

# The CLI is the only witness in the settings checks: settings.json holds
# another plugin's entry, so the uninstall's fail-closed verification accepts a
# removal it cannot confirm with jq (which this workspace does not have).
cat > "$SETTINGS" <<'JSONEOF'
{
  "enabledPlugins": {"foreign@local": true},
  "hooks": {"PreToolUse": [{"hooks": [{"command": "keep-me"}]}]}
}
JSONEOF
settings_before="$(cksum "$SETTINGS")"

# The flag under test must be the only one in play: a shell that exported
# ANOLISA_DRY_RUN would silently turn the "real" runs below into dry runs.
env_for() {
    env -u ANOLISA_DRY_RUN -u ANOLISA_TARGET -u ANOLISA_COMPONENT \
        -u TOKENLESS_VERSION -u ANOLISA_VERSION \
        HOME="$FAKE_HOME" CLAUDE_BIN="$CLAUDE_STUB" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" "$@"
}
calls() {
    local n
    n="$(grep -Fc -- "$1" "$STUB_LOG" 2>/dev/null)"
    echo "${n:-0}"
}
snapshot() { cat "$STUB_LOG" 2>/dev/null || true; }
log_size() { wc -c < "$STUB_LOG" 2>/dev/null || echo 0; }
expect_line() {  # <label> <expected line> <output>
    case "$3" in
        *"$2"*) pass "$1" ;;
        *) fail "$1 (missing: $2)" ;;
    esac
}
expect_no_line() {  # <label> <unexpected line> <output>
    if printf '%s\n' "$3" | grep -Fq -- "$2"; then
        fail "$1 (unexpected: $2)"
    else
        pass "$1"
    fi
}

# ── A dry-run install previews and writes nothing ───────────────────────────
dry_install_out="$(env_for env ANOLISA_DRY_RUN=1 bash "$INSTALL_SH" 2>&1)"
expect_line "dry-run install previews the dev-fallback stamp" \
    "DRY-RUN: stamp $MANIFEST from $TEMPLATE" "$dry_install_out"
expect_line "dry-run install previews the qualified marketplace add" \
    "DRY-RUN: $CLAUDE_STUB plugin marketplace add $PLUGIN_SRC (only when the marketplace is not registered)" "$dry_install_out"
expect_line "dry-run install previews the plugin install" \
    "DRY-RUN: $CLAUDE_STUB plugin install tokenless@anolisa-tokenless" "$dry_install_out"
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
if [ "$(calls 'plugin marketplace add')" -ge 1 ] && \
        [ "$(calls 'plugin install tokenless@anolisa-tokenless')" -ge 1 ]; then
    pass "install registers the marketplace and the plugin through the CLI"
else
    fail "install did not register with the CLI"
fi
if [ "$(log_size)" -gt 0 ]; then
    pass "the CLI log is a usable snapshot"
else
    fail "the CLI log is empty, so the snapshots below prove nothing"
fi

# ── An already-stamped host does not get a stamp preview ────────────────────
stamped_out="$(env_for env ANOLISA_DRY_RUN=1 bash "$INSTALL_SH" 2>&1)"
expect_no_line "an already-stamped host is not told the manifest would be stamped" \
    "DRY-RUN: stamp $MANIFEST" "$stamped_out"
expect_line "an already-stamped host still gets the registration preview" \
    "DRY-RUN: $CLAUDE_STUB plugin install tokenless@anolisa-tokenless" "$stamped_out"

# ── A dry-run uninstall previews and removes nothing ────────────────────────
calls_before="$(snapshot)"
dry_uninstall_out="$(env_for env ANOLISA_DRY_RUN=1 bash "$UNINSTALL_SH" 2>&1)"
expect_line "dry-run uninstall previews the plugin removal" \
    "DRY-RUN: $CLAUDE_STUB plugin uninstall tokenless@anolisa-tokenless" "$dry_uninstall_out"
expect_line "dry-run uninstall previews the marketplace removal" \
    "DRY-RUN: $CLAUDE_STUB plugin marketplace remove anolisa-tokenless" "$dry_uninstall_out"
expect_line "dry-run uninstall previews the settings verification" \
    "DRY-RUN: verify that tokenless@anolisa-tokenless and anolisa-tokenless are gone from $SETTINGS" "$dry_uninstall_out"
expect_no_line "a dry run with the CLI in place does not claim the cache is deleted" \
    "rm -rf $CACHE" "$dry_uninstall_out"
if [ -d "$CACHE" ] && [ "$(cksum "$SETTINGS")" = "$settings_before" ] && \
        [ "$(snapshot)" = "$calls_before" ]; then
    pass "dry-run uninstall leaves the cache, settings.json and the CLI alone"
else
    fail "dry-run uninstall removed host state"
fi

# ── Without a resolvable CLI the preview names the fallback instead ─────────
no_cli_out="$(env_for env ANOLISA_DRY_RUN=1 CLAUDE_BIN="$SANDBOX/bin/missing-claude" \
    bash "$UNINSTALL_SH" 2>&1)"
expect_line "a no-CLI dry run previews the manual fallback" \
    "DRY-RUN: no claude CLI in $SANDBOX/bin/missing-claude: manual fallback only" "$no_cli_out"
expect_line "the fallback preview names the settings rewrite" \
    "DRY-RUN: rewrite $SETTINGS to drop tokenless@anolisa-tokenless and anolisa-tokenless (needs jq)" "$no_cli_out"
expect_line "the fallback preview names the cache removal" \
    "DRY-RUN: rm -rf $CACHE" "$no_cli_out"
expect_no_line "a no-CLI dry run does not announce CLI calls it cannot make" \
    "plugin uninstall tokenless@anolisa-tokenless" "$no_cli_out"

# ── The real uninstall still drives the CLI and stays idempotent ────────────
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
