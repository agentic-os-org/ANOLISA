#!/usr/bin/env bash
# install.sh — Register Tokenless in OpenCode's global local-plugin directory.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-opencode}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
PLUGIN_SOURCE="$ADAPTER_DIR/opencode/plugin.js"

# OpenCode 1.0.0-1.3.3 call every module export as a plugin function, so the
# object default export OpenCode 2 requires aborts their plugin initialization
# with `TypeError: fn is not a function`. 1.3.4 is the first release that reads
# an object default export, so it is the floor the manifest advertises.
MIN_OPENCODE_VERSION="1.3.4"

# Prints the first dotted-number version in the CLI's --version output.
opencode_version_number() {
    printf '%s' "$1" | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -n 1 || true
}

# opencode_version_at_least <have> <want>. A non-numeric suffix ("3-test") is
# ignored and a missing component counts as 0.
opencode_version_at_least() {
    local have want index=0 part_have part_want
    IFS='.' read -r -a have <<< "$1"
    IFS='.' read -r -a want <<< "$2"
    while [ "$index" -lt "${#want[@]}" ]; do
        part_have="${have[$index]:-0}"
        part_want="${want[$index]:-0}"
        part_have="${part_have//[!0-9]/}"
        part_want="${part_want//[!0-9]/}"
        if [ "${part_have:-0}" -gt "${part_want:-0}" ]; then return 0; fi
        if [ "${part_have:-0}" -lt "${part_want:-0}" ]; then return 1; fi
        index=$((index + 1))
    done
    return 0
}

# opencode_version_supported <raw --version output>. An unparseable version is
# not gated: refusing to install on a host we cannot read would disable the
# adapter for every future release whose --version wording changes.
opencode_version_supported() {
    local number
    number="$(opencode_version_number "$1")"
    if [ -z "$number" ]; then
        return 0
    fi
    opencode_version_at_least "$number" "$MIN_OPENCODE_VERSION"
}

OPENCODE_BIN="${OPENCODE_BIN:-}"
if [ -z "$OPENCODE_BIN" ]; then
    OPENCODE_BIN="$(command -v opencode 2>/dev/null || true)"
fi

# `anolisa adapter enable` records the plugin link in a receipt this script can
# neither read nor write; its own CLI is the only supported way to remove link
# and receipt together. Overridable so tests never reach a real installation.
ANOLISA_BIN="${ANOLISA_BIN:-}"
if [ -z "$ANOLISA_BIN" ]; then
    ANOLISA_BIN="$(command -v anolisa 2>/dev/null || true)"
fi
if [ -z "$OPENCODE_BIN" ] || { [ ! -x "$OPENCODE_BIN" ] && ! command -v "$OPENCODE_BIN" >/dev/null 2>&1; }; then
    echo "[${COMPONENT}] opencode CLI not found — skipping ${AGENT} plugin installation."
    exit 0
fi
if [ ! -f "$PLUGIN_SOURCE" ]; then
    echo "[${COMPONENT}] ERROR: OpenCode plugin source not found: ${PLUGIN_SOURCE}" >&2
    exit 1
fi

CONFIG_HOME="${TOKENLESS_OPENCODE_CONFIG_DIR:-${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}}"
PLUGIN_DIR="$CONFIG_HOME/plugins"
PLUGIN_LINK="$PLUGIN_DIR/tokenless.js"

is_managed_link() {
    [ -L "$PLUGIN_LINK" ] && {
        [ "$PLUGIN_LINK" -ef "$PLUGIN_SOURCE" ] ||
            [ "$(readlink "$PLUGIN_LINK")" = "$PLUGIN_SOURCE" ]
    }
}

# The config directory the anolisa CLI resolves for OpenCode. It reads
# OPENCODE_CONFIG_DIR, then XDG_CONFIG_HOME/opencode, then $HOME/.config/opencode
# -- and never TOKENLESS_OPENCODE_CONFIG_DIR, which is this installer's own
# highest-priority override.
cli_config_dir() {
    if [ -n "${OPENCODE_CONFIG_DIR:-}" ]; then
        printf '%s' "$OPENCODE_CONFIG_DIR"
    elif [ -n "${XDG_CONFIG_HOME:-}" ]; then
        printf '%s' "${XDG_CONFIG_HOME}/opencode"
    else
        printf '%s' "${HOME:-}/.config/opencode"
    fi
}

# Canonicalize a directory without touching anything below it.
canonical_dir() {
    realpath -m -- "$1" 2>/dev/null || printf '%s' "$1"
}

# Compare two *registration* paths: canonicalize the containing directory, then
# re-append the file name. Resolving the link itself would be wrong here, because
# every OpenCode registration points at the one shared bundle, so
# `A/plugins/tokenless.js` and `B/plugins/tokenless.js` canonicalize to the same
# `plugin.js` and a run scoped to B would clear A. Canonicalizing the directory
# still folds an alias or a different spelling of the same directory into one
# path, which is the equality that actually matters.
same_registration_path() {
    local left right
    left="$(canonical_dir "$(dirname -- "$1")")/$(basename -- "$1")"
    right="$(canonical_dir "$(dirname -- "$2")")/$(basename -- "$2")"
    [ "$left" = "$right" ]
}

