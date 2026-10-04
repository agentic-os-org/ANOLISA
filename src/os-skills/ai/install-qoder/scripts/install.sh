#!/bin/sh
# install.sh — Install Qoder CLI (qodercli) and register the Tokenless plugin.
#
# Wraps the official Qoder installer (https://qoder.com/install) with the
# preflight checks and post-install verification used by the ANOLISA tokenless
# qoder adapter (src/tokenless/adapters/tokenless/qoder/scripts/install.sh):
#   - qodercli lookup order: newest ~/.qoder/bin/qodercli/qodercli-<version>
#     (sort -V), then ~/.qoder/bin/qodercli/qodercli, then $PATH
#   - `qodercli plugins install/list/uninstall` capability check
#   - python3 requirement (Tokenless hooks depend on it)
#   - verification that tokenless@local is present, user-scoped, and enabled
#
# Tokenless registration prefers the anolisa CLI (component record); when only
# the npm-installed adapter resources exist it runs the adapter's own
# install.sh directly.
#
# Usage:
#   sh scripts/install.sh [--skip-tokenless]
#
# Options:
#   --skip-tokenless  Install Qoder CLI only; skip Tokenless registration.

set -eu

SKIP_TOKENLESS=0
for arg in "$@"; do
    case "$arg" in
        --skip-tokenless) SKIP_TOKENLESS=1 ;;
        *)
            echo "install-qoder: unknown argument: $arg (usage: $0 [--skip-tokenless])" >&2
            exit 2
            ;;
    esac
done

info() { echo "[install-qoder] $*"; }
err()  { echo "[install-qoder] ERROR: $*" >&2; }

# --- Preflight: required commands ------------------------------------------
for cmd in curl python3; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        err "$cmd is required (official installer / Tokenless hooks)."
        exit 1
    fi
done

# --- Locate qodercli (mirrors the adapter's find_qodercli) -----------------
find_qodercli() {
    versioned_glob="$HOME/.qoder/bin/qodercli/qodercli-*"
    # shellcheck disable=SC2086,SC2012 # Intentional version glob + version sort, mirroring the adapter.
    latest_versioned="$(ls -d $versioned_glob 2>/dev/null | sort -V | tail -1 || true)"

    for candidate in "$latest_versioned" \
                     "$HOME/.qoder/bin/qodercli/qodercli" \
                     qodercli; do
        [ -n "$candidate" ] || continue
        if [ -x "$candidate" ] || command -v "$candidate" >/dev/null 2>&1; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

QODERCLI="$(find_qodercli || true)"
if [ -n "$QODERCLI" ]; then
    info "qodercli already installed: $QODERCLI"
else
    info "Installing Qoder CLI via the official installer..."
    if ! curl -fsSL --max-time 20 https://qoder.com/install -o /dev/null 2>/dev/null; then
        err "cannot reach https://qoder.com/install (network offline or blocked?)."
        exit 1
    fi
    curl -fsSL https://qoder.com/install | bash

    QODERCLI="$(find_qodercli || true)"
    if [ -z "$QODERCLI" ]; then
        err "qodercli not found after running the official installer."
        err "Open a new shell (PATH refresh) and re-run this script."
        exit 1
    fi
    info "Installed qodercli: $QODERCLI"
fi

# --- Capability check (mirrors the adapter's plugin lifecycle probe) -------
for subcommand in install list uninstall; do
    if ! "$QODERCLI" plugins "$subcommand" --help >/dev/null 2>&1; then
        err "qodercli lacks required 'plugins $subcommand' support (Qoder CLI too old?)."
        exit 1
    fi
done
info "qodercli plugin lifecycle commands available."

# --- Tokenless registration -------------------------------------------------
if [ "$SKIP_TOKENLESS" = 1 ]; then
    info "Skipping Tokenless registration (--skip-tokenless)."
    info "Enable later with: anolisa adapter enable tokenless qoder"
    exit 0
fi

if command -v anolisa >/dev/null 2>&1; then
    info "Enabling the Tokenless qoder adapter via anolisa..."
    anolisa adapter enable tokenless qoder
elif [ -f "$HOME/.local/share/anolisa/adapters/tokenless/qoder/scripts/install.sh" ]; then
    info "anolisa CLI not found; running the tokenless qoder adapter install script..."
    # shellcheck disable=SC1090 # Path is validated above; adapter script is bash.
    bash "$HOME/.local/share/anolisa/adapters/tokenless/qoder/scripts/install.sh"
else
    err "Tokenless is not installed. Install it first:"
    err "  curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash"
    err "then re-run this script (or pass --skip-tokenless to install qodercli only)."
    exit 1
fi

# --- Verify registration (mirrors the adapter's inventory check) -----------
if ! "$QODERCLI" plugins list --json 2>/dev/null | python3 -c '
import json
import sys

try:
    inventory = json.load(sys.stdin)
except ValueError:
    sys.exit(1)
if isinstance(inventory, dict):
    inventory = inventory.get("plugins")
if not isinstance(inventory, list):
    sys.exit(1)
matches = [i for i in inventory
           if isinstance(i, dict) and i.get("id") == "tokenless@local"]
ok = (len(matches) == 1
      and matches[0].get("scope") == "user"
      and matches[0].get("enabled") is True)
sys.exit(0 if ok else 1)
'; then
    err "tokenless@local (user scope, enabled) not found in qodercli plugins list."
    err "Re-run Step 5 of the skill, then fully restart the Qoder IDE."
    exit 1
fi

info "Tokenless plugin registered (tokenless@local, user scope, enabled)."
info "Fully restart the Qoder IDE / qodercli session to load the plugin."
