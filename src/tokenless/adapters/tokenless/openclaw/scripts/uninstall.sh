#!/usr/bin/env bash
# uninstall.sh — Remove tokenless plugin via OpenClaw official CLI.
#
# TODO(adapter-manifest): keep this explicit script while adapter actions are
# invoked by component Makefile/build-all instead of a shared manifest runner.
set -euo pipefail

# A minimal runner may omit HOME. Under `set -u` an unguarded expansion aborts
# this uninstaller with exit 1 -- the same status it otherwise reports for "not installed but installable",
# so the crash would reach the caller as a verdict instead of as an error.
# Resolve HOME once and derive every user-scope path from it; an empty value
# leaves the destination unresolved and the script refuses to act on a path
# under /.

HOME_DIR="${HOME:-}"

AGENT="${ANOLISA_TARGET:-openclaw}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
OPENCLAW_BIN="${OPENCLAW_BIN:-}"
# The OpenClaw home and state directory come from the explicit overrides
# first and from the home directory second. They stay empty when neither
# names a location, so the check below can tell "not configured" from
# "configured to /".
OPENCLAW_HOME="${OPENCLAW_HOME:-}"
if [ -z "$OPENCLAW_HOME" ] && [ -n "$HOME_DIR" ]; then
    OPENCLAW_HOME="$HOME_DIR/.openclaw"
fi
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-$OPENCLAW_HOME}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"
OPENCLAW_HOME="${OPENCLAW_HOME%/}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"
USER_LOCAL_BIN="${HOME_DIR:+$HOME_DIR/.local/bin:}"
export PATH="${USER_LOCAL_BIN}${OPENCLAW_STATE_DIR:+${OPENCLAW_STATE_DIR%/}/bin:}/usr/local/bin:$PATH"

# The uninstaller removes the plugin from the OpenClaw state directory, so
# that directory has to be known before anything is removed. A path built
# from a missing HOME would sit under /, and an explicit OPENCLAW_HOME or
# OPENCLAW_STATE_DIR still names a real directory.
if [ -z "$OPENCLAW_STATE_DIR" ]; then
    echo "[${COMPONENT}] ERROR: cannot resolve the OpenClaw state directory: HOME is unset and no override names it." >&2
    echo "[${COMPONENT}] Set HOME, OPENCLAW_HOME or OPENCLAW_STATE_DIR to the directory to use, then retry." >&2
    exit 1
fi

if [ -z "$OPENCLAW_BIN" ]; then
    OPENCLAW_BIN="$(command -v openclaw 2>/dev/null || true)"
fi

echo "[${COMPONENT}] Removing ${AGENT} plugin..."

if [ "$DRY_RUN" = "1" ]; then
    if [ -n "$OPENCLAW_BIN" ]; then
        echo "DRY-RUN: env -u OPENCLAW_HOME OPENCLAW_STATE_DIR=$OPENCLAW_STATE_DIR $OPENCLAW_BIN plugins uninstall tokenless --force"
    else
        echo "DRY-RUN: openclaw CLI not found; remove plugin files manually"
    fi
    echo "DRY-RUN: rm -rf ${OPENCLAW_STATE_DIR%/}/plugins/tokenless"
    echo "DRY-RUN: rm -rf ${OPENCLAW_STATE_DIR%/}/extensions/tokenless"
    exit 0
fi

if [ -z "$OPENCLAW_BIN" ]; then
    echo "[${COMPONENT}] openclaw CLI not found — removing plugin files manually."
    rm -rf "${OPENCLAW_STATE_DIR%/}/plugins/tokenless" 2>/dev/null || true
    rm -rf "${OPENCLAW_STATE_DIR%/}/extensions/tokenless" 2>/dev/null || true
    echo "[${COMPONENT}] Plugin files removed. Manually clean up openclaw.json if needed."
    exit 0
fi

# Use openclaw CLI for proper removal (handles file cleanup + config update)
env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" plugins uninstall tokenless --force || true

# Verify rather than trust: the status above is swallowed because "was not
# installed" and "refused" are indistinguishable by exit code alone, and the caller
# deletes the adapter resources as soon as this script returns 0. The manual path
# above removes these two directories itself, so the CLI path is held to the same
# post-condition.
CLI_FAILED=0
for leftover in "${OPENCLAW_STATE_DIR%/}/plugins/tokenless" "${OPENCLAW_STATE_DIR%/}/extensions/tokenless"; do
    if [ -e "$leftover" ] || [ -L "$leftover" ]; then
        echo "[${COMPONENT}] ERROR: ${leftover} is still there after 'plugins uninstall'." >&2
        CLI_FAILED=1
    fi
done
if [ "$CLI_FAILED" = "1" ]; then
    echo "[${COMPONENT}] ${AGENT} plugin removal is incomplete; remove the paths above and run this script again." >&2
    exit 1
fi

echo "[${COMPONENT}] ${AGENT} plugin removed via openclaw CLI."
