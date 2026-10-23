#!/usr/bin/env bash
# Regression tests for the Command Code (cmd) mod lifecycle.
set -euo pipefail

PASS=0
FAIL=0
pass() { echo "[PASS] $1"; PASS=$((PASS + 1)); }
fail() { echo "[FAIL] $1" >&2; FAIL=$((FAIL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless"
SANDBOX="$(mktemp -d -t tokenless-cmd-install-test.XXXXXX)"
trap 'rm -rf "$SANDBOX"' EXIT

FAKE_HOME="$SANDBOX/fake home"
FAKE_BIN="$SANDBOX/bin"
ADAPTER_DIR="$SANDBOX/adapter root"
# The location Command Code itself scans; the space in FAKE_HOME keeps the
# lifecycle scripts' path quoting honest.
MODS_DIR="$FAKE_HOME/.commandcode/mods"
mkdir -p "$FAKE_HOME" "$FAKE_BIN" "$ADAPTER_DIR/cmd/scripts"
cp "$SOURCE_ADAPTER_DIR/cmd/tokenless.ts" "$ADAPTER_DIR/cmd/tokenless.ts"
cp "$SOURCE_ADAPTER_DIR/cmd/scripts/"*.sh "$ADAPTER_DIR/cmd/scripts/"

cat > "$FAKE_BIN/cmd" <<'STUBEOF'
#!/usr/bin/env bash
if [ "${1:-}" = "--version" ]; then
    echo "1.82.5-test"
    exit 0
fi
exit 2
STUBEOF
chmod +x "$FAKE_BIN/cmd"

export HOME="$FAKE_HOME"
export PATH="$FAKE_BIN:$PATH"
export ANOLISA_ADAPTER_DIR="$ADAPTER_DIR"

DETECT_SH="$ADAPTER_DIR/cmd/scripts/detect.sh"
INSTALL_SH="$ADAPTER_DIR/cmd/scripts/install.sh"
UNINSTALL_SH="$ADAPTER_DIR/cmd/scripts/uninstall.sh"
MOD_SOURCE="$ADAPTER_DIR/cmd/tokenless.ts"
MOD_LINK="$MODS_DIR/tokenless.ts"

if bash "$DETECT_SH" >/dev/null 2>&1; then
    detect_rc=0
else
    detect_rc=$?
fi
if [ "$detect_rc" -eq 1 ]; then
    pass "detect reports an installable Command Code mod"
else
    fail "detect returned $detect_rc before installation"
fi

if bash "$INSTALL_SH" >/dev/null; then
    pass "Command Code mod installation succeeds"
else
    fail "Command Code mod installation failed"
fi
if [ -L "$MOD_LINK" ] && [ "$(readlink "$MOD_LINK")" = "$MOD_SOURCE" ]; then
    pass "installer creates the expected user-scope mod link"
else
    fail "installer did not create the managed mod link"
fi
if bash "$DETECT_SH" >/dev/null; then
    pass "detect recognizes the installed mod"
else
    fail "detect did not recognize the installed mod"
fi
if bash "$INSTALL_SH" >/dev/null; then
    pass "Command Code mod installation is idempotent"
else
    fail "repeated Command Code mod installation failed"
fi

# Command Code discovers mods through ~/.commandcode/mods, so the link must
# resolve to a real file: a dangling link would surface as a load warning.
if [ -f "$MOD_LINK" ]; then
    pass "the installed mod link resolves to a readable file"
else
    fail "the installed mod link does not resolve"
fi

ADAPTER_ALIAS="$SANDBOX/adapter alias"
ln -s "$ADAPTER_DIR" "$ADAPTER_ALIAS"
export ANOLISA_ADAPTER_DIR="$ADAPTER_ALIAS"
if bash "$DETECT_SH" >/dev/null; then
    pass "detect recognizes the mod through an adapter symlink"
else
    fail "detect rejected an equivalent adapter symlink"
fi
if bash "$INSTALL_SH" >/dev/null; then
    pass "installer recognizes the mod through an adapter symlink"
else
    fail "installer rejected an equivalent adapter symlink"
fi

if bash "$UNINSTALL_SH" >/dev/null; then
    pass "Command Code mod uninstallation succeeds through an adapter symlink"
else
    fail "Command Code mod uninstallation failed"
fi
export ANOLISA_ADAPTER_DIR="$ADAPTER_DIR"
if [ ! -e "$MOD_LINK" ] && [ ! -L "$MOD_LINK" ]; then
    pass "uninstaller removes the managed mod link"
else
    fail "uninstaller left the managed mod link behind"
fi
if bash "$UNINSTALL_SH" >/dev/null; then
    pass "Command Code mod uninstallation is idempotent"
else
    fail "repeated Command Code mod uninstallation failed"
fi

mkdir -p "$(dirname "$MOD_LINK")"
printf '%s\n' 'export default function foreign() {}' > "$MOD_LINK"
foreign_before="$(cksum "$MOD_LINK")"
if bash "$INSTALL_SH" >/dev/null 2>&1; then
    fail "installer replaced an unmanaged mod file"
else
    pass "installer refuses to replace an unmanaged mod file"
fi
if bash "$UNINSTALL_SH" >/dev/null 2>&1; then
    pass "uninstaller leaves an unmanaged mod file without failing"
else
    fail "uninstaller failed for an unmanaged mod file"
fi
if [ "$(cksum "$MOD_LINK")" = "$foreign_before" ]; then
    pass "unmanaged mod file remains unchanged"
else
    fail "unmanaged mod file was modified"
fi

# A dry-run uninstall must preview the removal and leave the live link
# in place, mirroring the installer's ANOLISA_DRY_RUN handling.
rm -f "$MOD_LINK"
bash "$INSTALL_SH" >/dev/null
dry_run_output="$(ANOLISA_DRY_RUN=1 bash "$UNINSTALL_SH" 2>&1)"
if [ $? -eq 0 ] && [ -L "$MOD_LINK" ] && [ "$(readlink "$MOD_LINK")" = "$MOD_SOURCE" ]; then
    pass "dry-run uninstall preserves the managed mod link"
else
    fail "dry-run uninstall removed or damaged the managed mod link"
fi
case "$dry_run_output" in
    *DRY-RUN:*) pass "dry-run uninstall previews the removal" ;;
    *) fail "dry-run uninstall did not print a DRY-RUN preview" ;;
esac
if bash "$UNINSTALL_SH" >/dev/null; then
    pass "real uninstall still removes the managed mod link"
else
    fail "real uninstall stopped working"
fi

# TOKENLESS_CMD_MODS_DIR and COMMANDCODE_HOME are installer-side overrides:
# Command Code scans ~/.commandcode/mods plus the directories listed in its own
# "mods.paths" setting and reads neither variable, so a link placed anywhere
# else is created but never loaded. The scripts must say so instead of
# reporting a plain success the host cannot honour.
OVERRIDE_DIR="$SANDBOX/override root/mods"
export TOKENLESS_CMD_MODS_DIR="$OVERRIDE_DIR"
override_out="$(bash "$INSTALL_SH" 2>&1)" && override_rc=0 || override_rc=$?
if [ "$override_rc" -eq 0 ] && [ -L "$OVERRIDE_DIR/tokenless.ts" ]; then
    pass "installer honours TOKENLESS_CMD_MODS_DIR"
else
    fail "installer did not create the overridden mod link (rc=$override_rc)"
fi
case "$override_out" in
    *"Command Code scans"*)
        pass "installer warns that an overridden mods directory is not host-scanned" ;;
    *)
        fail "installer reported the overridden mods directory without a warning" ;;
