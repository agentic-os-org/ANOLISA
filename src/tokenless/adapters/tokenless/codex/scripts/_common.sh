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
    # An unset HOME must neither abort (set -u) nor degenerate the last probe
    # into "/.local/bin/codex": build the candidate list and only append the
    # home-relative probe when HOME actually resolves (see #8692's convention).
    local candidates=(codex /usr/local/bin/codex /usr/bin/codex)
    if [[ -n "${HOME:-}" ]]; then
        candidates+=("$HOME/.local/bin/codex")
    fi
    for candidate in "${candidates[@]}"; do
        if command -v "$candidate" &>/dev/null; then
            echo "$candidate"
            return
        fi
    done
    # Last resort: direct path check
    for candidate in "${candidates[@]:1}"; do
        if [[ -x "$candidate" ]]; then
            echo "$candidate"
            return
        fi
    done
    echo ""
}
