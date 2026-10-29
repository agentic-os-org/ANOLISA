#!/usr/bin/env bash
# install.sh — Register tokenless as a local Claude Code marketplace and
# install the plugin via the official `claude plugin` CLI.
#
# Responsibility boundary (mirrors openclaw/scripts/install.sh):
#   - This script ONLY deploys an already-stamped plugin manifest.
#   - Manifest stamping (plugin.json.in -> plugin.json) is the Makefile's job:
#       make -C src/tokenless stamp-adapter-templates
#     which `make install` runs automatically before `install-adapter-resources`
#     copies the result into $SHARE_DIR/claude-code.
#   - A dev-only fallback stamps the manifest in place when called outside the
#     RPM/Makefile flow, so adapter-install works on a freshly checked-out tree.
#
# Claude Code v2 requires plugins to be sourced from a registered marketplace.
# We expose the adapter's claude-code/ directory itself as a single-plugin
# marketplace ("anolisa-tokenless"), then install tokenless@anolisa-tokenless
# from it. The marketplace name is component-scoped so multiple ANOLISA
# components can each register their own without colliding.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-claude-code}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"

PLUGIN_SRC="$ADAPTER_DIR/claude-code"
MARKETPLACE_NAME="anolisa-${COMPONENT}"
PLUGIN_ID="${COMPONENT}@${MARKETPLACE_NAME}"

CLAUDE_BIN="${CLAUDE_BIN:-claude}"

