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
# Never let the suite reach a real anolisa installation: the installer delegates
# a receipt-owned removal to `anolisa adapter disable` when it can find the CLI.
export ANOLISA_BIN="$SANDBOX/no-anolisa"

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

# -- Upgrade state: a registration made while the old host was supported --------
# The adapter source behind the link is replaced in place (the npm postinstall
# swaps the shared adapter tree), so an already installed OpenCode 1.0.0-1.3.3
# host would load the new object default export and abort plugin initialization
# on every start. A fresh install has no link and cannot reach this path.
mkdir -p "$(dirname "$PLUGIN_LINK")"
ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"

detect_rc=0
detect_out="$(TOKENLESS_TEST_OPENCODE_VERSION=1.2.3 bash "$DETECT_SH" 2>&1)" || detect_rc=$?
if [ "$detect_rc" -eq 2 ]; then
    pass "detect reports the unsupported host"
else
    fail "detect returned ${detect_rc} for an unsupported host with a stale link"
fi
case "$detect_out" in
    *"unsupported registration still present"*)
        pass "detect names the stale registration on an unsupported host" ;;
    *) fail "detect did not name the stale registration" ;;
esac
if [ -L "$PLUGIN_LINK" ]; then
    pass "detect leaves the removal to the installer"
else
    fail "detect removed the registration itself"
fi

dry_out="$(ANOLISA_DRY_RUN=1 TOKENLESS_TEST_OPENCODE_VERSION=1.2.3 bash "$INSTALL_SH" 2>&1)"
case "$dry_out" in
    *"DRY-RUN: rm"*) dry_previewed=1 ;;
    *) dry_previewed=0 ;;
esac
if [ "$dry_previewed" -eq 1 ] && [ -L "$PLUGIN_LINK" ]; then
    pass "dry-run previews the removal and keeps the stale registration"
else
    fail "dry-run did not preview the stale registration removal"
fi

install_rc=0
install_out="$(TOKENLESS_TEST_OPENCODE_VERSION=1.2.3 bash "$INSTALL_SH" 2>&1)" || install_rc=$?
if [ "$install_rc" -eq 0 ] && [ ! -e "$PLUGIN_LINK" ] && [ ! -L "$PLUGIN_LINK" ]; then
    pass "installer clears the stale registration on an unsupported host"
else
    fail "installer left the stale registration (rc=${install_rc})"
fi
case "$install_out" in
    *"removed the now-unsupported registration"*)
        pass "installer reports why the registration was removed" ;;
    *) fail "installer did not report the removal" ;;
esac
if [ -f "$PLUGIN_SOURCE" ]; then
    pass "clearing the registration leaves the adapter source intact"
else
    fail "installer deleted the adapter plugin source"
fi

# A file this adapter did not create is never removed, even on an old host.
printf '%s\n' 'export const ForeignPlugin = async () => ({})' > "$PLUGIN_LINK"
foreign_cksum="$(cksum "$PLUGIN_LINK")"
TOKENLESS_TEST_OPENCODE_VERSION=1.2.3 bash "$INSTALL_SH" >/dev/null 2>&1 || true
if [ "$(cksum "$PLUGIN_LINK")" = "$foreign_cksum" ]; then
    pass "installer leaves an unmanaged file alone on an unsupported host"
else
    fail "installer removed or modified an unmanaged file"
fi
rm -f "$PLUGIN_LINK"

# A supported host keeps the registration it already has.
ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"
if TOKENLESS_TEST_OPENCODE_VERSION=2.0.29 bash "$INSTALL_SH" >/dev/null 2>&1 &&
    [ -L "$PLUGIN_LINK" ] && [ "$(readlink "$PLUGIN_LINK")" = "$PLUGIN_SOURCE" ]; then
    pass "installer keeps a valid registration on a supported host"
else
    fail "installer disturbed a valid registration on a supported host"
fi
rm -f "$PLUGIN_LINK"

# -- Reconcile mode: the package postinstall contract ---------------------------
# An npm-owned upgrade replaces the shared adapter tree in place, so the plugin
# link ends up pointing at the new module without the installer ever running.
# The postinstall therefore re-runs the installer in reconcile-only mode, which
# must clear a registration the floor invalidated and must never create one.
rm -f "$PLUGIN_LINK"
recon_rc=0
ANOLISA_ADAPTER_RECONCILE=1 TOKENLESS_TEST_OPENCODE_VERSION=2.0.29 \
    bash "$INSTALL_SH" >/dev/null 2>&1 || recon_rc=$?
if [ "$recon_rc" -eq 0 ] && [ ! -e "$PLUGIN_LINK" ] && [ ! -L "$PLUGIN_LINK" ]; then
    pass "reconcile mode never registers a host that has no link"
else
    fail "reconcile mode created a registration (rc=${recon_rc})"
fi

ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"
recon_rc=0
ANOLISA_ADAPTER_RECONCILE=1 TOKENLESS_TEST_OPENCODE_VERSION=2.0.29 \
    bash "$INSTALL_SH" >/dev/null 2>&1 || recon_rc=$?
