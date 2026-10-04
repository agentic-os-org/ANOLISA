#!/bin/sh
# install.sh — Register the Tokenless plugin for OpenCode and verify the link.
#
# Wraps the two documented registration routes with the checks used by the
# ANOLISA tokenless opencode adapter
# (src/tokenless/adapters/tokenless/opencode/scripts/install.sh):
#   - opencode CLI lookup: $OPENCODE_BIN, then PATH (adapter install.sh:10-17)
#   - config dir (scripts route): TOKENLESS_OPENCODE_CONFIG_DIR, then
#     OPENCODE_CONFIG_DIR, then XDG_CONFIG_HOME/opencode, then
#     ~/.config/opencode (adapter install.sh:23)
#   - the anolisa driver resolves the same directory except it ignores
#     TOKENLESS_OPENCODE_CONFIG_DIR (docs/user-guide/en/user-entrypoint/anolisa-cli.md:229-236)
#   - the plugin lands at plugins/tokenless.js as a symlink to the adapter's
#     plugin.js; both routes refuse conflicting files or links
#
# Usage:
#   sh scripts/install.sh [--skip-tokenless]
#
# Options:
#   --skip-tokenless  Run the environment preflight only; skip registration.

set -eu

SKIP_TOKENLESS=0
for arg in "$@"; do
    case "$arg" in
        --skip-tokenless) SKIP_TOKENLESS=1 ;;
        -h|--help)
            echo "usage: $0 [--skip-tokenless]"
            exit 0
            ;;
        *)
            echo "install-opencode: unknown argument: $arg (usage: $0 [--skip-tokenless])" >&2
            exit 2
            ;;
    esac
done

info() { echo "[install-opencode] $*"; }
err()  { echo "[install-opencode] ERROR: $*" >&2; }

# --- Locate the opencode CLI (mirrors the adapter/driver lookup) -------------
OPENCODE_BIN="${OPENCODE_BIN:-}"
if [ -z "$OPENCODE_BIN" ]; then
    OPENCODE_BIN="$(command -v opencode 2>/dev/null || true)"
fi
if [ -z "$OPENCODE_BIN" ] || { [ ! -x "$OPENCODE_BIN" ] && ! command -v "$OPENCODE_BIN" >/dev/null 2>&1; }; then
    err "opencode CLI not found (checked \$OPENCODE_BIN and PATH)."
    err "The repo documents no opencode installer; install OpenCode from its official channel, then re-run."
    exit 1
fi
info "opencode CLI found: $OPENCODE_BIN"

ADAPTER_INSTALL="$HOME/.local/share/anolisa/adapters/tokenless/opencode/scripts/install.sh"

# --- Tokenless registration --------------------------------------------------
if [ "$SKIP_TOKENLESS" = 1 ]; then
    info "Skipping Tokenless registration (--skip-tokenless)."
    info "Enable later with: anolisa adapter enable tokenless opencode"
    exit 0
fi

if command -v anolisa >/dev/null 2>&1; then
    info "Enabling the Tokenless opencode adapter via anolisa..."
    anolisa adapter enable tokenless opencode
elif [ -f "$ADAPTER_INSTALL" ]; then
    info "anolisa CLI not found; running the tokenless opencode adapter install script..."
    # shellcheck disable=SC1090 # Path is validated above; adapter script is bash.
    bash "$ADAPTER_INSTALL"
else
    err "Tokenless is not installed (neither the anolisa CLI nor the adapter resources were found)."
    err "Install it first (see the install-tokenless skill), then re-run this script."
    err "Or pass --skip-tokenless to preflight the opencode CLI only."
    exit 1
fi

# --- Verify the plugin link (mirrors the adapter's managed-link contract) ----
CONFIG_HOME="${TOKENLESS_OPENCODE_CONFIG_DIR:-${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}}"
PLUGIN_LINK="$CONFIG_HOME/plugins/tokenless.js"
if [ ! -L "$PLUGIN_LINK" ] || [ ! -e "$PLUGIN_LINK" ]; then
    err "plugin link missing or dangling after registration: $PLUGIN_LINK"
    err "Resolve any reported conflict, keep the same OPENCODE_CONFIG_DIR, and retry."
    exit 1
fi
LINK_TARGET="$(readlink "$PLUGIN_LINK")"
case "$LINK_TARGET" in
    *tokenless*) info "plugin link verified: $PLUGIN_LINK -> $LINK_TARGET" ;;
    *)
        err "unexpected plugin link target (expected the tokenless adapter's plugin.js): $LINK_TARGET"
        exit 1
        ;;
esac

# --- anolisa route: additionally verify the receipt --------------------------
if command -v anolisa >/dev/null 2>&1; then
    if ! anolisa adapter status tokenless >/dev/null 2>&1; then
        err "anolisa adapter status tokenless failed after enable."
        err "Keep the same configuration-directory settings and retry the enable."
        exit 1
    fi
fi

info "Tokenless plugin registered for OpenCode."
info "Fully restart OpenCode to load the plugin, then check 'tokenless stats list'."
