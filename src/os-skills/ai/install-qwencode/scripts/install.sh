#!/bin/sh
# install.sh — Install Qwen Code (qwen CLI) and register the Tokenless extension.
#
# Wraps the official npm distribution (npm install -g @qwen-code/qwen-code,
# Node.js >= 22 per the package engines) with the preflight checks and
# post-install verification used by the ANOLISA tokenless qwencode adapter
# (src/tokenless/adapters/tokenless/qwencode/scripts/):
#   - qwen lookup with $HOME/.local/bin and /usr/local/bin prepended to PATH
#     (install.sh/detect.sh export the same PATH)
#   - tokenless and rtk runtime prerequisites (detect.sh lists both)
#   - registration via `qwen extensions link`, verified with
#     `qwen extensions list` showing the tokenless extension
#
# Tokenless registration prefers the anolisa CLI (component record); when only
# the npm-installed adapter resources exist it runs the adapter's own
# install.sh directly.
#
# Usage:
#   sh scripts/install.sh [--skip-tokenless]
#
# Options:
#   --skip-tokenless  Install Qwen Code only; skip Tokenless registration.

set -eu

EXTENSION_NAME="tokenless"
# Mirror the adapter's PATH so a user-local qwen is found like detect.sh does.
PATH="$HOME/.local/bin:/usr/local/bin:$PATH"
export PATH

SKIP_TOKENLESS=0
for arg in "$@"; do
    case "$arg" in
        --skip-tokenless) SKIP_TOKENLESS=1 ;;
        *)
            echo "install-qwencode: unknown argument: $arg (usage: $0 [--skip-tokenless])" >&2
            exit 2
            ;;
    esac
done

info() { echo "[install-qwencode] $*"; }
err()  { echo "[install-qwencode] ERROR: $*" >&2; }

# --- Preflight: required commands ------------------------------------------
for cmd in npm python3; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        err "$cmd is required (official npm package / hook scripts)."
        exit 1
    fi
done

# --- Locate qwen ------------------------------------------------------------
QWEN_BIN="$(command -v qwen 2>/dev/null || true)"

if [ -n "$QWEN_BIN" ]; then
    info "qwen already installed: $QWEN_BIN"
else
    # Official package requires Node.js >= 22 (engines field).
    node_major="$(node --version 2>/dev/null | sed 's/^v//' | cut -d. -f1 || true)"
    case "$node_major" in
        ''|*[!0-9]*)
            err "cannot determine Node.js version (node missing or unparsable)."
            exit 1
            ;;
    esac
    if [ "$node_major" -lt 22 ]; then
        err "Node.js >= 22 required by @qwen-code/qwen-code, found v${node_major}.x."
        err "Upgrade first, e.g.: nvm install --lts"
        exit 1
    fi

    info "Installing Qwen Code from npm..."
    if ! npm ping >/dev/null 2>&1; then
        err "cannot reach the npm registry (network offline, blocked, or bad proxy?)."
        exit 1
    fi
    # Never sudo npm -g; a user prefix avoids EACCES (see SKILL.md Step 2).
    npm install -g @qwen-code/qwen-code

    QWEN_BIN="$(command -v qwen 2>/dev/null || true)"
    if [ -z "$QWEN_BIN" ]; then
        err "qwen not found after npm install."
        err "Check 'npm config get prefix' and add its bin directory to PATH, then re-run."
        exit 1
    fi
    info "Installed qwen: $QWEN_BIN"
fi

# --- Tokenless registration -------------------------------------------------
if [ "$SKIP_TOKENLESS" = 1 ]; then
    info "Skipping Tokenless registration (--skip-tokenless)."
    info "Enable later with: anolisa adapter enable tokenless qwencode"
    exit 0
fi

# The extension hooks need the tokenless and rtk binaries; detect.sh lists both
# as prerequisites, so probe them before registering.
for rt_bin in tokenless rtk; do
    if ! command -v "$rt_bin" >/dev/null 2>&1; then
        err "$rt_bin CLI not found — the tokenless extension hooks need it."
        err "Install Tokenless first:"
        err "  curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash"
        err "then re-run this script (or pass --skip-tokenless to install qwen only)."
        exit 1
    fi
done
info "tokenless and rtk CLIs ready."

if command -v anolisa >/dev/null 2>&1; then
    info "Enabling the Tokenless qwencode adapter via anolisa..."
    anolisa adapter enable tokenless qwencode
elif [ -f "$HOME/.local/share/anolisa/adapters/tokenless/qwencode/scripts/install.sh" ]; then
    info "anolisa CLI not found; running the tokenless qwencode adapter install script..."
    # shellcheck disable=SC1090 # Path is validated above; adapter script is bash.
    bash "$HOME/.local/share/anolisa/adapters/tokenless/qwencode/scripts/install.sh"
else
    err "Tokenless adapter resources not found under ~/.local/share/anolisa/adapters/tokenless/qwencode."
    err "Install Tokenless first (see above), then re-run this script."
    exit 1
fi

# --- Verify registration (mirrors the adapter's extensions list check) ------
if ! "$QWEN_BIN" extensions list 2>/dev/null \
        | grep -qE "(^|[[:space:]])${EXTENSION_NAME}([[:space:]]|$)"; then
    err "extension '${EXTENSION_NAME}' not visible in 'qwen extensions list'."
    err "Re-run Step 5 of the skill, then restart qwen-code and check again."
    exit 1
fi

info "Tokenless extension registered and listed (qwen extensions list)."
info "Restart qwen-code and run one tool call to load the extension."
