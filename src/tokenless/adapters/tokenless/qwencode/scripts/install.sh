#!/usr/bin/env bash
# install.sh — Register tokenless plugin for Qwen Code via `qwen extensions link`.
#
# Responsibility boundary:
#   - This script ONLY deploys an already-stamped plugin manifest.
#   - Manifest stamping (qwen-extension.json.in -> qwen-extension.json) is the
#     Makefile's job:
#       make -C src/tokenless stamp-adapter-templates
#     which `make install` runs automatically before `install-adapter-resources`
#     copies the result into $SHARE_DIR/qwencode.
#   - A dev-only fallback stamps the manifest in place when called outside the
#     RPM/Makefile flow, so adapter-install works on a freshly checked-out tree.
#
# Qwen Code extensions can be installed via `link` (local path, no npm build
# needed) or `install` (local path, copies files). We use `link` so that
# changes to the adapter directory are reflected immediately without reinstall.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-qwencode}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"

PLUGIN_SRC="$ADAPTER_DIR/qwencode"
EXTENSION_NAME="tokenless"

QWEN_BIN="${QWEN_BIN:-}"
export PATH="$HOME/.local/bin:/usr/local/bin:$PATH"
if [ -z "$QWEN_BIN" ]; then
    QWEN_BIN="$(command -v qwen 2>/dev/null || true)"
fi

DRY_RUN="${ANOLISA_DRY_RUN:-0}"

echo "[${COMPONENT}] Installing ${AGENT} plugin..."

if ! command -v "$QWEN_BIN" &>/dev/null; then
    echo "[${COMPONENT}] qwen CLI not found (QWEN_BIN=${QWEN_BIN}) — skipping plugin installation."
    echo "[${COMPONENT}] Install Qwen Code first, then run this script again."
    exit 0
fi

if [ ! -d "$PLUGIN_SRC" ]; then
    echo "[${COMPONENT}] Plugin source not found: $PLUGIN_SRC" >&2
    exit 1
fi

# Dev-only fallback: stamp qwen-extension.json from .in template when Makefile
# hasn't run yet. Production installs (RPM, `make install`) ship a stamped
# manifest inside SHARE_DIR, so this branch is a no-op in those flows.
EXTENSION_MANIFEST="$PLUGIN_SRC/qwen-extension.json"
EXTENSION_TEMPLATE="$PLUGIN_SRC/qwen-extension.json.in"
if [ ! -f "$EXTENSION_MANIFEST" ] && [ -f "$EXTENSION_TEMPLATE" ]; then
    VERSION="${TOKENLESS_VERSION:-${ANOLISA_VERSION:-0.0.0-dev}}"
    sed "s/@VERSION@/${VERSION}/g" "$EXTENSION_TEMPLATE" > "$EXTENSION_MANIFEST"
    echo "[${COMPONENT}] dev-fallback: stamped qwen-extension.json (version=${VERSION}) — production builds should stamp via Makefile"
fi

if [ ! -f "$EXTENSION_MANIFEST" ]; then
    echo "[${COMPONENT}] ERROR: $EXTENSION_MANIFEST missing." >&2
    echo "[${COMPONENT}]        Stamp the manifest first:" >&2
    echo "[${COMPONENT}]            make -C src/tokenless stamp-adapter-templates" >&2
    exit 1
fi

# Idempotent: unlink first if already linked, then link again.
# `qwen extensions list` may not reliably show link state, so we
# always re-link to ensure the latest version is active.
if "$QWEN_BIN" extensions list 2>/dev/null | grep -qE "(^|[[:space:]])${EXTENSION_NAME}([[:space:]]|$)"; then
    echo "[${COMPONENT}] extension '${EXTENSION_NAME}' already registered, re-linking..."
    if ! "$QWEN_BIN" extensions uninstall "$EXTENSION_NAME" 2>/dev/null; then
        echo "[${COMPONENT}] WARNING: qwen extensions uninstall failed (non-fatal, will re-link)"
    fi
fi

if [ "$DRY_RUN" = "1" ]; then
    echo "DRY-RUN: $QWEN_BIN extensions link $PLUGIN_SRC"
    exit 0
fi

echo "[${COMPONENT}] linking extension from ${PLUGIN_SRC}..."
"$QWEN_BIN" extensions link "$PLUGIN_SRC" \
    || { echo "[${COMPONENT}] ERROR: qwen extensions link failed" >&2; exit 1; }

# Verify the extension was registered.
if "$QWEN_BIN" extensions list 2>/dev/null | grep -qE "(^|[[:space:]])${EXTENSION_NAME}([[:space:]]|$)"; then
    echo "[${COMPONENT}] ${AGENT} plugin installed and linked via qwen CLI."
else
    echo "[${COMPONENT}] WARNING: extension '${EXTENSION_NAME}' may not be visible in extensions list yet."
    echo "[${COMPONENT}] Run 'qwen extensions list' or restart qwen-code to confirm."
fi

# Clean up tokenless-specific hook entries from settings.json — the
# extension's qwen-extension.json already provides all hooks, so duplicate
# entries in settings.json cause double registration (2x invocations per
# event). Only remove hook entries whose name starts with "tokenless-",
# preserving any user-defined hooks that may also exist.
# Qwen Code stores global settings under ~/.qwen/settings.json.
SETTINGS_JSON="$HOME/.qwen/settings.json"
if [ -f "$SETTINGS_JSON" ] && command -v python3 &>/dev/null; then
    if python3 -c "
import json, sys
path = sys.argv[1]
with open(path) as f:
    d = json.load(f)
if 'hooks' not in d:
    sys.exit(1)
changed = False
for event, definitions in d['hooks'].items():
    if not isinstance(definitions, list):
        continue
    filtered = [defn for defn in definitions
                if not any(h.get('name', '').startswith('tokenless-')
                           for h in defn.get('hooks', []))]
    if len(filtered) != len(definitions):
        d['hooks'][event] = filtered
        changed = True
# If all hook events are now empty, remove the entire hooks key.
if all(not v for v in d['hooks'].values()):
    del d['hooks']
if changed:
    with open(path, 'w') as f:
        json.dump(d, f, indent=2)
    sys.exit(0)
sys.exit(1)
" "$SETTINGS_JSON" 2>/dev/null; then
        echo "[${COMPONENT}] Cleaned up tokenless hook entries from ${SETTINGS_JSON} (extension provides them via qwen-extension.json)."
    fi
fi