if [ "$recon_rc" -eq 0 ] && [ -L "$PLUGIN_LINK" ] &&
    [ "$(readlink "$PLUGIN_LINK")" = "$PLUGIN_SOURCE" ]; then
    pass "reconcile mode keeps a valid registration on a supported host"
else
    fail "reconcile mode disturbed a valid registration (rc=${recon_rc})"
fi
rm -f "$PLUGIN_LINK"

ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"
recon_rc=0
ANOLISA_ADAPTER_RECONCILE=1 TOKENLESS_TEST_OPENCODE_VERSION=1.2.3 \
    bash "$INSTALL_SH" >/dev/null 2>&1 || recon_rc=$?
if [ "$recon_rc" -eq 0 ] && [ ! -e "$PLUGIN_LINK" ] && [ ! -L "$PLUGIN_LINK" ]; then
    pass "reconcile mode clears the stale registration on an unsupported host"
else
    fail "reconcile mode left the stale registration (rc=${recon_rc})"
fi

ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"
recon_dry="$(ANOLISA_DRY_RUN=1 ANOLISA_ADAPTER_RECONCILE=1 \
    TOKENLESS_TEST_OPENCODE_VERSION=1.2.3 bash "$INSTALL_SH" 2>&1)"
case "$recon_dry" in
    *"DRY-RUN: rm"*) recon_previewed=1 ;;
    *) recon_previewed=0 ;;
esac
if [ "$recon_previewed" -eq 1 ] && [ -L "$PLUGIN_LINK" ]; then
    pass "reconcile mode honours ANOLISA_DRY_RUN"
else
    fail "reconcile mode ignored ANOLISA_DRY_RUN"
fi
rm -f "$PLUGIN_LINK"

printf '%s\n' 'export const ForeignPlugin = async () => ({})' > "$PLUGIN_LINK"
recon_cksum="$(cksum "$PLUGIN_LINK")"
recon_rc=0
ANOLISA_ADAPTER_RECONCILE=1 TOKENLESS_TEST_OPENCODE_VERSION=2.0.29 \
    bash "$INSTALL_SH" >/dev/null 2>&1 || recon_rc=$?
if [ "$recon_rc" -eq 0 ] && [ "$(cksum "$PLUGIN_LINK")" = "$recon_cksum" ]; then
    pass "reconcile mode leaves an unmanaged file alone without failing"
else
    fail "reconcile mode touched or rejected an unmanaged file (rc=${recon_rc})"
fi
rm -f "$PLUGIN_LINK"

# -- Receipt-owned registrations: the installer must not silently desync one ----
# `anolisa adapter enable` records the link in a receipt this installer can
# neither read nor write. It therefore hands the removal to the CLI that owns
# both, and only removes the link itself when that is unavailable or fails --
# saying so, instead of implying a receipt protection it does not enforce.
ANOLISA_CALLS="$SANDBOX/anolisa-calls.log"
cat > "$FAKE_BIN/anolisa" <<'STUBEOF'
#!/usr/bin/env bash
echo "$*" >> "$TOKENLESS_TEST_ANOLISA_CALLS"
if [ -n "${TOKENLESS_TEST_ANOLISA_FAIL:-}" ]; then
    exit 1
fi
# A receipt-consistent disable removes the link together with the claim.
rm -f "$TOKENLESS_TEST_PLUGIN_LINK"
exit 0
STUBEOF
chmod +x "$FAKE_BIN/anolisa"

# The CLI resolves its config directory from OPENCODE_CONFIG_DIR and never reads
# TOKENLESS_OPENCODE_CONFIG_DIR, so a delegation is only safe when both name the
# same directory. The shared case sets both; the divergent case sets only the
# installer's own override.
run_installer() {
    cli_rc=0
    env "$@" bash "$INSTALL_SH" > "$SANDBOX/out.txt" 2> "$SANDBOX/err.txt" || cli_rc=$?
}

: > "$ANOLISA_CALLS"
ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"
run_installer "ANOLISA_BIN=$FAKE_BIN/anolisa" "OPENCODE_CONFIG_DIR=$CONFIG_DIR" \
    "TOKENLESS_TEST_PLUGIN_LINK=$PLUGIN_LINK" "TOKENLESS_TEST_ANOLISA_CALLS=$ANOLISA_CALLS" \
    "TOKENLESS_TEST_OPENCODE_VERSION=1.2.3"
if [ "$cli_rc" -eq 0 ] && [ ! -L "$PLUGIN_LINK" ] &&
    grep -qx 'adapter disable tokenless opencode' "$ANOLISA_CALLS" &&
    ! grep -q "partial cleanup" "$SANDBOX/out.txt"; then
    pass "installer delegates a receipt-owned removal to the anolisa CLI"
else
    fail "installer did not delegate to the anolisa CLI (rc=${cli_rc})"
fi

