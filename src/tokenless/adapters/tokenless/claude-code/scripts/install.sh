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
export PATH="$HOME/.local/bin:/usr/local/bin:$PATH"

echo "[${COMPONENT}] Installing ${AGENT} plugin..."

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

# Dev-only fallback: stamp plugin.json from .in template when Makefile hasn't
# run yet. Production installs (RPM, `make install`) ship a stamped manifest
# inside SHARE_DIR, so this branch is a no-op in those flows.
PLUGIN_MANIFEST="$PLUGIN_SRC/.claude-plugin/plugin.json"
PLUGIN_TEMPLATE="$PLUGIN_SRC/.claude-plugin/plugin.json.in"
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
