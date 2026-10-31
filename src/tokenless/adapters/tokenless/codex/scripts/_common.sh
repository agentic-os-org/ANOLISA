#!/usr/bin/env bash
# _common.sh — Shared helpers for codex adapter scripts.
# Source this file from install.sh and uninstall.sh.

# Resolve the codex CLI binary.
resolve_codex() {
    # An explicit CODEX_BIN is authoritative. Silently substituting some other
    # codex found on the system would deregister against a different installation
    # than the one the caller pinned, so an unusable pin resolves to "not found"
    # and the caller fails closed instead of guessing.
    if [[ -n "$CODEX_BIN" ]]; then
        if command -v "$CODEX_BIN" &>/dev/null; then
            echo "$CODEX_BIN"
        else
            echo ""
        fi
        return
    fi
    local USER_HOME="${HOME:-}"
    case "$USER_HOME" in
        /*) ;;
        *) USER_HOME="" ;;
    esac
    local user_codex=""
    if [[ -n "$USER_HOME" ]]; then
        user_codex="$USER_HOME/.local/bin/codex"
    fi
    for candidate in codex /usr/local/bin/codex /usr/bin/codex "$user_codex"; do
        if command -v "$candidate" &>/dev/null; then
            echo "$candidate"
            return
        fi
    done
    # Last resort: direct path check
    for candidate in /usr/local/bin/codex /usr/bin/codex "$user_codex"; do
        if [[ -x "$candidate" ]]; then
            echo "$candidate"
            return
        fi
    done
    echo ""
}