# A minimal runner may omit HOME. Under `set -u` an unguarded expansion aborts
# this installer with exit 1 before it can say why, and unlike the detector this
# script registers a marketplace and installs a plugin. Resolve HOME once and
# derive every user-scope path from it, so an explicit CLAUDE_HOME override keeps
# working and a missing home is reported before the marketplace is registered.
#
# CLAUDE_HOME is the adapter's own configuration directory -- $HOME/.claude --
# which is what every other *_HOME override in this component means (HERMES_HOME
# is $HOME/.hermes, CODEX_HOME is $HOME/.codex). It is not the home root: a
# caller who sets it to "$HOME/.claude" would otherwise have the settings file
# looked up at "$HOME/.claude/.claude/settings.json", and the uninstall would
# report success without having touched the real registration.
HOME_DIR="${HOME:-}"
# A relative HOME would resolve every path below against whatever directory
# the caller happened to be in. A home has to be an absolute path; anything
# else is treated as no home at all, which is the case the guard below
# already reports.
case "$HOME_DIR" in
    /*) ;;
    [A-Za-z]:[\\/]*) ;;
    *) HOME_DIR="" ;;
esac
USER_LOCAL_BIN="${HOME_DIR:+$HOME_DIR/.local/bin:}"
CLAUDE_HOME="${CLAUDE_HOME:-${HOME_DIR:+$HOME_DIR/.claude}}"
export PATH="${USER_LOCAL_BIN}/usr/local/bin:$PATH"

echo "[${COMPONENT}] Installing ${AGENT} plugin..."

if [ -z "$CLAUDE_HOME" ]; then
    echo "[${COMPONENT}] Cannot resolve the Claude Code configuration directory: HOME is unset and CLAUDE_HOME is not set." >&2
    echo "[${COMPONENT}] Set HOME or CLAUDE_HOME to the Claude Code configuration directory, then retry." >&2
    exit 1
fi

# The CLI reads its own configuration directory from the environment, so an
# explicit override has to reach it -- otherwise the guard below passes while
# the CLI still resolves a different directory and writes somewhere this
# script knows nothing about. CLAUDE_CONFIG_DIR is the name the CLI itself
# uses; it is derived from CLAUDE_HOME only when the caller did not set it.
CLAUDE_CONFIG_DIR="${CLAUDE_CONFIG_DIR:-$CLAUDE_HOME}"
export CLAUDE_CONFIG_DIR

if ! command -v "$CLAUDE_BIN" &>/dev/null; then
    echo "[${COMPONENT}] claude CLI not found (CLAUDE_BIN=${CLAUDE_BIN}) — skipping plugin installation."
    echo "[${COMPONENT}] Install Claude Code first, then run this script again."
    exit 0
fi

if [ ! -d "$PLUGIN_SRC" ]; then
    echo "[${COMPONENT}] Plugin source not found: $PLUGIN_SRC" >&2
    exit 1
fi

if [ ! -f "$PLUGIN_SRC/.claude-plugin/marketplace.json" ]; then
    echo "[${COMPONENT}] ERROR: $PLUGIN_SRC/.claude-plugin/marketplace.json missing." >&2
    exit 1
fi

PLUGIN_MANIFEST="$PLUGIN_SRC/.claude-plugin/plugin.json"
PLUGIN_TEMPLATE="$PLUGIN_SRC/.claude-plugin/plugin.json.in"

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    # Preview before the first write: the dev fallback below stamps the manifest
    # into the plugin tree, so a dry run stops ahead of it and of the two CLI
    # registrations it would follow. The stamp and the marketplace registration
    # are conditional in the real path, so the preview says so rather than
    # announcing work that would not happen on an already-provisioned host (the
    # `marketplace list` probe is not run here: it would call the CLI).
    if [ ! -f "$PLUGIN_MANIFEST" ] && [ -f "$PLUGIN_TEMPLATE" ]; then
        echo "DRY-RUN: stamp ${PLUGIN_MANIFEST} from ${PLUGIN_TEMPLATE}"
    fi
    echo "DRY-RUN: ${CLAUDE_BIN} plugin marketplace add ${PLUGIN_SRC} (only when the marketplace is not registered)"
    echo "DRY-RUN: ${CLAUDE_BIN} plugin install ${PLUGIN_ID}"
    exit 0
fi

# Dev-only fallback: stamp plugin.json from .in template when Makefile hasn't
# run yet. Production installs (RPM, `make install`) ship a stamped manifest
# inside SHARE_DIR, so this branch is a no-op in those flows.

if [ ! -f "$PLUGIN_MANIFEST" ] && [ -f "$PLUGIN_TEMPLATE" ]; then
    VERSION="${TOKENLESS_VERSION:-${ANOLISA_VERSION:-0.0.0-dev}}"
    sed "s/@VERSION@/${VERSION}/g" "$PLUGIN_TEMPLATE" > "$PLUGIN_MANIFEST"
    echo "[${COMPONENT}] dev-fallback: stamped plugin.json (version=${VERSION}) — production builds should stamp via Makefile"
fi

if [ ! -f "$PLUGIN_MANIFEST" ] ; then
    echo "[${COMPONENT}] ERROR: $PLUGIN_MANIFEST missing." >&2
    echo "[${COMPONENT}]        Stamp the manifest first:" >&2
    echo "[${COMPONENT}]            make -C src/tokenless stamp-adapter-templates" >&2
    exit 1
fi

# Validate the stamped plugin via the official CLI — same gate the install
# call would hit; surfacing the error here gives a cleaner failure message.
"$CLAUDE_BIN" plugin validate "$PLUGIN_SRC" >/dev/null \
    || { echo "[${COMPONENT}] plugin validation failed" >&2; exit 1; }

# Idempotent marketplace add. We probe `marketplace list` first so that a
# `marketplace add` failure surfaces its real cause (bad path, malformed
# manifest, settings.json write error) instead of being swallowed as
# "already registered".
echo "[${COMPONENT}] registering marketplace '${MARKETPLACE_NAME}' from ${PLUGIN_SRC}..."
if "$CLAUDE_BIN" plugin marketplace list 2>/dev/null \
       | grep -qE "(^|[[:space:]])${MARKETPLACE_NAME}([[:space:]]|$)"; then
    echo "[${COMPONENT}] marketplace '${MARKETPLACE_NAME}' already registered"
else
    "$CLAUDE_BIN" plugin marketplace add "$PLUGIN_SRC" \
        || { echo "[${COMPONENT}] marketplace add failed" >&2; exit 1; }
fi

# Idempotent plugin install.
echo "[${COMPONENT}] installing ${PLUGIN_ID}..."
"$CLAUDE_BIN" plugin install "$PLUGIN_ID" 2>&1 \
    || { echo "[${COMPONENT}] plugin install failed" >&2; exit 1; }

echo "[${COMPONENT}] ${AGENT} plugin installed via claude CLI."
echo "[${COMPONENT}] Restart claude (or run /plugin) to activate."
