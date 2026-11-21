#!/usr/bin/env bash
# _common.sh — Shared helpers for codex adapter scripts.
# Source this file from install.sh and uninstall.sh.

USER_HOME="${HOME:-}"
case "$USER_HOME" in
    /*) ;;
    *) USER_HOME="" ;;
esac

# Accept only an absolute path; anything else is treated as unset.
#
# Every root this adapter derives a destructive path from (`rm -f`, `rm -rf`)
# goes through here: a relative override must never resolve into the caller's
# working directory. Callers that must reject a relative override instead of
# ignoring it check the raw value themselves.
abs_or_empty() {
    case "${1:-}" in
        /*) printf '%s' "$1" ;;
        *) printf '' ;;
    esac
}

# Resolve the codex CLI binary.
resolve_codex() {
    # An explicit CODEX_BIN is authoritative. Silently substituting some other
    # codex found on the system would deregister against a different installation
    # than the one the caller pinned, so an unusable pin resolves to "not found"
    # and the caller fails closed instead of guessing.
    if [[ -n "${CODEX_BIN:-}" ]]; then
        if command -v "$CODEX_BIN" &>/dev/null; then
            echo "$CODEX_BIN"
        else
            echo ""
        fi
        return
    fi
    for candidate in codex /usr/local/bin/codex /usr/bin/codex; do
        if command -v "$candidate" &>/dev/null; then
            echo "$candidate"
            return
        fi
    done
    if [[ -n "$USER_HOME" ]]; then
        local cand="$USER_HOME/.local/bin/codex"
        if [[ -x "$cand" ]]; then
            echo "$cand"
            return
        fi
    fi
    # Last resort: direct path check
    for candidate in /usr/local/bin/codex /usr/bin/codex; do
        if [[ -x "$candidate" ]]; then
            echo "$candidate"
            return
        fi
    done
    echo ""
}