esac
case "$override_out" in
    *"mods.paths"*)
        pass "installer warning names the setting that activates an override" ;;
    *)
        fail "installer warning did not explain how to activate the override" ;;
esac
if [ ! -e "$MOD_LINK" ] && [ ! -L "$MOD_LINK" ]; then
    pass "an override leaves the host-scanned mods directory untouched"
else
    fail "an override wrote into the host-scanned mods directory"
fi
override_detect_out="$(bash "$DETECT_SH" 2>&1)" && override_detect_rc=0 || override_detect_rc=$?
if [ "$override_detect_rc" -eq 0 ]; then
    pass "detect still reports an installed mod under an override"
else
    fail "detect returned $override_detect_rc for the overridden mod link"
fi
case "$override_detect_out" in
    *"Command Code scans"*)
        pass "detect warns that the overridden mods directory is not host-scanned" ;;
    *)
        fail "detect reported the overridden mod link without a warning" ;;
esac
bash "$UNINSTALL_SH" >/dev/null
unset TOKENLESS_CMD_MODS_DIR

# COMMANDCODE_HOME resolves the same way and carries the same caveat: on its own
# it moves the link without activating the mod.
COMMANDCODE_HOME_DIR="$SANDBOX/commandcode home"
export COMMANDCODE_HOME="$COMMANDCODE_HOME_DIR"
cmd_home_out="$(bash "$INSTALL_SH" 2>&1)" && cmd_home_rc=0 || cmd_home_rc=$?
if [ "$cmd_home_rc" -eq 0 ] && [ -L "$COMMANDCODE_HOME_DIR/mods/tokenless.ts" ]; then
    pass "installer honours COMMANDCODE_HOME"
else
    fail "installer did not create the COMMANDCODE_HOME mod link (rc=$cmd_home_rc)"
fi
case "$cmd_home_out" in
    *"Command Code scans"*)
        pass "installer warns that COMMANDCODE_HOME alone does not activate the mod" ;;
    *)
        fail "installer reported COMMANDCODE_HOME without a warning" ;;
esac
bash "$UNINSTALL_SH" >/dev/null
unset COMMANDCODE_HOME

# Without an override the link lands where the host scans it, so nothing warns.
host_out="$(bash "$INSTALL_SH" 2>&1)" && host_rc=0 || host_rc=$?
if [ "$host_rc" -eq 0 ] && [ -L "$MOD_LINK" ]; then
    pass "the host-scanned install still succeeds without overrides"
else
    fail "the host-scanned install failed (rc=$host_rc)"
fi
case "$host_out" in
    *WARNING*) fail "the host-scanned install warned without reason" ;;
    *) pass "the host-scanned install prints no mods-directory warning" ;;
esac

# Without a cmd binary the installer is a no-op and detect reports "absent",
# so a machine that never installed Command Code is left untouched.
rm -f "$FAKE_BIN/cmd"
if bash "$INSTALL_SH" >/dev/null 2>&1; then
    pass "installer skips cleanly when the cmd CLI is absent"
else
    fail "installer failed when the cmd CLI is absent"
fi
if bash "$DETECT_SH" >/dev/null 2>&1; then
    fail "detect reported a usable Command Code without the cmd CLI"
else
    detect_absent_rc=$?
    if [ "$detect_absent_rc" -eq 2 ]; then
        pass "detect exits 2 when the cmd CLI is absent"
    else
        fail "detect returned $detect_absent_rc instead of 2 without the cmd CLI"
    fi
fi

echo ""
echo "Command Code adapter tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
