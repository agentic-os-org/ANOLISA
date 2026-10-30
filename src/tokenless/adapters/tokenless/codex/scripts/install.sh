#!/usr/bin/env bash
# Install the Tokenless plugin for Codex.
#
# Two-phase installation:
#   Phase 1 — Binary: verify tokenless CLI is reachable (RPM or dev build).
#   Phase 2 — Plugin:  create a local codex marketplace, register it, and
#                       install the tokenless plugin via `codex plugin add`.
#
# Codex marketplace layout:
#
#   codex-marketplace/              ← marketplace root
#   ├── .agents/
#   │   └── plugins/
#   │       └── marketplace.json   ← manifest
#   └── tokenless/                 ← symlink to the actual plugin dir
#       ├── plugin.json
#       ├── hooks/
#       └── scripts/
#
# The plugin path in marketplace.json ("./tokenless") is resolved relative
# to the marketplace root, not the manifest file.
#
# Usage:
#   ./install.sh                    # Interactive
#   ./install.sh --non-interactive  # CI / automated
#
# Environment variables:
#   TOKENLESS_INSTALL_PREFIX   Installation prefix (default: ~/.local)
#   TOKENLESS_SOURCE_DIR       Path to anolisa/src/tokenless (dev-only override)
#   TOKENLESS_SKIP_BUILD       Set to "1" to skip cargo build
#   CODEX_BIN                  Path to codex CLI (default: codex)

set -euo pipefail

PREFIX="${TOKENLESS_INSTALL_PREFIX:-$HOME/.local}"
BINDIR="$PREFIX/bin"
CODEX_BIN="${CODEX_BIN:-}"
MARKETPLACE_NAME="anolisa-tokenless"
PLUGIN_ID="tokenless@${MARKETPLACE_NAME}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"

# Resolve codex binary via shared helper (see _common.sh).

# Resolve the codex plugin source directory.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=./_common.sh
source "$SCRIPT_DIR/_common.sh"
PLUGIN_SRC="$(cd "$SCRIPT_DIR/.." && pwd)"   # codex/ directory

# The paths and the CLI lookup are inert - no writes, no CLI calls - so they are
# resolved before Phase 1: the dry-run preview needs them to name what it would
# do, and a host without the tokenless binary must see the build it would run.
CODEX_BIN="$(resolve_codex)"
MARKETPLACE_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/anolisa/codex-marketplace"
AGENTS_DIR="$MARKETPLACE_DIR/.agents/plugins"
PLUGIN_LINK="$MARKETPLACE_DIR/tokenless"

# ---------------------------------------------------------------------------
# Phase 1 — Ensure tokenless binary is available
# ---------------------------------------------------------------------------

TOKENLESS_BIN=""
if command -v tokenless >/dev/null 2>&1; then
    TOKENLESS_BIN="$(command -v tokenless)"
elif [[ -x "$BINDIR/tokenless" ]]; then
    TOKENLESS_BIN="$BINDIR/tokenless"
elif [[ -x /usr/local/bin/tokenless ]]; then
    TOKENLESS_BIN="/usr/local/bin/tokenless"
elif [[ -x /usr/bin/tokenless ]]; then
    TOKENLESS_BIN="/usr/bin/tokenless"
fi

# Resolved here so the preview and the real path cannot drift apart: the
# assignment has no side effects and `cd` only fails when the directory is
# missing, which the preview reports instead of promising a build.
if [[ -n "${TOKENLESS_SOURCE_DIR:-}" ]]; then
    SRCDIR="$TOKENLESS_SOURCE_DIR"
else
    SRCDIR="$(cd "$SCRIPT_DIR/../../../.." 2>/dev/null && pwd || printf '%s' "$SCRIPT_DIR/../../../..")"
fi

# Phase 1 is a hard precondition, so it sits above the preview: a host with
# neither the binary nor the source tree stops here in both modes, with the same
# message and the same exit code, and the preview has nothing to qualify.
if [[ -z "$TOKENLESS_BIN" && ! -f "$SRCDIR/Cargo.toml" ]]; then
    echo "[tokenless] ERROR: tokenless binary not found and no source tree at $SRCDIR" >&2
    echo "[tokenless] Install the tokenless RPM package first:" >&2
    echo "[tokenless]   rpm -Uvh tokenless-*.rpm" >&2
    exit 1
fi

