#!/usr/bin/env bash
# detect.sh — Inspect tokenless QwenPaw integration. Read-only.
#
# Reports qwenpaw CLI, QwenPaw working directory, tokenless plugin install
# state, and adapter resource availability. Exit codes:
#   0 = installed and ready
#   1 = not installed but installable
#   2 = missing prerequisites
set -euo pipefail

AGENT="${ANOLISA_TARGET:-qwenpaw}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
# Resolve the working directory like QwenPaw: QWENPAW_WORKING_DIR, else
# COPAW_WORKING_DIR, else a legacy ~/.copaw, else ~/.qwenpaw.
PLUGIN_ID="tokenless"
QWENPAW_WORKING_DIR="${QWENPAW_WORKING_DIR:-${COPAW_WORKING_DIR:-}}"
if [ -z "$QWENPAW_WORKING_DIR" ]; then
    if [ -d "$HOME/.copaw" ]; then QWENPAW_WORKING_DIR="$HOME/.copaw"; other="$HOME/.qwenpaw"; else QWENPAW_WORKING_DIR="$HOME/.qwenpaw"; other="$HOME/.copaw"; fi
    # ~/.copaw may have appeared or vanished since the plugin was installed:
    # prefer the default location that actually holds it.
    if [ ! -d "$QWENPAW_WORKING_DIR/plugins/$PLUGIN_ID" ] && [ -d "$other/plugins/$PLUGIN_ID" ]; then
        QWENPAW_WORKING_DIR="$other"
    fi
fi
QWENPAW_BIN="${QWENPAW_BIN:-}"
export PATH="${QWENPAW_HOME:-$HOME/.qwenpaw}/bin:$HOME/.local/bin:/usr/local/bin:$PATH"

PLUGIN_SRC="$ADAPTER_DIR/qwenpaw"

# QwenPaw installs requirements.txt with its own interpreter (the CLI's
# shebang). On a platform without a matching wheel pip installs nothing and
# still exits 0, and a released wheel can export every class name the plugin
# imports while still predating a field the plugin passes: the published 0.8.4
# wheel had no PostToolRequest.command, so a name-level check passed and every
# tool call died with a TypeError. Ask the bundle's own plugin.py what it needs
# and prove the installed wheel provides it. QWENPAW_PYTHON overrides the
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
    ANOLISA_PLUGIN_PY="$PLUGIN_SRC/plugin.py" "$python" - <<'PY'
import importlib.util
import os
import sys
import types

plugin_py = os.environ["ANOLISA_PLUGIN_PY"]


def fail(reason):
    # Both callers capture stdout, so the reason lands in the script's own
    # report instead of only in this process' stderr.
    print(reason)
    sys.exit(1)


try:
    import anolisa_tokenless as sdk
except ImportError as error:
    fail(f"anolisa_tokenless is not importable by {sys.executable}: {error}")

if not hasattr(sdk, "RecoveryMethod"):
    fail(f"anolisa_tokenless {sdk.__version__} predates the SDK surface the plugin needs")


def declared_gaps():
    """What the bundled plugin.py declares this wheel to be missing."""
    # plugin.py owns the declaration, so the verdict cannot drift from what the
    # plugin actually constructs. It imports AgentScope at module level, which
    # is QwenPaw's dependency and not this interpreter's, so stub those modules.
    for name, attributes in {
        "agentscope": (),
        "agentscope.message": ("TextBlock", "ToolResultState"),
        "agentscope.middleware": ("MiddlewareBase",),
        "agentscope.tool": ("ToolChunk", "ToolResponse"),
    }.items():
        module = types.ModuleType(name)
        module.__path__ = []
        for attribute in attributes:
            setattr(module, attribute, type(attribute, (), {}))
        sys.modules[name] = module
    spec = importlib.util.spec_from_file_location("tokenless_qwenpaw_plugin", plugin_py)
    bundle = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bundle)
    return bundle._missing_sdk_surface(sdk)


try:
    gaps = declared_gaps()
