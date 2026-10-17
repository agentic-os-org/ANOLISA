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

# The reported version is overridable so the supported-version floor can be
# exercised without a second stub binary.
cat > "$FAKE_BIN/opencode" <<'STUBEOF'
#!/usr/bin/env bash
if [ "${1:-}" = "--version" ]; then
    echo "${TOKENLESS_TEST_OPENCODE_VERSION:-1.19.2}"
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

# -- The supported-version floor ------------------------------------------------
# OpenCode 1.0.0-1.3.3 call every module export as a plugin function, so the
# object default export OpenCode 2 requires would abort their initialization
# with `TypeError: fn is not a function`. The installer must not link the plugin
# into those hosts, and detect must say why instead of reporting "installable".
rm -f "$PLUGIN_LINK"
for version in 1.3.4 1.19.2 2.0.29; do
    if TOKENLESS_TEST_OPENCODE_VERSION="$version" bash "$INSTALL_SH" >/dev/null 2>&1 &&
        [ -L "$PLUGIN_LINK" ] && [ "$(readlink "$PLUGIN_LINK")" = "$PLUGIN_SOURCE" ]; then
        pass "installer supports OpenCode ${version}"
    else
        fail "installer refused supported OpenCode ${version}"
    fi
    rm -f "$PLUGIN_LINK"
done

for version in 1.0.0 1.2.3-test 1.3.3; do
    detect_rc=0
    TOKENLESS_TEST_OPENCODE_VERSION="$version" bash "$DETECT_SH" >/dev/null 2>&1 || detect_rc=$?
    install_rc=0
    TOKENLESS_TEST_OPENCODE_VERSION="$version" bash "$INSTALL_SH" >/dev/null 2>&1 || install_rc=$?
    if [ "$detect_rc" -eq 2 ] && [ "$install_rc" -eq 0 ] &&
        [ ! -e "$PLUGIN_LINK" ] && [ ! -L "$PLUGIN_LINK" ]; then
        pass "OpenCode ${version} is reported unsupported and never linked"
    else
        fail "OpenCode ${version}: detect_rc=${detect_rc} install_rc=${install_rc} link left behind"
    fi
done

echo ""
echo "OpenCode adapter tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
