#!/usr/bin/env bash
# Regression tests for the QwenPaw plugin install scripts.
set -uo pipefail

PASS=0
FAIL=0

pass() { echo "[PASS] $1"; PASS=$((PASS + 1)); }
fail() { echo "[FAIL] $1" >&2; FAIL=$((FAIL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless"
SANDBOX="$(mktemp -d -t tokenless-qwenpaw-install-test.XXXXXX)"
trap 'rm -rf "$SANDBOX"' EXIT

FAKE_HOME="$SANDBOX/home"
ADAPTER_DIR="$SANDBOX/adapter root"
QWENPAW_STUB="$FAKE_HOME/.qwenpaw/bin/qwenpaw"
STUB_LOG="$FAKE_HOME/.qwenpaw/stub.log"
PLUGIN_DST="$FAKE_HOME/.qwenpaw/plugins/tokenless"
mkdir -p "$FAKE_HOME/.qwenpaw/bin" "$ADAPTER_DIR" "$SANDBOX/emptybin"
cp -R "$SOURCE_ADAPTER_DIR"/. "$ADAPTER_DIR"/

# Source checkouts contain the version templates; packages contain the
# stamped files. Stamp only the sandbox copy so this test never modifies the tree.
for template in plugin.json requirements.txt; do
    sed 's/@VERSION@/0.0.0-test/g' "$ADAPTER_DIR/qwenpaw/$template.in" > "$ADAPTER_DIR/qwenpaw/$template"
done

cat > "$QWENPAW_STUB" <<'STUBEOF'
#!/usr/bin/env bash
# Mimics `qwenpaw plugin ...`: copies the bundle, prompts on uninstall, and
# exits 0 even on failure, exactly like the real CLI.
set -euo pipefail
log="$HOME/.qwenpaw/stub.log"
printf '%s\n' "$*" >> "$log"
printf 'WORKING_DIR=%s\n' "${QWENPAW_WORKING_DIR:-unset}" >> "$log"
plugins="${QWENPAW_WORKING_DIR:-$HOME/.qwenpaw}/plugins"

if [ "${1:-}" = "plugin" ] && [ "${2:-}" = "install" ]; then
    src="${3:?plugin path required}"
    if [ "${QWENPAW_STUB_FAIL_INSTALL:-0}" = "1" ]; then
        echo "❌ Failed to install plugin: simulated" >&2
        exit 0
    fi
    dst="$plugins/tokenless"
    if [ -d "$dst" ] && [ "${4:-}" != "--force" ]; then
        echo "❌ Plugin 'tokenless' already installed. Use --force to overwrite." >&2
        exit 0
    fi
    rm -rf "$dst"
    mkdir -p "$plugins"
    cp -R "$src" "$dst"
    exit 0
fi

if [ "${1:-}" = "plugin" ] && [ "${2:-}" = "uninstall" ]; then
    [ "${3:-}" = "tokenless" ]
    read -r answer || answer=""
    [ "$answer" = "y" ] || exit 1
    if [ "${QWENPAW_STUB_FAIL_UNINSTALL:-0}" = "1" ]; then
        echo "❌ Failed to uninstall plugin: simulated" >&2
        exit 0
    fi
    rm -rf "$plugins/tokenless"
    exit 0
fi

echo "unsupported qwenpaw invocation: $*" >&2
exit 2
STUBEOF
chmod +x "$QWENPAW_STUB"

DETECT_SH="$ADAPTER_DIR/qwenpaw/scripts/detect.sh"
INSTALL_SH="$ADAPTER_DIR/qwenpaw/scripts/install.sh"
UNINSTALL_SH="$ADAPTER_DIR/qwenpaw/scripts/uninstall.sh"

run() { HOME="$FAKE_HOME" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" ANOLISA_SKIP_WHEEL_PREFLIGHT=1 "$@"; }
# Same harness with the wheel preflight armed; every case below pins the probe
# result through a stub, so the suite never reaches the network.
# QWENPAW_HOME is pinned because install.sh prepends `${QWENPAW_HOME:-$HOME/.qwenpaw}/bin`
# to PATH; an ambient value would add a directory this harness does not control.
run_probe() {
    HOME="$FAKE_HOME" QWENPAW_HOME="$FAKE_HOME/.qwenpaw" \
        ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" ANOLISA_SKIP_WHEEL_PREFLIGHT=0 "$@"
}

run bash "$DETECT_SH" >/dev/null 2>&1
if [ "$?" -eq 1 ]; then
    pass "detect reports installable before installation"
else
    fail "detect did not report installable before installation"
fi

if run bash "$INSTALL_SH" >/dev/null; then
    pass "plugin installation succeeds"
else
    fail "plugin installation failed"
fi

if [ -f "$PLUGIN_DST/plugin.json" ] && [ -f "$PLUGIN_DST/requirements.txt" ]; then
    pass "qwenpaw copied the stamped bundle into its plugins directory"
else
    fail "installed bundle is missing plugin.json or requirements.txt"
fi

if grep -Fqx "plugin install $ADAPTER_DIR/qwenpaw --force" "$STUB_LOG" && \
        grep -Fqx "WORKING_DIR=$FAKE_HOME/.qwenpaw" "$STUB_LOG"; then
    pass "installer hands the bundle root to qwenpaw with the working directory set"
else
    fail "installer used an unexpected qwenpaw invocation"
fi

if run bash "$DETECT_SH" >/dev/null; then
    pass "detect reports ready after installation"
else
    fail "detect did not report ready after installation"
fi

if run bash "$INSTALL_SH" >/dev/null; then
    pass "plugin reinstallation is idempotent"
else
    fail "plugin reinstallation failed"
fi

rm -rf "$PLUGIN_DST"
if run env QWENPAW_STUB_FAIL_INSTALL=1 bash "$INSTALL_SH" >/dev/null 2>&1; then
    fail "installer trusted a zero exit status without an installed plugin"
else
    pass "installer fails when qwenpaw exits 0 without installing"
fi

if run env ANOLISA_DRY_RUN=1 bash "$INSTALL_SH" | grep -q '^DRY-RUN: ' && [ ! -d "$PLUGIN_DST" ]; then
    pass "dry-run install prints the command and changes nothing"
else
    fail "dry-run install misbehaved"
fi

run bash "$INSTALL_SH" >/dev/null || fail "failed to reinstall plugin for uninstall coverage"

if run bash "$UNINSTALL_SH" >/dev/null && [ ! -d "$PLUGIN_DST" ] && \
        grep -Fqx "plugin uninstall tokenless" "$STUB_LOG"; then
    pass "uninstaller confirms through qwenpaw and removes the plugin directory"
else
    fail "uninstaller did not remove the plugin through qwenpaw"
fi

if run bash "$UNINSTALL_SH" >/dev/null; then
    pass "repeated uninstallation succeeds"
else
    fail "repeated uninstallation failed"
fi

run bash "$INSTALL_SH" >/dev/null || fail "failed to reinstall plugin for stale-bundle coverage"
printf 'plugin = "previous"\n' > "$PLUGIN_DST/plugin.py"
if run env QWENPAW_STUB_FAIL_INSTALL=1 bash "$INSTALL_SH" >/dev/null 2>&1; then
    fail "installer accepted the previous bundle left behind by a failed reinstall"
else
    pass "installer fails when a failed reinstall leaves the previous bundle in place"
fi
run bash "$UNINSTALL_SH" >/dev/null || fail "failed to uninstall plugin before working-directory coverage"

COPAW_ENV_DIR="$SANDBOX/copaw-env"
if run env COPAW_WORKING_DIR="$COPAW_ENV_DIR" bash "$INSTALL_SH" >/dev/null && \
        [ -f "$COPAW_ENV_DIR/plugins/tokenless/plugin.json" ] && [ ! -d "$PLUGIN_DST" ]; then
    pass "installer honors COPAW_WORKING_DIR"
else
    fail "installer ignored COPAW_WORKING_DIR"
fi
if run env COPAW_WORKING_DIR="$COPAW_ENV_DIR" bash "$DETECT_SH" >/dev/null; then
    pass "detect honors COPAW_WORKING_DIR"
else
    fail "detect ignored COPAW_WORKING_DIR"
fi
if run env COPAW_WORKING_DIR="$COPAW_ENV_DIR" bash "$UNINSTALL_SH" >/dev/null && \
        [ ! -d "$COPAW_ENV_DIR/plugins/tokenless" ]; then
    pass "uninstaller honors COPAW_WORKING_DIR"
else
    fail "uninstaller ignored COPAW_WORKING_DIR"
fi

mkdir -p "$FAKE_HOME/.copaw"
if run bash "$INSTALL_SH" >/dev/null && \
        [ -f "$FAKE_HOME/.copaw/plugins/tokenless/plugin.json" ] && [ ! -d "$PLUGIN_DST" ]; then
    pass "installer uses a legacy ~/.copaw working directory"
else
    fail "installer ignored a legacy ~/.copaw working directory"
fi
if run bash "$DETECT_SH" >/dev/null; then
    pass "detect uses a legacy ~/.copaw working directory"
else
    fail "detect ignored a legacy ~/.copaw working directory"
fi
if run bash "$UNINSTALL_SH" >/dev/null && [ ! -d "$FAKE_HOME/.copaw/plugins/tokenless" ]; then
    pass "uninstaller uses a legacy ~/.copaw working directory"
else
    fail "uninstaller ignored a legacy ~/.copaw working directory"
fi
rm -rf "$FAKE_HOME/.copaw"

QP_ENV_DIR="$SANDBOX/qwenpaw-env"
if run env QWENPAW_WORKING_DIR="$QP_ENV_DIR" COPAW_WORKING_DIR="$COPAW_ENV_DIR" bash "$INSTALL_SH" >/dev/null && \
        [ -f "$QP_ENV_DIR/plugins/tokenless/plugin.json" ] && [ ! -d "$COPAW_ENV_DIR/plugins/tokenless" ]; then
    pass "QWENPAW_WORKING_DIR takes precedence over COPAW_WORKING_DIR"
else
    fail "COPAW_WORKING_DIR overrode QWENPAW_WORKING_DIR"
fi
run env QWENPAW_WORKING_DIR="$QP_ENV_DIR" bash "$UNINSTALL_SH" >/dev/null || fail "failed to uninstall plugin from QWENPAW_WORKING_DIR"
mkdir -p "$FAKE_HOME/.copaw"
if run env QWENPAW_WORKING_DIR="$QP_ENV_DIR" bash "$INSTALL_SH" >/dev/null && \
        [ -f "$QP_ENV_DIR/plugins/tokenless/plugin.json" ] && [ ! -d "$FAKE_HOME/.copaw/plugins/tokenless" ]; then
    pass "QWENPAW_WORKING_DIR takes precedence over a legacy ~/.copaw"
else
    fail "a legacy ~/.copaw overrode QWENPAW_WORKING_DIR"
fi
run env QWENPAW_WORKING_DIR="$QP_ENV_DIR" bash "$UNINSTALL_SH" >/dev/null || fail "failed to uninstall plugin from QWENPAW_WORKING_DIR"
rm -rf "$FAKE_HOME/.copaw"

# ~/.copaw appearing after installation must not hide the plugin in ~/.qwenpaw.
run bash "$INSTALL_SH" >/dev/null || fail "failed to reinstall plugin for working-directory drift coverage"
mkdir -p "$FAKE_HOME/.copaw"
run bash "$DETECT_SH" >"$SANDBOX/detect-drift.out" 2>&1
if grep -q "installed ($PLUGIN_DST)" "$SANDBOX/detect-drift.out"; then
    pass "detect finds the plugin in ~/.qwenpaw after ~/.copaw appears"
else
    fail "detect lost the plugin in ~/.qwenpaw after ~/.copaw appeared"
fi
if run bash "$UNINSTALL_SH" >/dev/null && [ ! -d "$PLUGIN_DST" ] && \
        [ "$(grep '^WORKING_DIR=' "$STUB_LOG" | tail -n1)" = "WORKING_DIR=$FAKE_HOME/.qwenpaw" ]; then
    pass "uninstaller removes the plugin from ~/.qwenpaw after ~/.copaw appears"
else
    fail "uninstaller missed the plugin in ~/.qwenpaw after ~/.copaw appeared"
fi
if run bash "$UNINSTALL_SH" | grep -q 'no tokenless plugin is installed'; then
    pass "uninstaller reports where it looked when nothing is installed"
else
    fail "uninstaller claimed success although nothing was installed"
fi
rm -rf "$FAKE_HOME/.copaw"

run bash "$INSTALL_SH" >/dev/null || fail "failed to reinstall plugin for stale-detect coverage"
printf 'plugin = "previous"\n' > "$PLUGIN_DST/plugin.py"
run bash "$DETECT_SH" >"$SANDBOX/detect-stale.out" 2>&1
if [ "$?" -eq 1 ] && grep -q 'stale (' "$SANDBOX/detect-stale.out"; then
    pass "detect reports an installed bundle that differs from the source as stale"
else
    fail "detect did not report the stale bundle"
fi
run bash "$UNINSTALL_SH" >/dev/null || fail "failed to uninstall plugin after stale-detect coverage"

run bash "$INSTALL_SH" >/dev/null || fail "failed to reinstall plugin for uninstall-failure coverage"
if run env QWENPAW_STUB_FAIL_UNINSTALL=1 bash "$UNINSTALL_SH" >/dev/null 2>&1; then
    fail "uninstaller reported success although qwenpaw left the plugin installed"
else
    [ -f "$PLUGIN_DST/plugin.json" ] && pass "uninstaller fails and keeps the directory when qwenpaw does not unload the plugin" \
        || fail "uninstaller removed the directory although qwenpaw did not unload the plugin"
fi
run bash "$UNINSTALL_SH" >/dev/null || fail "failed to uninstall plugin after uninstall-failure coverage"

# The stub CLI has a bash shebang, so the installer cannot find QwenPaw's
# Python and leaves the SDK unverified; QWENPAW_PYTHON plus a fake package
# exercises both verdicts.
mkdir -p "$SANDBOX/sdk-ok/anolisa_tokenless" "$SANDBOX/sdk-old/anolisa_tokenless"
printf '__version__ = "0.0.0-test"\nRecoveryMethod = object\n' > "$SANDBOX/sdk-ok/anolisa_tokenless/__init__.py"
printf '__version__ = "0.0.0-old"\n' > "$SANDBOX/sdk-old/anolisa_tokenless/__init__.py"
PYTHON3="$(command -v python3)"
if run bash "$INSTALL_SH" 2>&1 | grep -q 'import left unverified'; then
    pass "installer reports an unverified SDK when the CLI has no Python shebang"
else
    fail "installer did not report the unverified SDK"
fi
if run env QWENPAW_PYTHON="$PYTHON3" PYTHONPATH="$SANDBOX/sdk-ok" bash "$INSTALL_SH" | grep -q 'anolisa_tokenless 0.0.0-test is importable'; then
    pass "installer verifies the SDK through QwenPaw's Python"
else
    fail "installer did not verify the SDK through QwenPaw's Python"
fi
if run env QWENPAW_PYTHON="$PYTHON3" PYTHONPATH="$SANDBOX/sdk-old" bash "$INSTALL_SH" >/dev/null 2>&1; then
    fail "installer accepted a wheel without the required SDK surface"
else
    pass "installer fails when the installed SDK predates the plugin"
fi
if run env QWENPAW_PYTHON="$PYTHON3" PYTHONPATH="$SANDBOX/emptybin" bash "$INSTALL_SH" >/dev/null 2>&1; then
    fail "installer accepted a QwenPaw environment without anolisa_tokenless"
else
    pass "installer fails when anolisa_tokenless is not importable"
fi
run env QWENPAW_PYTHON="$PYTHON3" PYTHONPATH="$SANDBOX/sdk-old" bash "$DETECT_SH" >"$SANDBOX/detect-old.out" 2>&1
if [ "$?" -eq 1 ] && grep -q 'not importable' "$SANDBOX/detect-old.out"; then
    pass "detect reports an SDK that predates the plugin as not ready"
else
    fail "detect did not report the outdated SDK"
fi
if run env QWENPAW_PYTHON="$PYTHON3" PYTHONPATH="$SANDBOX/sdk-ok" bash "$DETECT_SH" | grep -q 'importable (0.0.0-test)'; then
    pass "detect reports the importable SDK version"
else
    fail "detect did not report the importable SDK version"
fi
run bash "$UNINSTALL_SH" >/dev/null || fail "failed to uninstall plugin after SDK coverage"

if run env PATH="$SANDBOX/emptybin:/usr/bin:/bin" QWENPAW_HOME="$SANDBOX/nowhere" \
        bash "$INSTALL_SH" 2>/dev/null | grep -q 'skipping plugin installation' && [ ! -d "$PLUGIN_DST" ]; then
    pass "installer skips without a qwenpaw CLI"
else
    fail "installer did not skip cleanly without a qwenpaw CLI"
fi

run env PATH="$SANDBOX/emptybin:/usr/bin:/bin" QWENPAW_HOME="$SANDBOX/nowhere" \
    bash "$DETECT_SH" >/dev/null 2>&1
if [ "$?" -eq 2 ]; then
    pass "detect reports missing prerequisites without a qwenpaw CLI"
else
    fail "detect did not report missing prerequisites without a qwenpaw CLI"
fi

# --- wheel preflight -------------------------------------------------------
# The bundle pins anolisa_tokenless by GitHub Release URL. When the version
# bump lands before the matching release is published the asset 404s, and
# `qwenpaw plugin install` used to surface that as a bare pip HTTP error. The
# installer now probes the asset for this host first and names the missing tag.
case "$(uname -s)/$(uname -m)" in
    Linux/x86_64 | Linux/aarch64 | Darwin/arm64) PREFLIGHT_PLATFORM=1 ;;
    *) PREFLIGHT_PLATFORM=0 ;;
esac

if [ "$PREFLIGHT_PLATFORM" -eq 0 ]; then
    echo "[SKIP] this platform has no pinned wheel line; preflight not exercised"
else
    PROBE_BIN="$SANDBOX/probebin"
    PY_BIN="$SANDBOX/pybin"
    MIN_BIN="$SANDBOX/minbin"
    PROBE_LOG="$SANDBOX/probe.log"
    # install.sh prepends `${QWENPAW_HOME:-$HOME/.qwenpaw}/bin:$HOME/.local/bin:
    # /usr/local/bin` ahead of whatever PATH this harness passes, so stubs on
    # that PATH alone lose to a real curl/python3 on the host (Homebrew puts
    # both in /usr/local/bin). The first two are sandboxed through HOME and
    # QWENPAW_HOME, so publish the stubs into $HOME/.local/bin as well: install.sh
    # puts that directory ahead of /usr/local/bin, which makes the stub win on
    # every host. Each case below also asserts which tool actually ran.
    SHADOW_BIN="$FAKE_HOME/.local/bin"
    mkdir -p "$PROBE_BIN" "$PY_BIN" "$MIN_BIN" "$SHADOW_BIN"

    # curl wins over python3 inside the installer, so the python3 branch needs
    # a PATH that carries the interpreter stub and the core tools but no curl.
    cat > "$PROBE_BIN/curl" <<'CURLEOF'
#!/usr/bin/env bash
printf '%s\n' "curl $*" >> "${PROBE_LOG:-/dev/null}"
# PROBE_STATUS=none models a probe that reports nothing at all, which is what a
# probe gives up with when it hits its deadline.
status="${PROBE_STATUS:-000}"
[ "$status" = "none" ] || printf '%s' "$status"
CURLEOF
    cat > "$PROBE_BIN/python3" <<'PYSTUBEOF'
#!/usr/bin/env bash
cat > /dev/null
printf '%s\n' "python3 $*" >> "${PROBE_LOG:-/dev/null}"
printf '%s\n' "${PROBE_STATUS:-000}"
PYSTUBEOF
    chmod +x "$PROBE_BIN/curl" "$PROBE_BIN/python3"
    cp "$PROBE_BIN/python3" "$PY_BIN/python3"
    for tool in bash cat cmp cp dirname env grep head mkdir rm sed uname; do
        tool_path="$(command -v "$tool" 2>/dev/null || true)"
        [ -n "$tool_path" ] && ln -sf "$tool_path" "$MIN_BIN/$tool"
    done

    # Empties the shadow directory. Guarded so an unset SHADOW_BIN can never turn
    # this into `rm -f /*`.
    clear_shadow() {
        [ -n "$SHADOW_BIN" ] && rm -f "$SHADOW_BIN"/* 2>/dev/null
        return 0
    }

    probe_case() {  # probe_case <status> [probe-bin]
        local probe_path="$MIN_BIN" stub
        clear_shadow
        if [ -n "${2:-}" ]; then
            probe_path="$2:$MIN_BIN"
            for stub in "$2"/*; do
                [ -f "$stub" ] && cp "$stub" "$SHADOW_BIN/${stub##*/}"
            done
        fi
        rm -rf "$PLUGIN_DST"
        : > "$STUB_LOG"
        rm -f "$PROBE_LOG"
        run_probe env PATH="$probe_path" PROBE_STATUS="$1" PROBE_LOG="$PROBE_LOG" \
            bash "$INSTALL_SH" >"$SANDBOX/preflight.out" 2>&1
        echo "$?"
    }

    # Proves the case really ran against the stub it set up: a host tool winning
    # the PATH would leave this log empty (or name the other tool) and the case
    # would fail instead of passing on a coincidental status.
    used_tool() {  # used_tool <curl|python3>
        [ -s "$PROBE_LOG" ] && head -n 1 "$PROBE_LOG" | grep -q "^$1 "
    }

    # /usr/local/bin is the one directory install.sh prepends that this suite
    # cannot shadow: the path is absolute and the suite must not need root.
    # Publishing stubs into $HOME/.local/bin beats it, so every case that needs a
    # probe tool is safe; a case that needs one *absent* cannot be faked there.
    # Ask the installer's own question instead of finding out through a red test
    # that quietly probed the real GitHub.
    host_probe_tool() {  # host_probe_tool <curl|curl-or-python3>
        local probe='command -v curl >/dev/null 2>&1'
        if [ "$1" = "curl-or-python3" ]; then
            probe="$probe || command -v python3 >/dev/null 2>&1"
        fi
        clear_shadow
        env PATH="$FAKE_HOME/.qwenpaw/bin:$SHADOW_BIN:/usr/local/bin:$MIN_BIN" \
            bash -c "$probe"
    }

    status="$(probe_case 404 "$PROBE_BIN")"
    if [ "$status" -ne 0 ] && used_tool curl &&
        grep -q 'wheel asset is unavailable (HTTP 404)' "$SANDBOX/preflight.out" &&
        grep -q 'tokenless/v0\.0\.0-test' "$SANDBOX/preflight.out" &&
        grep -q 'ANOLISA_SKIP_WHEEL_PREFLIGHT=1' "$SANDBOX/preflight.out"; then
        pass "installer names the missing wheel asset and its release tag instead of a bare pip 404"
    else
        fail "installer did not explain the missing wheel asset (rc=$status)"
    fi
    # A 404 on one asset URL proves only that the asset is undownloadable: the
    # publish workflow can leave an existing release with an incomplete asset
    # set. The message must offer both maintainer paths and must not assert that
    # the release itself is absent.
    # The two `.` wildcards stand for the backticks around the release tag.
    if ! grep -q 'does not exist' "$SANDBOX/preflight.out" &&
        grep -q 'push the .tokenless/v0\.0\.0-test. tag' "$SANDBOX/preflight.out" &&
        grep -q 'delete that release and re-run' "$SANDBOX/preflight.out"; then
        pass "installer reports an unavailable asset without claiming the release is missing"
    else
        fail "installer equated an asset 404 with a missing release"
    fi
    if [ ! -d "$PLUGIN_DST" ] && ! grep -q '^plugin install ' "$STUB_LOG"; then
        pass "installer fails before QwenPaw copies a bundle it cannot run"
    else
        fail "installer mutated QwenPaw state for an undownloadable wheel"
    fi

    status="$(probe_case 200 "$PROBE_BIN")"
    if [ "$status" -eq 0 ] && [ -f "$PLUGIN_DST/plugin.json" ] && used_tool curl; then
        pass "installer proceeds when the pinned wheel is published"
    else
        fail "installer blocked a published wheel (rc=$status)"
    fi

    status="$(probe_case 000 "$PROBE_BIN")"
    if [ "$status" -eq 0 ] && [ -f "$PLUGIN_DST/plugin.json" ] && used_tool curl; then
        pass "installer leaves the verdict to pip when the wheel host is unreachable"
    else
        fail "installer treated an unreachable wheel host as undownloadable (rc=$status)"
    fi

    status="$(probe_case 403 "$PROBE_BIN")"
    if [ "$status" -eq 0 ] && [ -f "$PLUGIN_DST/plugin.json" ] && used_tool curl &&
        grep -q 'Could not verify the SDK wheel' "$SANDBOX/preflight.out"; then
        pass "installer warns and proceeds on an inconclusive wheel probe"
    else
        fail "installer mishandled an inconclusive wheel probe (rc=$status)"
    fi

    # An empty answer is not a verdict either: this is what the python3 branch
    # reports once it hits its deadline, so the wheel has to go to pip.
    status="$(probe_case none "$PROBE_BIN")"
    if [ "$status" -eq 0 ] && [ -f "$PLUGIN_DST/plugin.json" ] && used_tool curl &&
        ! grep -q 'wheel asset is unavailable' "$SANDBOX/preflight.out"; then
        pass "installer leaves the verdict to pip when the probe reports no status"
    else
        fail "installer mishandled a probe that reported no status (rc=$status)"
    fi

    # This case needs curl to be *absent* so the installer falls back to python3.
    if host_probe_tool curl; then
        echo "[SKIP] a real curl in /usr/local/bin, which install.sh prepends ahead of any"
        echo "[SKIP] PATH this suite passes; the python3-only probe case is not exercised"
    else
        status="$(probe_case 404 "$PY_BIN")"
        if [ "$status" -ne 0 ] && used_tool python3 &&
            grep -q 'wheel asset is unavailable (HTTP 404)' "$SANDBOX/preflight.out"; then
            pass "installer probes the wheel through python3 when curl is absent"
        else
            fail "python3 wheel probe did not report the unavailable asset (rc=$status)"
        fi
    fi

    # This case needs both probe tools to be absent.
    if host_probe_tool curl-or-python3; then
        echo "[SKIP] a real curl/python3 in /usr/local/bin, which install.sh prepends and"
        echo "[SKIP] this suite cannot shadow; the no-probe-tool case is not exercised"
    else
        status="$(probe_case 404 "")"
        if [ "$status" -eq 0 ] && [ -f "$PLUGIN_DST/plugin.json" ] && [ ! -s "$PROBE_LOG" ]; then
            pass "installer skips the probe when no probe tool is available"
        else
            fail "installer required a wheel probe tool (rc=$status)"
        fi
    fi

    # --- python3 probe deadline ---------------------------------------------
    # urlopen's timeout bounds each socket operation, not the whole exchange, so a
    # server that dribbles response headers used to keep the python3 probe busy for
    # many multiples of ANOLISA_TOKENLESS_PROBE_TIMEOUT while curl --max-time gave
    # up on schedule. The shell stubs above cannot cover this: they print a status
    # without doing any I/O. Run the program install.sh actually ships against a
    # slow local server instead -- no external network, no stub in the way.
    REAL_PYTHON3="$(command -v python3 2>/dev/null || true)"
    if [ -z "$REAL_PYTHON3" ]; then
        echo "[SKIP] no python3 on PATH; the probe deadline is not exercised"
    else
        PROBE_PY="$SANDBOX/wheel_probe.py"
        # install.sh carries two `<<'PY'` heredocs; the probe is the one with
        # urlopen. Failing loudly here beats silently testing the wrong program.
        awk '/<<.*PY/{flag=1; buf=""; next}
             /^PY$/{if (flag && buf ~ /urlopen/) printf "%s", buf; flag=0; next}
             flag{buf=buf $0 ORS}' "$INSTALL_SH" > "$PROBE_PY"
        if ! grep -q 'urlopen' "$PROBE_PY"; then
            fail "could not extract the python3 wheel probe program from install.sh"
        else
            SLOW_PY="$SANDBOX/slow_response.py"
            cat > "$SLOW_PY" <<'SLOWEOF'
import socket
import sys
import time

port_file, interval, headers = sys.argv[1], float(sys.argv[2]), int(sys.argv[3])
listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("127.0.0.1", 0))
listener.listen(1)
listener.settimeout(30)  # never hold the suite hostage if no client shows up
with open(port_file, "w") as handle:
    handle.write(str(listener.getsockname()[1]))
