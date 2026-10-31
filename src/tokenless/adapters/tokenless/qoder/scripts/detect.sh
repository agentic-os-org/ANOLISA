#!/usr/bin/env bash
# detect.sh — Check whether Qoder CLI supports the native plugin lifecycle.
set -euo pipefail

# A minimal runner may omit HOME. Under `set -u` an unguarded expansion aborts
# this detector with exit 1 -- the same status it otherwise reports for "not installed but installable",
# so the crash would reach the caller as a verdict instead of as an error.
# Resolve HOME once and derive every user-scope path from it; an empty value
# leaves the destination unresolved and the script refuses to act on a path
# under /.

HOME_DIR="${HOME:-}"

AGENT="${ANOLISA_TARGET:-qoder}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"

# The Qoder installation directory comes from the explicit override first
# and from the home directory second. It stays empty when neither names a
# location, so the probe below has nothing to look at rather than a path
# under /.
QODER_HOME="${QODER_HOME:-}"
if [ -z "$QODER_HOME" ] && [ -n "$HOME_DIR" ]; then
    QODER_HOME="$HOME_DIR/.qoder"
fi
versioned_glob="${QODER_HOME:+$QODER_HOME/bin/qodercli/qodercli-${ANOLISA_QODER_VERSION:-*}}"
# shellcheck disable=SC2086 # Intentional version glob expansion.
latest_versioned="$(ls -d $versioned_glob 2>/dev/null | sort -V | tail -1 || true)"

QODERCLI=""
for candidate in "$latest_versioned" \
                 "${QODER_HOME:+$QODER_HOME/bin/qodercli/qodercli}" \
                 "qodercli"; do
    [ -n "$candidate" ] || continue
    if [ -x "$candidate" ] || command -v "$candidate" >/dev/null 2>&1; then
        QODERCLI="$candidate"
        break
    fi
done

if [ -z "$QODERCLI" ]; then
    if [ -z "$QODER_HOME" ]; then
        echo "[${COMPONENT}] ${AGENT}: HOME is unset, so there is no Qoder installation directory to look in" >&2
    else
        echo "[${COMPONENT}] ${AGENT}: qodercli not found in standard locations" >&2
    fi
    exit 1
fi

for subcommand in install list uninstall; do
    if ! "$QODERCLI" plugins "$subcommand" --help >/dev/null 2>&1; then
        echo "[${COMPONENT}] ${AGENT}: qodercli lacks 'plugins ${subcommand}' support" >&2
        exit 1
    fi
done

echo "[${COMPONENT}] ${AGENT}: detected compatible qodercli at ${QODERCLI}"
