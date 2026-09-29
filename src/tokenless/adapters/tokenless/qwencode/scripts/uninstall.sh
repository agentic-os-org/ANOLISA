#!/usr/bin/env bash
# uninstall.sh — Remove tokenless plugin from Qwen Code via `qwen extensions uninstall`.
# Falls back to manual cleanup of the extension directory when qwen is unavailable.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-qwencode}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"

EXTENSION_NAME="tokenless"

QWEN_BIN="${QWEN_BIN:-}"
export PATH="$HOME/.local/bin:/usr/local/bin:$PATH"

echo "[${COMPONENT}] Uninstalling ${AGENT} plugin..."

if [ -z "$QWEN_BIN" ]; then
    QWEN_BIN="$(command -v qwen 2>/dev/null || true)"
fi

if [ -n "$QWEN_BIN" ] && [ -x "$QWEN_BIN" ]; then
    "$QWEN_BIN" extensions uninstall "$EXTENSION_NAME" 2>&1 || true
    # Verify rather than trust: the status above is swallowed because "was not
    # installed" and "refused" are indistinguishable by exit code alone, and the
    # caller deletes the adapter resources as soon as this script returns 0.
    if [ -e "${HOME}/.qwen/extensions/${EXTENSION_NAME}" ]; then
        echo "[${COMPONENT}] ERROR: ${HOME}/.qwen/extensions/${EXTENSION_NAME} is still there after 'extensions uninstall'." >&2
        echo "[${COMPONENT}] ${AGENT} plugin removal is incomplete; re-run this script once the CLI succeeds." >&2
        exit 1
    fi
    echo "[${COMPONENT}] ${AGENT} plugin removed via qwen CLI."
    exit 0
fi

echo "[${COMPONENT}] qwen CLI not found — falling back to manual cleanup."

# Qwen Code stores installed extensions under ~/.qwen/extensions/ and
# metadata in ~/.qwen/.qwen-extension-install.json.
QWEN_HOME="${HOME}/.qwen"
EXTENSIONS_DIR="${QWEN_HOME}/extensions"

# Find and remove the tokenless extension directory.
for subdir in "$EXTENSIONS_DIR"/*; do
    [ -d "$subdir" ] || continue
    # Match by name field in qwen-extension.json.
    manifest="$subdir/qwen-extension.json"
    if [ -f "$manifest" ] && grep -q '"name": *"tokenless"' "$manifest" 2>/dev/null; then
        rm -rf "$subdir"
        echo "[${COMPONENT}] removed $subdir"
    fi
done

# Also clean up the install-metadata file that may reference the link source.
for subdir in "$EXTENSIONS_DIR"/*; do
    [ -d "$subdir" ] || continue
    metadata="$subdir/.qwen-extension-install.json"
    if [ -f "$metadata" ] && grep -q '"name": *"tokenless"' "$metadata" 2>/dev/null; then
        rm -rf "$subdir"
        echo "[${COMPONENT}] removed $subdir (via install-metadata)"
    fi
done

# Remove from extension-enablement.json if present.
ENABLEMENT_FILE="${QWEN_HOME}/extension-enablement.json"
if [ -f "$ENABLEMENT_FILE" ] && command -v jq &>/dev/null; then
    tmp="$(mktemp "${ENABLEMENT_FILE}.XXXXXX")"
    jq 'del(.tokenless)' "$ENABLEMENT_FILE" > "$tmp" && mv "$tmp" "$ENABLEMENT_FILE"
    echo "[${COMPONENT}] cleaned $ENABLEMENT_FILE"
fi

echo "[${COMPONENT}] manual cleanup complete."