DRY_RUN="${ANOLISA_DRY_RUN:-0}"
if [ "$DRY_RUN" = "1" ]; then
    # Preview before the first write, in the order the real path acts and only
    # where it would act: Phase 1 builds and installs a binary when the host has
    # none and a source tree exists, Phase 2 writes the marketplace tree and
    # drives the CLI registrations, and a host without the codex CLI reaches
    # neither of the last two.
    if [[ -z "$TOKENLESS_BIN" ]]; then
        echo "DRY-RUN: cargo build --release -p tokenless-cli in ${SRCDIR}"
        echo "DRY-RUN: install the built binary to ${BINDIR}/tokenless (mode 755)"
    else
        echo "DRY-RUN: keep the existing tokenless binary ${TOKENLESS_BIN}"
    fi
    if [[ -n "$CODEX_BIN" ]]; then
        echo "DRY-RUN: mkdir -p ${AGENTS_DIR}"
        echo "DRY-RUN: ln -sfn ${PLUGIN_SRC} ${PLUGIN_LINK}"
        echo "DRY-RUN: write ${AGENTS_DIR}/marketplace.json"
        echo "DRY-RUN: ${CODEX_BIN} plugin marketplace remove ${MARKETPLACE_NAME} (only when it is already registered)"
        echo "DRY-RUN: ${CODEX_BIN} plugin marketplace add ${MARKETPLACE_DIR}"
        echo "DRY-RUN: ${CODEX_BIN} plugin add tokenless@${MARKETPLACE_NAME}"
    else
        echo "DRY-RUN: no codex CLI found, so the registration and the marketplace directory are both skipped"
    fi
    exit 0
fi

if [[ -z "$TOKENLESS_BIN" ]]; then
    # No binary — build from source (dev scenario). The source tree's presence
    # is already guaranteed by the precheck above, which both modes share.
    echo "[tokenless] Building tokenless from source: $SRCDIR"
    cd "$SRCDIR"
    cargo build --release -p tokenless-cli 2>&1 | tail -3
    mkdir -p "$BINDIR"
    cp "$SRCDIR/target/release/tokenless" "$BINDIR/tokenless"
    chmod 755 "$BINDIR/tokenless"
    echo "[tokenless] Built and installed to $BINDIR/tokenless"
else
    echo "[tokenless] Binary: $TOKENLESS_BIN ($("$TOKENLESS_BIN" --version))"
fi

# ---------------------------------------------------------------------------
# Phase 2 — Register plugin with codex
# ---------------------------------------------------------------------------

# 2a. Resolve codex and check availability (both resolved above, before the
# dry-run preview, so the preview can name the CLI it would drive)
if [[ -z "$CODEX_BIN" ]]; then
    echo "[tokenless] codex CLI not found — skipping plugin registration."
    echo "[tokenless] Install codex first, then run: codex plugin add ${PLUGIN_ID}"
    exit 0
fi

# 2a. Create marketplace directory layout in a user-local location so
# non-root users can run this script even when the plugin source tree
# is installed under a root-owned prefix (e.g. /usr/share/anolisa).
mkdir -p "$AGENTS_DIR"

# Symlink plugin at marketplace root (codex resolves plugin paths relative to
# the marketplace root, not the manifest file).
if [[ ! -L "$PLUGIN_LINK" ]] || [[ "$(readlink "$PLUGIN_LINK")" != "$PLUGIN_SRC" ]]; then
    rm -f "$PLUGIN_LINK"
    ln -sfn "$PLUGIN_SRC" "$PLUGIN_LINK"
fi

# 2b. Write marketplace.json at .agents/plugins/marketplace.json
cat > "$AGENTS_DIR/marketplace.json" <<JSON
{
    "name": "${MARKETPLACE_NAME}",
    "interface": {
        "displayName": "ANOLISA Tokenless"
    },
    "plugins": [
        {
            "name": "tokenless",
            "source": {
                "source": "local",
                "path": "./tokenless"
            },
            "policy": {
                "installation": "AVAILABLE"
            },
            "category": "developer-tools"
        }
    ]
}
JSON

# 2c. Register marketplace (idempotent — remove first if exists)
if "$CODEX_BIN" plugin marketplace list 2>/dev/null | grep -q "^${MARKETPLACE_NAME}[[:space:]]"; then
    "$CODEX_BIN" plugin marketplace remove "$MARKETPLACE_NAME" 2>/dev/null || true
fi
echo "[tokenless] Registering marketplace '${MARKETPLACE_NAME}'..."
"$CODEX_BIN" plugin marketplace add "$MARKETPLACE_DIR" 2>&1

# 2d. Install the plugin from marketplace
echo "[tokenless] Installing plugin '${PLUGIN_ID}'..."
"$CODEX_BIN" plugin add "${PLUGIN_ID}" 2>&1

echo "[tokenless] Codex plugin installation complete."
echo "[tokenless] Verify: codex plugin list"
