#!/usr/bin/env bash
# install.sh — Install the tokenless plugin into QwenPaw through its own CLI.
#
# `qwenpaw plugin install` copies the bundle into the QwenPaw working
# directory, validates plugin.py, installs requirements.txt into QwenPaw's
# Python environment, and hot-loads when QwenPaw is running. It exits 0 even
# when it fails, so the installed files are compared with the source afterwards.
# requirements.txt pins the SDK wheel to a GitHub Release asset for this exact
# version, so that asset is probed before the bundle is handed over.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-qwenpaw}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
# Resolve the working directory like QwenPaw: QWENPAW_WORKING_DIR, else
# COPAW_WORKING_DIR, else a legacy ~/.copaw, else ~/.qwenpaw.
QWENPAW_WORKING_DIR="${QWENPAW_WORKING_DIR:-${COPAW_WORKING_DIR:-}}"
if [ -z "$QWENPAW_WORKING_DIR" ]; then
    if [ -d "$HOME/.copaw" ]; then QWENPAW_WORKING_DIR="$HOME/.copaw"; else QWENPAW_WORKING_DIR="$HOME/.qwenpaw"; fi
fi
QWENPAW_BIN="${QWENPAW_BIN:-}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"
export PATH="${QWENPAW_HOME:-$HOME/.qwenpaw}/bin:$HOME/.local/bin:/usr/local/bin:$PATH"

PLUGIN_ID="tokenless"
PLUGIN_SRC="$ADAPTER_DIR/qwenpaw"
PLUGIN_DST="${QWENPAW_WORKING_DIR%/}/plugins/${PLUGIN_ID}"

# QwenPaw installs requirements.txt with its own interpreter (the CLI's
# shebang). On a platform without a matching wheel pip installs nothing and
# still exits 0, and a released wheel may predate the SDK surface the plugin
# imports, so prove the SDK imports there. QWENPAW_PYTHON overrides the
# interpreter; a CLI without a Python shebang (frozen build) is left unverified.
check_sdk() {
    local python="${QWENPAW_PYTHON:-}"
    if [ -z "$python" ]; then
        python="$(sed -n '1s/^#![[:space:]]*//p' "$QWENPAW_BIN")"
        case "$python" in
            /usr/bin/env\ *) python="$(command -v "${python#/usr/bin/env }" 2>/dev/null || true)" ;;
        esac
    fi
    if [ -z "$python" ] || [ ! -x "$python" ] || ! "$python" --version 2>&1 | grep -q '^Python '; then
        return 2
    fi
    "$python" - <<'PY'
import sys
try:
    import anolisa_tokenless as sdk
except ImportError as error:
    sys.exit(f"anolisa_tokenless is not importable by {sys.executable}: {error}")
if not hasattr(sdk, "RecoveryMethod"):
    sys.exit(f"anolisa_tokenless {sdk.__version__} predates the SDK surface the plugin needs")
print(sdk.__version__)
PY
}

# The bundle pins the native SDK wheel by direct URL, and that URL is a GitHub
# Release asset for this exact Tokenless version. A version bump can land on
# main before the matching `tokenless/vX.Y.Z` release is published, and then
# `qwenpaw plugin install` dies inside pip with a bare "HTTP error 404" that
# never says which asset is missing. Probe the asset for this host first so the
# failure names the asset, its release tag, and how to close the gap.
#
# The probe is advisory: with no probe tool, no wheel line matching this
# platform, or an unreachable network it stays out of the way and leaves the
# verdict to pip. Only an explicit 404 reads as "this asset cannot be
# downloaded" -- it says nothing about whether the release itself exists.
# ANOLISA_SKIP_WHEEL_PREFLIGHT=1 turns the probe off for offline mirrors.
wheel_url_for_host() {
    local pattern
    case "$(uname -s 2>/dev/null || true)/$(uname -m 2>/dev/null || true)" in
        Linux/x86_64)  pattern='manylinux_2_17_x86_64' ;;
        Linux/aarch64) pattern='manylinux_2_17_aarch64' ;;
        Darwin/arm64)  pattern='macosx_11_0_arm64' ;;
        *)             return 0 ;;
    esac
    sed -n "s|^anolisa-tokenless @ \(https://[^ ;]*${pattern}[^ ;]*\.whl\).*|\1|p" \
        "$PLUGIN_SRC/requirements.txt" | head -n 1
}

probe_http_status() {
    local url="$1" timeout="${ANOLISA_TOKENLESS_PROBE_TIMEOUT:-15}"
    if command -v curl >/dev/null 2>&1; then
        # curl prints 000 when it cannot reach the host at all.
        curl -sIL -o /dev/null --max-time "$timeout" -w '%{http_code}' "$url" 2>/dev/null || true
        return 0
    fi
    if command -v python3 >/dev/null 2>&1; then
        python3 - "$url" "$timeout" <<'PY' 2>/dev/null || true
import sys
import urllib.error
import urllib.request

url, timeout = sys.argv[1], float(sys.argv[2])
request = urllib.request.Request(url, method="HEAD")
try:
    with urllib.request.urlopen(request, timeout=timeout) as response:
        print(response.status)
except urllib.error.HTTPError as error:
    print(error.code)
except (urllib.error.URLError, OSError, ValueError):
    pass  # unreachable host: no verdict, leave it to pip
PY
        return 0
    fi
    return 1
}

