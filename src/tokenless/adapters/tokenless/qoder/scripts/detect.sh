#!/usr/bin/env bash
# detect.sh — Check whether Qoder CLI supports the native plugin lifecycle.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-qoder}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"

versioned_glob="$HOME/.qoder/bin/qodercli/qodercli-${ANOLISA_QODER_VERSION:-*}"
# shellcheck disable=SC2086 # Intentional version glob expansion.
latest_versioned="$(ls -d $versioned_glob 2>/dev/null | sort -V | tail -1 || true)"

QODERCLI=""
for candidate in "$latest_versioned" \
                 "$HOME/.qoder/bin/qodercli/qodercli" \
                 "qodercli"; do
    [ -n "$candidate" ] || continue
    if [ -x "$candidate" ] || command -v "$candidate" >/dev/null 2>&1; then
        QODERCLI="$candidate"
        break
    fi
done

if [ -z "$QODERCLI" ]; then
    echo "[${COMPONENT}] ${AGENT}: qodercli not found in standard locations" >&2
    exit 1
fi

for subcommand in install list uninstall; do
    if ! "$QODERCLI" plugins "$subcommand" --help >/dev/null 2>&1; then
        echo "[${COMPONENT}] ${AGENT}: qodercli lacks 'plugins ${subcommand}' support" >&2
        exit 1
    fi
done

echo "[${COMPONENT}] ${AGENT}: detected compatible qodercli at ${QODERCLI}"