# A divergent config directory must not delegate: the CLI would clear whatever
# link its own receipt records, which is a different, still working registration.
: > "$ANOLISA_CALLS"
ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"
run_installer "ANOLISA_BIN=$FAKE_BIN/anolisa" "OPENCODE_CONFIG_DIR=$SANDBOX/other config" \
    "TOKENLESS_TEST_PLUGIN_LINK=$PLUGIN_LINK" "TOKENLESS_TEST_ANOLISA_CALLS=$ANOLISA_CALLS" \
    "TOKENLESS_TEST_OPENCODE_VERSION=1.2.3"
if [ "$cli_rc" -eq 0 ] && [ ! -L "$PLUGIN_LINK" ] && [ ! -s "$ANOLISA_CALLS" ]; then
    pass "installer does not delegate when another config directory is selected"
else
    fail "installer delegated across config directories (rc=${cli_rc})"
fi
if grep -q "partial cleanup" "$SANDBOX/out.txt" &&
    grep -q "anolisa adapter disable tokenless opencode" "$SANDBOX/out.txt"; then
    pass "the unreconciled receipt is reported on stdout, where callers capture it"
else
    fail "the receipt-recovery notice did not reach stdout"
fi
if grep -q "partial cleanup" "$SANDBOX/err.txt"; then
    fail "the receipt-recovery notice is still stderr-only"
else
    pass "the receipt-recovery notice does not depend on stderr"
fi

# Two config directories whose links point at the *same* shared bundle: the
# registration paths differ, so a run scoped to one must not clear the other.
# Resolving the link itself would canonicalize both to the one plugin.js and let
# the delegation reach a registration this run never selected.
OTHER_CONFIG_DIR="$SANDBOX/other config root/opencode"
OTHER_LINK="$OTHER_CONFIG_DIR/plugins/tokenless.js"
mkdir -p "$OTHER_CONFIG_DIR/plugins"
: > "$ANOLISA_CALLS"
ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"
ln -s "$PLUGIN_SOURCE" "$OTHER_LINK"
run_installer "ANOLISA_BIN=$FAKE_BIN/anolisa" "OPENCODE_CONFIG_DIR=$OTHER_CONFIG_DIR" \
    "TOKENLESS_TEST_PLUGIN_LINK=$OTHER_LINK" "TOKENLESS_TEST_ANOLISA_CALLS=$ANOLISA_CALLS" \
    "TOKENLESS_TEST_OPENCODE_VERSION=1.2.3"
if [ "$cli_rc" -eq 0 ] && [ ! -s "$ANOLISA_CALLS" ] &&
    [ ! -L "$PLUGIN_LINK" ] && [ -L "$OTHER_LINK" ]; then
    pass "a shared bundle does not make two registrations the same path"
else
    fail "installer cleared a registration in another config directory (rc=${cli_rc} calls=$(wc -l < "$ANOLISA_CALLS"))"
fi
rm -f "$OTHER_LINK"

: > "$ANOLISA_CALLS"
ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"
run_installer "ANOLISA_BIN=$FAKE_BIN/anolisa" "OPENCODE_CONFIG_DIR=$CONFIG_DIR" \
    "TOKENLESS_TEST_ANOLISA_FAIL=1" "TOKENLESS_TEST_PLUGIN_LINK=$PLUGIN_LINK" \
    "TOKENLESS_TEST_ANOLISA_CALLS=$ANOLISA_CALLS" "TOKENLESS_TEST_OPENCODE_VERSION=1.2.3"
if [ "$cli_rc" -eq 0 ] && [ ! -L "$PLUGIN_LINK" ] &&
    grep -q "anolisa adapter disable tokenless opencode" "$SANDBOX/out.txt"; then
    pass "installer removes the link itself and names the stale receipt when the CLI fails"
else
    fail "installer did not fall back when the CLI failed (rc=${cli_rc})"
fi

ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"
run_installer "ANOLISA_BIN=$SANDBOX/no-anolisa" "TOKENLESS_TEST_OPENCODE_VERSION=1.2.3"
if [ "$cli_rc" -eq 0 ] && [ ! -L "$PLUGIN_LINK" ] &&
    grep -q "partial cleanup" "$SANDBOX/out.txt"; then
    pass "installer reports the receipt boundary when no anolisa CLI is available"
else
    fail "installer did not report the receipt boundary (rc=${cli_rc})"
fi

ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"
: > "$ANOLISA_CALLS"
run_installer "ANOLISA_DRY_RUN=1" "ANOLISA_BIN=$FAKE_BIN/anolisa" \
    "OPENCODE_CONFIG_DIR=$CONFIG_DIR" "TOKENLESS_TEST_PLUGIN_LINK=$PLUGIN_LINK" \
    "TOKENLESS_TEST_ANOLISA_CALLS=$ANOLISA_CALLS" "TOKENLESS_TEST_OPENCODE_VERSION=1.2.3"
if [ -L "$PLUGIN_LINK" ] && [ ! -s "$ANOLISA_CALLS" ]; then
    pass "dry-run invokes the CLI nowhere and keeps the registration"
else
    fail "dry-run mutated state or invoked the CLI"
fi
rm -f "$PLUGIN_LINK"

echo ""
echo "OpenCode adapter tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