VERSION="$($OPENCODE_BIN --version 2>/dev/null || true)"
if ! opencode_version_supported "$VERSION"; then
    echo "[${COMPONENT}] ${AGENT}: OpenCode ${VERSION:-unknown} is older than ${MIN_OPENCODE_VERSION}."
    echo "[${COMPONENT}] ${AGENT}: such releases call every plugin export as a function, which"
    echo "[${COMPONENT}] ${AGENT}: the object default export OpenCode 2 requires cannot satisfy."
    # Lowering the floor has to clear a registration made while those releases
    # were still supported. The adapter source behind the link is replaced in
    # place (the npm postinstall swaps the shared adapter tree), so leaving the
    # link would hand this host the new object default export and abort its
    # plugin initialization on every start.
    if is_managed_link; then
        if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
            echo "DRY-RUN: rm ${PLUGIN_LINK}"
        else
            # Hand the removal to the CLI that owns the receipt -- but only for
            # the link this run selected. `adapter disable` takes a component and
            # a framework, not a path, so it clears whatever link the receipt
            # records; when TOKENLESS_OPENCODE_CONFIG_DIR pointed this script at
            # a different directory, delegating would delete another, still
            # working registration. `adapter disable` is idempotent, so a
            # registration this installer created itself is a successful no-op
            # and the removal below still runs.
            cleared_receipt=0
            cli_link="$(cli_config_dir)/plugins/$(basename "$PLUGIN_LINK")"
            if [ -n "$ANOLISA_BIN" ] && same_registration_path "$PLUGIN_LINK" "$cli_link" &&
                "$ANOLISA_BIN" adapter disable "$COMPONENT" "$AGENT" >/dev/null 2>&1 &&
                ! is_managed_link; then
                cleared_receipt=1
            fi
            if is_managed_link; then
                rm "$PLUGIN_LINK"
            fi
            echo "[${COMPONENT}] ${AGENT}: removed the now-unsupported registration at ${PLUGIN_LINK}."
            if [ "$cleared_receipt" -eq 0 ]; then
                # stdout, not stderr: this branch exits 0, and a caller that
                # captures only stdout (the npm postinstall) must still be able to
                # report that the cleanup was partial.
                echo "[${COMPONENT}] ${AGENT}: partial cleanup: the link is removed, but this script"
                echo "[${COMPONENT}] ${AGENT}: does not read or write ANOLISA receipts, so a receipt may"
                echo "[${COMPONENT}] ${AGENT}: still claim it. Reconcile with:"
                echo "[${COMPONENT}] ${AGENT}:   anolisa adapter disable ${COMPONENT} ${AGENT}"
            fi
        fi
    elif [ -e "$PLUGIN_LINK" ] || [ -L "$PLUGIN_LINK" ]; then
        echo "[${COMPONENT}] ${AGENT}: WARNING: leaving unmanaged path untouched: ${PLUGIN_LINK}" >&2
    fi
    echo "[${COMPONENT}] ${AGENT}: skipping plugin installation."
    exit 0
fi

# Reconcile mode never registers the plugin; it only clears a registration the
# supported-version floor invalidated. Package postinstall hooks run it after
# replacing the shared adapter tree in place, so an upgrade cannot leave an old
# host loading a module its loader cannot initialize -- and cannot register a
# host the user never enabled.
RECONCILE_ONLY="${ANOLISA_ADAPTER_RECONCILE:-0}"

if [ -e "$PLUGIN_LINK" ] || [ -L "$PLUGIN_LINK" ]; then
    if is_managed_link; then
        echo "[${COMPONENT}] ${AGENT} plugin already installed at ${PLUGIN_LINK}."
        exit 0
    fi
    if [ "$RECONCILE_ONLY" = "1" ]; then
        echo "[${COMPONENT}] ${AGENT}: leaving unmanaged path untouched: ${PLUGIN_LINK}"
        exit 0
    fi
    echo "[${COMPONENT}] ERROR: refusing to replace unmanaged path: ${PLUGIN_LINK}" >&2
    exit 1
fi

if [ "$RECONCILE_ONLY" = "1" ]; then
    echo "[${COMPONENT}] ${AGENT}: no Tokenless registration to reconcile."
    exit 0
fi

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    # Every write the real path performs, conditionally where it acts
    # conditionally: the plugin directory is created only when it is missing,
    # while the link is the write that always follows.
    if [ ! -d "$PLUGIN_DIR" ]; then
        echo "DRY-RUN: mkdir -p ${PLUGIN_DIR}"
    fi
    echo "DRY-RUN: ln -s ${PLUGIN_SOURCE} ${PLUGIN_LINK}"
    exit 0
fi

mkdir -p "$PLUGIN_DIR"
ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"
if ! is_managed_link; then
    echo "[${COMPONENT}] ERROR: failed to verify OpenCode plugin link: ${PLUGIN_LINK}" >&2
    exit 1
fi

echo "[${COMPONENT}] ${AGENT} plugin installed at ${PLUGIN_LINK}."
echo "[${COMPONENT}] Restart OpenCode to load the plugin."