try:
    connection, _ = listener.accept()
except socket.timeout:
    sys.exit(0)
connection.recv(65536)
connection.sendall(b"HTTP/1.1 200 OK\r\n")
for index in range(headers):
    time.sleep(interval)
    try:
        connection.sendall(b"X-Dribble-%d: x\r\n" % index)
    except OSError:
        break
try:
    connection.sendall(b"Content-Length: 0\r\n\r\n")
    connection.close()
except OSError:
    pass
SLOWEOF
            TIME_PY="$SANDBOX/time_probe.py"
            cat > "$TIME_PY" <<'TIMEEOF'
import subprocess
import sys
import time

out_path, program, url, timeout = sys.argv[1:5]
started = time.monotonic()
with open(out_path, "wb") as handle:
    subprocess.run([sys.executable, program, url, timeout], stdout=handle,
                   stderr=subprocess.DEVNULL)
print("%.2f" % (time.monotonic() - started))
TIMEEOF
            SLOW_PORT_FILE="$SANDBOX/slow_response.port"
            "$REAL_PYTHON3" "$SLOW_PY" "$SLOW_PORT_FILE" 0.5 12 &
            SLOW_PID=$!
            slow_port=""
            for _ in $(seq 1 50); do
                if [ -s "$SLOW_PORT_FILE" ]; then
                    slow_port="$(cat "$SLOW_PORT_FILE")"
                    break
                fi
                sleep 0.1
            done
            if [ -z "$slow_port" ]; then
                fail "the slow-response server never reported its port"
            else
                # Budget 1s against a server that keeps talking for 6s: a probe
                # that honours the deadline is back near 1s with no verdict, one
                # that does not rides the response to the end and reports 200.
                deadline_out="$SANDBOX/deadline.out"
                elapsed="$("$REAL_PYTHON3" "$TIME_PY" "$deadline_out" "$PROBE_PY" \
                    "http://127.0.0.1:${slow_port}/tokenless/v0.0.0-test/anolisa_tokenless-0.0.0-test-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl" 1)"
                if [ -n "$elapsed" ] && [ ! -s "$deadline_out" ] &&
                    awk -v value="$elapsed" 'BEGIN { exit !(value < 3) }'; then
                    pass "python3 probe stops at ANOLISA_TOKENLESS_PROBE_TIMEOUT on a slow response"
                else
                    fail "python3 probe outlived its deadline (elapsed=${elapsed:-?}s, verdict=$(cat "$deadline_out" 2>/dev/null))"
                fi
            fi
            kill "$SLOW_PID" 2>/dev/null || true
            wait "$SLOW_PID" 2>/dev/null || true
        fi
    fi

    rm -rf "$PLUGIN_DST"
    clear_shadow
    : > "$STUB_LOG"
    rm -f "$PROBE_LOG"
    if run env PATH="$PROBE_BIN:$MIN_BIN" PROBE_STATUS=404 PROBE_LOG="$PROBE_LOG" \
            bash "$INSTALL_SH" >/dev/null 2>&1 &&
        [ -f "$PLUGIN_DST/plugin.json" ] && [ ! -s "$PROBE_LOG" ]; then
        pass "ANOLISA_SKIP_WHEEL_PREFLIGHT=1 keeps an offline mirror installable"
    else
        fail "ANOLISA_SKIP_WHEEL_PREFLIGHT=1 did not bypass the probe"
    fi
    run bash "$UNINSTALL_SH" >/dev/null || fail "failed to uninstall after preflight coverage"
fi

echo ""
echo "QwenPaw adapter tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
