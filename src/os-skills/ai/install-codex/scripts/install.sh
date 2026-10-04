#!/bin/sh
# install.sh — Install the OpenAI Codex CLI and register the Tokenless plugin.
#
# Wraps the official npm distribution (npm install -g @openai/codex) with the
# preflight checks and post-install verification used by the ANOLISA tokenless
# codex adapter (src/tokenless/adapters/tokenless/codex/scripts/install.sh):
#   - codex lookup order (adapters/.../codex/scripts/_common.sh resolve_codex):
#     $PATH codex, /usr/local/bin/codex, /usr/bin/codex, ~/.local/bin/codex
#   - tokenless CLI readiness (adapters/.../codex/scripts/detect.sh paths)
#   - plugin registration via the anolisa-tokenless marketplace, verified with
#     `codex plugin list` showing tokenless@anolisa-tokenless installed
#
# Tokenless registration prefers the anolisa CLI (component record); when only
# the npm-installed adapter resources exist it runs the adapter's own
# install.sh directly (marketplace add + plugin add).
#
# Usage:
#   sh scripts/install.sh [--skip-tokenless]
#
# Options:
#   --skip-tokenless  Install Codex CLI only; skip Tokenless registration.

set -eu

MARKETPLACE_NAME="anolisa-tokenless"

SKIP_TOKENLESS=0
for arg in "$@"; do
    case "$arg" in
        --skip-tokenless) SKIP_TOKENLESS=1 ;;
        *)
            echo "install-codex: unknown argument: $arg (usage: $0 [--skip-tokenless])" >&2
            exit 2
            ;;
    esac
done

info() { echo "[install-codex] $*"; }
err()  { echo "[install-codex] ERROR: $*" >&2; }

# --- Preflight: required commands ------------------------------------------
for cmd in npm python3; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        err "$cmd is required (official npm package / Tokenless hook scripts)."
        exit 1
    fi
done

# --- Locate codex (mirrors the adapter's resolve_codex) --------------------
resolve_codex() {
    for candidate in codex /usr/local/bin/codex /usr/bin/codex "$HOME/.local/bin/codex"; do
        if command -v "$candidate" >/dev/null 2>&1; then
            command -v "$candidate"
            return 0
        fi
    done
    for candidate in /usr/local/bin/codex /usr/bin/codex "$HOME/.local/bin/codex"; do
        if [ -x "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

CODEX_BIN="$(resolve_codex || true)"
if [ -n "$CODEX_BIN" ]; then
    info "codex already installed: $CODEX_BIN"
else
    info "Installing Codex CLI from npm..."
    if ! npm ping >/dev/null 2>&1; then
        err "cannot reach the npm registry (network offline, blocked, or bad proxy?)."
        exit 1
    fi
    # Never sudo npm -g; a user prefix avoids EACCES (see SKILL.md Step 2).
    npm install -g @openai/codex

    CODEX_BIN="$(resolve_codex || true)"
    if [ -z "$CODEX_BIN" ]; then
        err "codex not found after npm install."
        err "Check 'npm config get prefix' and add its bin directory to PATH, then re-run."
        exit 1
    fi
    info "Installed codex: $CODEX_BIN"
fi

# --- Tokenless registration -------------------------------------------------
if [ "$SKIP_TOKENLESS" = 1 ]; then
    info "Skipping Tokenless registration (--skip-tokenless)."
    info "Enable later with: anolisa adapter enable tokenless codex"
    exit 0
fi

# The plugin's SessionStart hook verifies the tokenless CLI; check the same
# paths the adapter's detect.sh probes so registration is not attempted against
# a missing runtime.
TOKENLESS_BIN=""
if command -v tokenless >/dev/null 2>&1; then
    TOKENLESS_BIN="$(command -v tokenless)"
else
    for fp in "$HOME/.local/bin/tokenless" \
              /usr/local/bin/tokenless \
              /usr/bin/tokenless \
              "$HOME/.local/share/anolisa/tokenless/tokenless" \
              "$HOME/.local/lib/anolisa/tokenless/tokenless"; do
        if [ -f "$fp" ] && [ -x "$fp" ]; then
            TOKENLESS_BIN="$fp"
            break
        fi
    done
fi
if [ -z "$TOKENLESS_BIN" ]; then
    err "tokenless CLI not found — the Codex plugin needs it at session start."
    err "Install it first:"
    err "  curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash"
    err "then re-run this script (or pass --skip-tokenless to install codex only)."
    exit 1
fi
info "tokenless CLI ready: $TOKENLESS_BIN"

if command -v anolisa >/dev/null 2>&1; then
    info "Enabling the Tokenless codex adapter via anolisa..."
    anolisa adapter enable tokenless codex
elif [ -f "$HOME/.local/share/anolisa/adapters/tokenless/codex/scripts/install.sh" ]; then
    info "anolisa CLI not found; running the tokenless codex adapter install script..."
    # shellcheck disable=SC1090 # Path is validated above; adapter script is bash.
    bash "$HOME/.local/share/anolisa/adapters/tokenless/codex/scripts/install.sh"
else
    err "Tokenless adapter resources not found under ~/.local/share/anolisa/adapters/tokenless/codex."
    err "Install Tokenless first (see above), then re-run this script."
    exit 1
fi

# --- Verify registration ----------------------------------------------------
# `codex plugin list` is a table; only a row for this plugin whose status is
# not "not installed" counts as registered (mirrors the adapter's uninstall
# verification semantics).
if ! "$CODEX_BIN" plugin list 2>/dev/null \
        | grep -E "^[[:space:]]*tokenless(@[^[:space:]]*)?[[:space:]]" \
        | grep -Eiv "^[[:space:]]*[^[:space:]]+[[:space:]]+not[[:space:]]+installed([[:space:]]|$)" \
        >/dev/null; then
    err "tokenless@${MARKETPLACE_NAME} not listed as installed in 'codex plugin list'."
    err "Re-run Step 5 of the skill, then start a NEW Codex session."
    exit 1
fi

info "Tokenless plugin registered (tokenless@${MARKETPLACE_NAME}, installed)."
info "Start a NEW Codex session to load the plugin."