except Exception as error:
    # A guard that cannot load is no verdict: report it on stderr, which the
    # callers do not capture, and leave the decision to the plugin's own
    # registration check.
    gaps = []
    print(f"could not check the SDK surface declared by {plugin_py}: {error}", file=sys.stderr)
if gaps:
    fail(
        f"anolisa_tokenless {sdk.__version__} lacks {', '.join(gaps)}, which the "
        f"tokenless plugin at {plugin_py} requires"
    )
print(sdk.__version__)
PY
}

line()  { printf '[%s] %s\n' "$COMPONENT" "$*"; }
field() { printf '[%s]   %-26s %s\n' "$COMPONENT" "$1" "$2"; }

PREREQ_MISSING=()
INSTALL_MISSING=()
note_prereq_missing() { PREREQ_MISSING+=("$1"); }
note_install_missing() { INSTALL_MISSING+=("$1"); }

if [ -z "$QWENPAW_BIN" ]; then
    QWENPAW_BIN="$(command -v qwenpaw 2>/dev/null || true)"
fi

line "${AGENT} detect"
if [ -n "$QWENPAW_BIN" ] && [ -x "$QWENPAW_BIN" ]; then
    field "qwenpaw CLI" "present (${QWENPAW_BIN})"
else
    field "qwenpaw CLI" "missing"
    note_prereq_missing "qwenpaw CLI"
fi

if [ -d "$QWENPAW_WORKING_DIR" ]; then
    field "qwenpaw working dir" "present (${QWENPAW_WORKING_DIR})"
else
    field "qwenpaw working dir" "not initialized (${QWENPAW_WORKING_DIR})"
    note_install_missing "qwenpaw working dir"
fi

if [ -f "$PLUGIN_SRC/plugin.json" ] && [ -f "$PLUGIN_SRC/plugin.py" ] && [ -f "$PLUGIN_SRC/requirements.txt" ]; then
    field "plugin resource" "present (${PLUGIN_SRC})"
else
    field "plugin resource" "missing (${PLUGIN_SRC})"
    note_prereq_missing "plugin resource"
fi

plugin_dst="${QWENPAW_WORKING_DIR%/}/plugins/${PLUGIN_ID}"
if [ -f "$plugin_dst/plugin.json" ]; then
    stale=""
    for file in plugin.json plugin.py requirements.txt; do
        cmp -s "$PLUGIN_SRC/$file" "$plugin_dst/$file" || stale="$file"
    done
    if [ -n "$stale" ]; then
        field "${PLUGIN_ID} plugin" "stale (${plugin_dst}/${stale} differs from ${PLUGIN_SRC})"
        note_install_missing "${PLUGIN_ID} plugin"
    else
        field "${PLUGIN_ID} plugin" "installed (${plugin_dst})"
    fi
else
    field "${PLUGIN_ID} plugin" "missing (${plugin_dst})"
    note_install_missing "${PLUGIN_ID} plugin"
fi

if [ -n "$QWENPAW_BIN" ] && [ -x "$QWENPAW_BIN" ]; then
    sdk_version="$(check_sdk 2>/dev/null)" && sdk_status=0 || sdk_status=$?
    case "$sdk_status" in
        0) field "anolisa_tokenless SDK" "importable (${sdk_version})" ;;
        2) field "anolisa_tokenless SDK" "unverified (no Python shebang in ${QWENPAW_BIN})" ;;
        *)
            field "anolisa_tokenless SDK" "unusable (${sdk_version})"
            note_install_missing "anolisa_tokenless SDK"
            ;;
    esac
fi

if [ ${#PREREQ_MISSING[@]} -gt 0 ]; then
    line "${AGENT}: missing prerequisites (${PREREQ_MISSING[*]})"
    exit 2
fi
if [ ${#INSTALL_MISSING[@]} -gt 0 ]; then
    line "${AGENT}: not installed (ready to install)"
    exit 1
fi
line "${AGENT}: ready"
exit 0