preflight_wheel() {
    if [ "${ANOLISA_SKIP_WHEEL_PREFLIGHT:-0}" = "1" ]; then
        return 0
    fi
    local url status tag
    url="$(wheel_url_for_host)" || return 0
    if [ -z "$url" ]; then
        return 0
    fi
    status="$(probe_http_status "$url")" || return 0
    if [ "$status" = "404" ]; then
        # A lone 404 can also be a truncated redirect chain or a proxy hiccup,
        # and a wrong verdict here blocks an install that would otherwise work,
        # so confirm it before believing it.
        status="$(probe_http_status "$url")" || return 0
    fi
    case "$status" in
        404) ;;
        # Published, still redirecting, or unreachable: no verdict, pip decides.
        2??|3??|000|"") return 0 ;;
        *)
            echo "[${COMPONENT}] Could not verify the SDK wheel at ${url} (probe reported HTTP ${status}); leaving the verdict to pip." >&2
            return 0
            ;;
    esac
    tag="${url#*/tokenless/v}"
    tag="${tag%%/*}"
    cat >&2 <<EOF
[${COMPONENT}] The Tokenless ${tag} Python SDK wheel asset is unavailable (HTTP 404):
[${COMPONENT}]   ${url}
[${COMPONENT}] This package was built from a source tree already at ${tag}, but that asset
[${COMPONENT}] cannot be downloaded. A maintainer must check whether the GitHub Release
[${COMPONENT}] \`tokenless/v${tag}\` exists:
[${COMPONENT}]   - it does not: push the \`tokenless/v${tag}\` tag and approve the \`release\`
[${COMPONENT}]     environment so the publish workflow uploads the wheels;
[${COMPONENT}]   - it does: the upload was incomplete, so delete that release and re-run
[${COMPONENT}]     the publish workflow, which refuses to overwrite an existing release.
[${COMPONENT}] Until the asset is downloadable, install a Tokenless package whose version
[${COMPONENT}] already has published wheels.
[${COMPONENT}] Behind an offline or mirrored network, re-run with
[${COMPONENT}] ANOLISA_SKIP_WHEEL_PREFLIGHT=1 to let pip resolve the wheel itself.
EOF
    return 1
}

echo "[${COMPONENT}] Installing ${AGENT} plugin..."

if [ ! -f "$PLUGIN_SRC/plugin.json" ] || [ ! -f "$PLUGIN_SRC/plugin.py" ] || [ ! -f "$PLUGIN_SRC/requirements.txt" ]; then
    echo "[${COMPONENT}] Missing plugin.json, plugin.py or requirements.txt in $PLUGIN_SRC" >&2
    exit 1
fi

if [ -z "$QWENPAW_BIN" ]; then
    QWENPAW_BIN="$(command -v qwenpaw 2>/dev/null || true)"
fi
if [ -z "$QWENPAW_BIN" ] || [ ! -x "$QWENPAW_BIN" ]; then
    echo "[${COMPONENT}] qwenpaw CLI not found — skipping plugin installation."
    echo "[${COMPONENT}] Install QwenPaw first (https://qwenpaw.agentscope.io/), then run this script again."
    exit 0
fi

if [ "$DRY_RUN" = "1" ]; then
    echo "DRY-RUN: QWENPAW_WORKING_DIR=${QWENPAW_WORKING_DIR%/} $QWENPAW_BIN plugin install $PLUGIN_SRC --force"
    exit 0
fi

# Fail before QwenPaw copies anything: an unpublishable wheel leaves a bundle
# that is installed but cannot run.
if ! preflight_wheel; then
    echo "[${COMPONENT}] Not handing QwenPaw a bundle whose SDK wheel cannot be downloaded." >&2
    exit 1
fi

QWENPAW_WORKING_DIR="${QWENPAW_WORKING_DIR%/}" "$QWENPAW_BIN" plugin install "$PLUGIN_SRC" --force

# A failed install may leave the previous bundle in place, so the installed
# files must match the source bundle, not merely exist.
for file in plugin.json plugin.py requirements.txt; do
    if ! cmp -s "$PLUGIN_SRC/$file" "$PLUGIN_DST/$file"; then
        echo "[${COMPONENT}] qwenpaw plugin install did not install $PLUGIN_SRC/$file into $PLUGIN_DST — see the output above." >&2
        exit 1
    fi
done

sdk_version="$(check_sdk)" && sdk_status=0 || sdk_status=$?
case "$sdk_status" in
    0) echo "[${COMPONENT}] anolisa_tokenless ${sdk_version} is importable by QwenPaw's Python." ;;
    2) echo "[${COMPONENT}] Could not find QwenPaw's Python interpreter; anolisa_tokenless import left unverified." >&2 ;;
    *)
        echo "[${COMPONENT}] ${sdk_version}" >&2
        echo "[${COMPONENT}] The plugin is copied but cannot run; install a matching anolisa_tokenless wheel into QwenPaw's Python environment (see $PLUGIN_SRC/requirements.txt for the supported platforms)." >&2
        exit 1
        ;;
esac

echo "[${COMPONENT}] ${AGENT} plugin installed to $PLUGIN_DST (from $PLUGIN_SRC)."
