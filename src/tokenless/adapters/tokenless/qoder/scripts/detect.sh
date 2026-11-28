#!/usr/bin/env bash
# detect.sh — Check whether Qoder CLI supports the native plugin lifecycle.
#
# The exit code describes the *host CLI*, not the adapter:
#   0 = a qodercli supporting the plugin lifecycle is available
#   1 = qodercli is missing or lacks that lifecycle
#
# `tokenless init` therefore reads the JSON status published on stdout instead
# of the exit code. This script never asks qodercli whether the tokenless plugin
# is registered, so a compatible CLI yields "installable" — never "ready" — and
# an unusable CLI yields "missing_prereqs" rather than an offer to install:
#   {"status":"installable"}
#   {"status":"missing_prereqs"}
# install.sh is idempotent, so re-running it against a registered plugin is safe.
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
    echo '{"status":"missing_prereqs"}'
    exit 1
fi

for subcommand in install list uninstall; do
    if ! "$QODERCLI" plugins "$subcommand" --help >/dev/null 2>&1; then
        echo "[${COMPONENT}] ${AGENT}: qodercli lacks 'plugins ${subcommand}' support" >&2
        echo '{"status":"missing_prereqs"}'
        exit 1
    fi
done

echo "[${COMPONENT}] ${AGENT}: detected compatible qodercli at ${QODERCLI}"
echo '{"status":"installable"}'
