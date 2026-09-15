#!/usr/bin/env bash
# _common.sh — Shared helpers for kimicode adapter scripts.
# Source this file from detect.sh, install.sh and uninstall.sh.
#
# Kimi data-root resolution
# -------------------------
# Two upstream contracts are in the field and they do not share a directory:
#
#   * Kimi Code (MoonshotAI/kimi-code, the current product) reads
#     "$KIMI_CODE_HOME/config.toml", default "~/.kimi-code".
#   * kimi-cli (MoonshotAI/kimi-cli, now wound down; installing Kimi Code
#     migrates its config and sessions) read "$KIMI_SHARE_DIR/config.toml",
#     default "~/.kimi".
#
# Both ship a `kimi` binary, so the CLI on PATH cannot tell them apart — the
# data root is the only observable difference. Writing only the legacy root
# leaves a Kimi Code host with a config it never loads: detect reports ready,
# the hook never fires, and uninstall misses it. Resolution order:
#
#   1. $KIMI_CODE_HOME — explicit override for the current product
#   2. $KIMI_SHARE_DIR — explicit override for a legacy kimi-cli install
#   3. ~/.kimi-code    — the current product has already run on this host
#   4. ~/.kimi         — only a legacy install has run here
#   5. ~/.kimi-code    — nothing on disk yet: create for the current product
#
# The hook entries themselves need no such split: both products read a file
# named "config.toml" and the same strict "[[hooks]]" schema (event, matcher,
# command, timeout — any extra key fails the whole config load).

# Echoes "<origin>\t<path>" for the data root selected by the rules above.
# Single source of truth: resolve_kimi_home and resolve_kimi_home_origin both
# read this, so a reported reason can never drift from the resolved path.
_kimi_home_resolution() {
    if [ -n "${KIMI_CODE_HOME:-}" ]; then
        printf 'KIMI_CODE_HOME\t%s\n' "$KIMI_CODE_HOME"
    elif [ -n "${KIMI_SHARE_DIR:-}" ]; then
        printf 'KIMI_SHARE_DIR (legacy kimi-cli)\t%s\n' "$KIMI_SHARE_DIR"
    elif [ -d "${HOME}/.kimi-code" ]; then
        printf 'found ~/.kimi-code\t%s\n' "${HOME}/.kimi-code"
    elif [ -d "${HOME}/.kimi" ]; then
        printf 'found ~/.kimi (legacy kimi-cli)\t%s\n' "${HOME}/.kimi"
    else
        printf 'default (no Kimi data root on disk yet)\t%s\n' "${HOME}/.kimi-code"
    fi
}

# The Kimi data root this adapter must read and write.
resolve_kimi_home() {
    local resolution
    resolution="$(_kimi_home_resolution)"
    printf '%s\n' "${resolution#*$'\t'}"
}

# Why that root was selected — for detect.sh / install.sh diagnostics.
resolve_kimi_home_origin() {
    local resolution
    resolution="$(_kimi_home_resolution)"
    printf '%s\n' "${resolution%%$'\t'*}"
}

# Every data root on this host that may still hold tokenless hooks: the
# resolved one first, then the other well-known root when it exists.
#
# Kimi Code migrates a legacy "~/.kimi" config into "~/.kimi-code", so hooks
# written before a migration are present in both files. Uninstall cleans both,
# otherwise a later migration re-imports hooks whose scripts are already gone.
kimi_home_cleanup_roots() {
    local resolved candidate
    resolved="$(resolve_kimi_home)"
    printf '%s\n' "$resolved"
    for candidate in "${HOME}/.kimi-code" "${HOME}/.kimi"; do
        if [ "$candidate" != "$resolved" ] && [ -d "$candidate" ]; then
            printf '%s\n' "$candidate"
        fi
    done
}
