#!/usr/bin/env bash
# uninstall.sh — Remove tokenless plugin from Qwen Code via `qwen extensions uninstall`.
# Falls back to manual cleanup of the extension directory when qwen is unavailable.
set -euo pipefail

# A minimal runner may omit HOME. Under `set -u` an unguarded expansion aborts
# this uninstaller with exit 1 -- the same status it otherwise reports for "not installed but installable",
# so the crash would reach the caller as a verdict instead of as an error.
# Resolve HOME once and derive every user-scope path from it; an empty value
# leaves the destination unresolved and the script refuses to act on a path
# under /.

HOME_DIR="${HOME:-}"

AGENT="${ANOLISA_TARGET:-qwencode}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"

EXTENSION_NAME="tokenless"

QWEN_BIN="${QWEN_BIN:-}"
USER_LOCAL_BIN="${HOME_DIR:+$HOME_DIR/.local/bin:}"
export PATH="${USER_LOCAL_BIN}/usr/local/bin:$PATH"

# Qwen Code resolves its home the way the Rust adapter driver does: the
# QWEN_HOME environment variable wins, then a QWEN_HOME entry in the user's
# .env file, and only then the home directory itself. Reading the two in the
# same order keeps the shell scripts and the driver pointed at the same
# directory on a host that sets any of them.
QWEN_HOME="${QWEN_HOME:-}"
if [ -z "$QWEN_HOME" ] && [ -n "$HOME_DIR" ]; then
    QWEN_HOME="$HOME_DIR/.qwen"
    for qwen_env_file in "$HOME_DIR/.qwen/.env" "$HOME_DIR/.env"; do
        [ -f "$qwen_env_file" ] || continue
        qwen_env_value="$(sed -n 's/^[[:space:]]*\(export[[:space:]]\+\)\?QWEN_HOME[[:space:]]*=[[:space:]]*//p' \
            "$qwen_env_file" | tail -1)"
        [ -n "$qwen_env_value" ] || continue
        QWEN_HOME="$qwen_env_value"
        break
    done
fi

# The uninstaller removes the extension directory and the enablement file, so
# the directory has to be known before it touches either. A path built from a
# missing HOME would land under /, and an explicit QWEN_HOME still names a
# real directory.
if [ -z "$QWEN_HOME" ]; then
    echo "[${COMPONENT}] ERROR: cannot resolve the Qwen Code home: HOME is unset and QWEN_HOME is not set." >&2
    echo "[${COMPONENT}] Set HOME or QWEN_HOME to the Qwen Code directory to use, then retry." >&2
    exit 1
fi

echo "[${COMPONENT}] Uninstalling ${AGENT} plugin..."

if [ -z "$QWEN_BIN" ]; then
    QWEN_BIN="$(command -v qwen 2>/dev/null || true)"
fi

# Preview before any removal: the CLI path uninstalls the active
# extension and the manual fallback deletes the extension directory —
# a dry run must leave both in place.
if [ "$DRY_RUN" = "1" ]; then
    if [ -n "$QWEN_BIN" ] && [ -x "$QWEN_BIN" ]; then
        echo "DRY-RUN: $QWEN_BIN extensions uninstall $EXTENSION_NAME"
    fi
    echo "DRY-RUN: rm -rf ${QWEN_HOME}/extensions/${EXTENSION_NAME} (manual fallback)"
    echo "DRY-RUN: clean ${QWEN_HOME}/extension-enablement.json (manual fallback)"
    exit 0
fi

if [ -n "$QWEN_BIN" ] && [ -x "$QWEN_BIN" ]; then
    "$QWEN_BIN" extensions uninstall "$EXTENSION_NAME" 2>&1 || true
    # Verify rather than trust: the status above is swallowed because "was not
    # installed" and "refused" are indistinguishable by exit code alone, and the
    # caller deletes the adapter resources as soon as this script returns 0.
    if [ -e "${QWEN_HOME}/extensions/${EXTENSION_NAME}" ]; then
        echo "[${COMPONENT}] ERROR: ${QWEN_HOME}/extensions/${EXTENSION_NAME} is still there after 'extensions uninstall'." >&2
        echo "[${COMPONENT}] ${AGENT} plugin removal is incomplete; re-run this script once the CLI succeeds." >&2
        exit 1
    fi
    echo "[${COMPONENT}] ${AGENT} plugin removed via qwen CLI."
    exit 0
fi

echo "[${COMPONENT}] qwen CLI not found — falling back to manual cleanup."

# Qwen Code stores installed extensions under ~/.qwen/extensions/ and
# metadata in ~/.qwen/.qwen-extension-install.json.
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
