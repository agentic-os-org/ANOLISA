#!/usr/bin/env bash
# Regression tests for the OpenCode local-plugin lifecycle.
set -euo pipefail

PASS=0
FAIL=0
pass() { echo "[PASS] $1"; PASS=$((PASS + 1)); }
fail() { echo "[FAIL] $1" >&2; FAIL=$((FAIL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless"
SANDBOX="$(mktemp -d -t tokenless-opencode-install-test.XXXXXX)"
trap 'rm -rf "$SANDBOX"' EXIT

FAKE_HOME="$SANDBOX/home"
FAKE_BIN="$SANDBOX/bin"
ADAPTER_DIR="$SANDBOX/adapter root"
CONFIG_DIR="$SANDBOX/config root/opencode"
mkdir -p "$FAKE_HOME" "$FAKE_BIN" "$ADAPTER_DIR/opencode/scripts"
cp "$SOURCE_ADAPTER_DIR/opencode/plugin.js" "$ADAPTER_DIR/opencode/plugin.js"
cp "$SOURCE_ADAPTER_DIR/opencode/scripts/"*.sh "$ADAPTER_DIR/opencode/scripts/"

cat > "$FAKE_BIN/opencode" <<'STUBEOF'
#!/usr/bin/env bash
if [ "${1:-}" = "--version" ]; then
    echo "1.2.3-test"
    exit 0
fi
exit 2
STUBEOF
chmod +x "$FAKE_BIN/opencode"

export HOME="$FAKE_HOME"
export PATH="$FAKE_BIN:$PATH"
export ANOLISA_ADAPTER_DIR="$ADAPTER_DIR"
export TOKENLESS_OPENCODE_CONFIG_DIR="$CONFIG_DIR"

DETECT_SH="$ADAPTER_DIR/opencode/scripts/detect.sh"
INSTALL_SH="$ADAPTER_DIR/opencode/scripts/install.sh"
UNINSTALL_SH="$ADAPTER_DIR/opencode/scripts/uninstall.sh"
PLUGIN_SOURCE="$ADAPTER_DIR/opencode/plugin.js"
PLUGIN_LINK="$CONFIG_DIR/plugins/tokenless.js"

if bash "$DETECT_SH" >/dev/null 2>&1; then
    detect_rc=0
else
    detect_rc=$?
fi
if [ "$detect_rc" -eq 1 ]; then
    pass "detect reports an installable OpenCode plugin"
else
    fail "detect returned $detect_rc before installation"
fi

if bash "$INSTALL_SH" >/dev/null; then
    pass "OpenCode plugin installation succeeds"
else
    fail "OpenCode plugin installation failed"
fi
if [ -L "$PLUGIN_LINK" ] && [ "$(readlink "$PLUGIN_LINK")" = "$PLUGIN_SOURCE" ]; then
    pass "installer creates the expected global plugin link"
else
    fail "installer did not create the managed plugin link"
fi
if bash "$DETECT_SH" >/dev/null; then
    pass "detect recognizes the installed plugin"
else
    fail "detect did not recognize the installed plugin"
fi
if bash "$INSTALL_SH" >/dev/null; then
    pass "OpenCode plugin installation is idempotent"
else
    fail "repeated OpenCode plugin installation failed"
fi

ADAPTER_ALIAS="$SANDBOX/adapter alias"
ln -s "$ADAPTER_DIR" "$ADAPTER_ALIAS"
export ANOLISA_ADAPTER_DIR="$ADAPTER_ALIAS"
if bash "$DETECT_SH" >/dev/null; then
    pass "detect recognizes the plugin through an adapter symlink"
else
    fail "detect rejected an equivalent adapter symlink"
fi
if bash "$INSTALL_SH" >/dev/null; then
    pass "installer recognizes the plugin through an adapter symlink"
else
    fail "installer rejected an equivalent adapter symlink"
fi

if bash "$UNINSTALL_SH" >/dev/null; then
    pass "OpenCode plugin uninstallation succeeds through an adapter symlink"
else
    fail "OpenCode plugin uninstallation failed"
fi
export ANOLISA_ADAPTER_DIR="$ADAPTER_DIR"
if [ ! -e "$PLUGIN_LINK" ] && [ ! -L "$PLUGIN_LINK" ]; then
    pass "uninstaller removes the managed plugin link"
else
    fail "uninstaller left the managed plugin link behind"
fi
if bash "$UNINSTALL_SH" >/dev/null; then
    pass "OpenCode plugin uninstallation is idempotent"
else
    fail "repeated OpenCode plugin uninstallation failed"
fi

mkdir -p "$(dirname "$PLUGIN_LINK")"
printf '%s\n' 'export const ForeignPlugin = async () => ({})' > "$PLUGIN_LINK"
foreign_before="$(cksum "$PLUGIN_LINK")"
if bash "$INSTALL_SH" >/dev/null 2>&1; then
    fail "installer replaced an unmanaged plugin file"
else
    pass "installer refuses to replace an unmanaged plugin file"
fi
if bash "$UNINSTALL_SH" >/dev/null 2>&1; then
    pass "uninstaller leaves an unmanaged plugin file without failing"
else
    fail "uninstaller failed for an unmanaged plugin file"
fi
if [ "$(cksum "$PLUGIN_LINK")" = "$foreign_before" ]; then
    pass "unmanaged plugin file remains unchanged"
else
    fail "unmanaged plugin file was modified"
fi

# A dry-run uninstall must preview the removal and leave the live link
# in place, mirroring the installer's ANOLISA_DRY_RUN handling.
rm -f "$PLUGIN_LINK"
bash "$INSTALL_SH" >/dev/null
dry_run_output="$(ANOLISA_DRY_RUN=1 bash "$UNINSTALL_SH" 2>&1)"
if [ $? -eq 0 ] && [ -L "$PLUGIN_LINK" ] && [ "$(readlink "$PLUGIN_LINK")" = "$PLUGIN_SOURCE" ]; then
    pass "dry-run uninstall preserves the managed plugin link"
else
    fail "dry-run uninstall removed or damaged the managed plugin link"
fi
case "$dry_run_output" in
    *DRY-RUN:*) pass "dry-run uninstall previews the removal" ;;
    *) fail "dry-run uninstall did not print a DRY-RUN preview" ;;
esac
if bash "$UNINSTALL_SH" >/dev/null; then
    pass "real uninstall still removes the managed plugin link"
else
    fail "real uninstall stopped working"
fi

# A dry-run install must name every write the real path performs - including the
# plugin directory it creates when it is missing - and perform none of them.
rm -f "$PLUGIN_LINK"
rm -rf "$CONFIG_DIR/plugins"
dry_install_output="$(ANOLISA_DRY_RUN=1 bash "$INSTALL_SH" 2>&1)"
case "$dry_install_output" in
    *"DRY-RUN: mkdir -p $CONFIG_DIR/plugins"*) pass "dry-run install previews the plugin directory it would create" ;;
    *) fail "dry-run install does not name the plugin directory: ${dry_install_output}" ;;
esac
case "$dry_install_output" in
    *"DRY-RUN: ln -s $PLUGIN_SOURCE $PLUGIN_LINK"*) pass "dry-run install previews the plugin link" ;;
    *) fail "dry-run install does not name the plugin link: ${dry_install_output}" ;;
esac
if [ ! -d "$CONFIG_DIR/plugins" ] && [ ! -e "$PLUGIN_LINK" ]; then
    pass "dry-run install creates neither the directory nor the link"
else
    fail "dry-run install wrote to the config directory"
fi

# With the directory already in place the real path creates nothing, so the
# preview must not claim it would.
mkdir -p "$CONFIG_DIR/plugins"
present_output="$(ANOLISA_DRY_RUN=1 bash "$INSTALL_SH" 2>&1)"
case "$present_output" in
    *"mkdir -p $CONFIG_DIR/plugins"*) fail "an existing plugin directory is still previewed as a creation" ;;
    *) pass "an existing plugin directory is not previewed as a creation" ;;
esac

echo ""
echo "OpenCode adapter tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
