#!/usr/bin/env bash
# Uninstall the tokenless Codex plugin and clean up plugin data.
#
# Removes:
#   - Codex plugin registration (codex plugin remove)
#   - Codex marketplace entry (codex plugin marketplace remove)
#   - Marketplace symlink directory
#   - The tokenless binary from the install prefix (skipped in
#     deregistration-only mode, see TOKENLESS_DEREGISTER_ONLY below)
#
# NOTE: This script does NOT remove $HOME/.tokenless/ — that directory
# holds the SQLite stats DB and rewrite context shared by every tokenless
# adapter (cosh, qoder, hermes, openclaw, claude-code). Removing it here
# would destroy data belonging to plugins still installed.
#
# Usage:
#   ./uninstall.sh                    # Interactive (asks confirmation)
#   ./uninstall.sh --non-interactive  # CI / automated (no confirmation)
#
# Environment variables:
#   TOKENLESS_DEREGISTER_ONLY  Set to 1 to remove the Codex registration only
#                              and leave $PREFIX/bin/tokenless alone. This is
#                              what src/tokenless/scripts/{install,uninstall}.sh
#                              set when they deregister adapters before dropping
#                              the adapter resources: those scripts own the
#                              decision about the component binary (the
#                              receipt-driven uninstaller keeps it when another
#                              installation has taken the path over), so this
#                              script must not revisit it.

set -euo pipefail

INTERACTIVE=1
if [[ "${1:-}" == "--non-interactive" ]]; then
    INTERACTIVE=0
fi

# The shared helpers own the HOME normalization (absolute or empty) every
# root below derives from, so they are sourced before the roots are resolved.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=./_common.sh
source "$SCRIPT_DIR/_common.sh"

# An explicit prefix is authoritative: silently falling back to HOME would
# remove a different installation than the one the caller named.
PREFIX_OVERRIDE="$(abs_or_empty "${TOKENLESS_INSTALL_PREFIX:-}")"
if [[ -n "${TOKENLESS_INSTALL_PREFIX:-}" && -z "$PREFIX_OVERRIDE" ]]; then
    echo "[tokenless] TOKENLESS_INSTALL_PREFIX must be an absolute path" >&2
    exit 1
fi
PREFIX="${PREFIX_OVERRIDE:-${USER_HOME:+$USER_HOME/.local}}"
if [[ -z "$PREFIX" ]]; then
    echo "[tokenless] TOKENLESS_INSTALL_PREFIX or HOME must be set" >&2
    exit 1
fi
BINDIR="$PREFIX/bin"
CODEX_BIN="${CODEX_BIN:-}"
MARKETPLACE_NAME="anolisa-tokenless"
PLUGIN_ID="tokenless@${MARKETPLACE_NAME}"
# A relative XDG_DATA_HOME is ignored, as the XDG basedir spec requires.
MARKETPLACE_ROOT="$(abs_or_empty "${XDG_DATA_HOME:-}")"
if [[ -z "$MARKETPLACE_ROOT" && -n "$USER_HOME" ]]; then
    MARKETPLACE_ROOT="$USER_HOME/.local/share"
fi
MARKETPLACE_DIR=""
if [[ -n "$MARKETPLACE_ROOT" ]]; then
    MARKETPLACE_DIR="$MARKETPLACE_ROOT/anolisa/codex-marketplace"
fi
CODEX_HOME_DIR="$(abs_or_empty "${CODEX_HOME:-}")"
if [[ -z "$CODEX_HOME_DIR" && -n "$USER_HOME" ]]; then
    CODEX_HOME_DIR="$USER_HOME/.codex"
fi
CODEX_CONFIG=""
if [[ -n "$CODEX_HOME_DIR" ]]; then
    CODEX_CONFIG="$CODEX_HOME_DIR/config.toml"
fi
DRY_RUN="${ANOLISA_DRY_RUN:-0}"

DEREGISTER_ONLY=0
if [[ "${TOKENLESS_DEREGISTER_ONLY:-0}" == "1" ]]; then
    DEREGISTER_ONLY=1
fi

echo "[tokenless] Codex plugin uninstall"

# 1. Remove codex plugin registration
CODEX_BIN="$(resolve_codex)"

if [ "$DRY_RUN" = "1" ]; then
    # Preview before the first removal: a dry run must leave the codex
    # registrations, the marketplace tree and the component binary alone. The
    # two branches of the real path differ (the CLI one asks codex, the no-CLI
    # one reads config.toml and refuses to guess), so the preview names the one
    # the resolved state takes instead of claiming both.
    if [ -n "$CODEX_BIN" ]; then
        echo "DRY-RUN: ${CODEX_BIN} plugin remove tokenless@${MARKETPLACE_NAME} (only when it is registered)"
        echo "DRY-RUN: ${CODEX_BIN} plugin marketplace remove ${MARKETPLACE_NAME} (only when it is registered)"
        if [[ -n "$MARKETPLACE_DIR" ]]; then
            echo "DRY-RUN: rm -rf ${MARKETPLACE_DIR} (only when it is there and the registration is confirmed gone)"
        else
            echo "DRY-RUN: neither HOME nor XDG_DATA_HOME is set, so there is no marketplace directory to remove"
        fi
    else
        # No CLI to ask, so the real path decides on config.toml alone; the same
        # read-only check decides here, or the preview would report a kept
        # directory while the run deletes it.
        if [[ -z "$CODEX_CONFIG" ]]; then
            echo "DRY-RUN: no codex CLI resolved and the Codex config cannot be located, so the real run keeps"
            echo "DRY-RUN: the registration and exits 1"
            if [[ -n "$MARKETPLACE_DIR" ]]; then
                echo "DRY-RUN: ${MARKETPLACE_DIR} is kept"
            else
                echo "DRY-RUN: no marketplace directory is known, so nothing is removed there"
            fi
        elif [[ -f "$CODEX_CONFIG" ]] \
            && grep -Eq "tokenless|${MARKETPLACE_NAME}" "$CODEX_CONFIG" 2>/dev/null; then
            echo "DRY-RUN: no codex CLI resolved and ${CODEX_CONFIG} still names tokenless, so the real run keeps"
            if [[ -n "$MARKETPLACE_DIR" ]]; then
                echo "DRY-RUN: ${MARKETPLACE_DIR} and exits 1"
            else
                echo "DRY-RUN: the registration and exits 1 (no marketplace directory is known)"
            fi
        else
            echo "DRY-RUN: no codex CLI resolved and Codex records no tokenless registration"
            if [[ -n "$MARKETPLACE_DIR" ]]; then
                echo "DRY-RUN: rm -rf ${MARKETPLACE_DIR} (only when it is there), and the real run exits 0"
            else
                echo "DRY-RUN: (no marketplace directory is known), and the real run exits 0"
            fi
        fi
    fi
    # The component binary is a separate decision: the real path removes it
    # whenever TOKENLESS_DEREGISTER_ONLY is unset, whether or not the
    # registration could be confirmed, so it is previewed in every branch.
    if [ "$DEREGISTER_ONLY" = "0" ]; then
        echo "DRY-RUN: rm -f ${BINDIR}/tokenless (only when it is there; interactive mode asks first; without a terminal it is kept)"
    fi
    exit 0
fi

DEREGISTER_FAILED=0
# A query that fails is not an empty answer. Piping `plugin list` straight into
# grep turns a broken or refusing CLI into "nothing is registered", which reads as
# success — and the caller deletes the adapter resources the moment this script
# returns 0. So the query status is captured, and an unusable query is reported as
# "cannot confirm" rather than as "confirmed absent".
codex_list() {  # codex_list <args...> -> prints the listing, status = CLI status
    "$CODEX_BIN" "$@" 2>/dev/null
}

# `codex plugin list` is a table of every plugin the registered marketplaces
# offer, in whatever state they are in, and it is headed by the marketplace
# names:
#
#   Marketplace `anolisa-tokenless`
#   /home/.../codex-marketplace/.agents/plugins/marketplace.json
#
#   PLUGIN                       STATUS              VERSION  SOURCE
#   tokenless@anolisa-tokenless  installed, enabled  local    /home/.../tokenless
#
# So "tokenless shows up in the listing" is not evidence about the registration:
# that header matches too, and it is printed even when the plugin was never
# added. `plugin remove` only flips this plugin's STATUS to `not installed` —
# codex-cli 0.154.0 keeps the row until the marketplace that ships it is removed,
# which is the next thing this script does. Reading the surviving row as "still
# registered" turned every successful uninstall into exit 1 and, through
# TOKENLESS_DEREGISTER_ONLY, stopped the component uninstaller from dropping the
# adapter resources and the receipt. Only a row for this plugin whose status is
# not `not installed` counts as a surviving registration. Other marketplaces
# may offer a plugin with the same name; a bare row only identifies our plugin
# inside our marketplace section. Qualified ids do not need section context.
codex_plugin_registered() {  # <listing> -> 0 while an active plugin row survives
    local rows
    rows="$(printf '%s\n' "$1" \
        | awk -v id="$PLUGIN_ID" -v market="$MARKETPLACE_NAME" '
            $1 == "Marketplace" {
                section = $2
                gsub(/`/, "", section)
                next
            }
            ($1 == id || ($1 == "tokenless" && section == market)) &&
            !(tolower($2) == "not" && tolower($3) ~ /^installed,?$/) { print }
        ')"
    [[ -n "$rows" ]]
}

if [[ -n "$CODEX_BIN" ]]; then
    if plugin_listing="$(codex_list plugin list)"; then
        if codex_plugin_registered "$plugin_listing"; then
            echo "[tokenless] Removing codex plugin '${PLUGIN_ID}'..."
            "$CODEX_BIN" plugin remove "${PLUGIN_ID}" 2>&1 || true
            # The removal status is swallowed on purpose — "was not registered" and
            # "refused" both come back non-zero — so re-ask instead of trusting it.
            if after_listing="$(codex_list plugin list)"; then
                if codex_plugin_registered "$after_listing"; then
                    echo "[tokenless] ERROR: codex still lists the tokenless plugin after 'plugin remove'." >&2
                    DEREGISTER_FAILED=1
                fi
            else
                echo "[tokenless] ERROR: 'codex plugin list' failed after the removal, so the registration cannot be confirmed gone." >&2
                DEREGISTER_FAILED=1
            fi
        fi
    else
        echo "[tokenless] ERROR: 'codex plugin list' failed, so it cannot be confirmed whether the plugin is registered." >&2
        DEREGISTER_FAILED=1
    fi
    if market_listing="$(codex_list plugin marketplace list)"; then
        if printf '%s\n' "$market_listing" | grep -q "^${MARKETPLACE_NAME}[[:space:]]"; then
            echo "[tokenless] Removing marketplace '${MARKETPLACE_NAME}'..."
            "$CODEX_BIN" plugin marketplace remove "$MARKETPLACE_NAME" 2>&1 || true
            if after_market="$(codex_list plugin marketplace list)"; then
                if printf '%s\n' "$after_market" | grep -q "^${MARKETPLACE_NAME}[[:space:]]"; then
                    echo "[tokenless] ERROR: codex still lists the '${MARKETPLACE_NAME}' marketplace." >&2
                    DEREGISTER_FAILED=1
                fi
            else
                echo "[tokenless] ERROR: 'codex plugin marketplace list' failed after the removal, so it cannot be confirmed gone." >&2
                DEREGISTER_FAILED=1
            fi
        fi
    else
        echo "[tokenless] ERROR: 'codex plugin marketplace list' failed, so it cannot be confirmed whether the marketplace is registered." >&2
        DEREGISTER_FAILED=1
    fi
else
    # No CLI to ask, so neither the plugin nor the marketplace registration can be
    # queried. Both are persisted by Codex itself in $CODEX_HOME/config.toml, which
    # outlives the CLI being temporarily unresolvable — and the caller deletes the
    # shared adapter resources the moment this script returns 0. A surviving entry
    # would then point at a directory that no longer exists, with no receipt left to
    # retry from, so "cannot confirm" is reported as a failure rather than as
    # "confirmed absent". Where Codex has no config at all it was never installed
    # here and nothing was ever registered, which really is a clean state.
    if [[ -z "$CODEX_CONFIG" ]]; then
        # Not knowing where Codex keeps its config is not the same as knowing
        # there is none: HOME and CODEX_HOME are both unusable, so a surviving
        # registration cannot be ruled out and the caller must not delete the
        # shared resources this script's exit 0 releases.
        echo "[tokenless] ERROR: neither HOME nor CODEX_HOME is usable, so the Codex config cannot" >&2
        echo "[tokenless] ERROR: be located and the registration cannot be confirmed gone. Marketplace" >&2
        echo "[tokenless] ERROR: directory kept. Set HOME or CODEX_HOME and run this script again." >&2
        DEREGISTER_FAILED=1
    elif [[ -f "$CODEX_CONFIG" ]] \
       && grep -Eq "tokenless|${MARKETPLACE_NAME}" "$CODEX_CONFIG" 2>/dev/null; then
        echo "[tokenless] ERROR: codex CLI not found and ${CODEX_CONFIG} still mentions" >&2
        echo "[tokenless] ERROR: the tokenless plugin or the ${MARKETPLACE_NAME} marketplace, so the" >&2
        echo "[tokenless] ERROR: registration cannot be confirmed gone. Marketplace directory kept." >&2
        echo "[tokenless] Put codex back on PATH and run this script again." >&2
        DEREGISTER_FAILED=1
    else
        echo "[tokenless] codex CLI not found and Codex records no tokenless"
        echo "[tokenless] registration; nothing to deregister."
    fi
fi

# 2. Remove marketplace symlink directory — but only once the registration that
#    points into it is known to be gone. Deleting it first would leave a surviving
#    marketplace entry aimed at a directory that no longer exists, and the failure
#    is only reported at the end of this script.
if [[ $DEREGISTER_FAILED -eq 1 ]]; then
    if [[ -n "$MARKETPLACE_DIR" ]]; then
        echo "[tokenless] Keeping marketplace directory ${MARKETPLACE_DIR}: the Codex registration was not removed."
    else
        echo "[tokenless] No marketplace directory is known (HOME and XDG_DATA_HOME are unusable), so nothing is kept there."
    fi
elif [[ -z "$MARKETPLACE_DIR" ]]; then
    echo "[tokenless] neither HOME nor XDG_DATA_HOME is set — no marketplace directory to remove."
elif [[ -d "$MARKETPLACE_DIR" ]]; then
    rm -rf "$MARKETPLACE_DIR"
    echo "[tokenless] Removed marketplace directory: $MARKETPLACE_DIR"
fi

# 3. Remove binary
if [[ $DEREGISTER_ONLY -eq 1 ]]; then
    echo "[tokenless] Deregistration only: keeping $BINDIR/tokenless (its owner decides)."
elif [[ -f "$BINDIR/tokenless" ]]; then
    if [[ $INTERACTIVE -eq 1 ]]; then
        answer=""
        if [[ -t 0 ]]; then
            read -rp "Remove $BINDIR/tokenless? [y/N] " answer
        else
            # A caller that captures this script's output would never see the
            # prompt, and removing a component binary without an answer is not a
            # safe default either. Leave it and say so.
            echo "[tokenless] No terminal to confirm on; keeping $BINDIR/tokenless."
        fi
        if [[ "$answer" =~ ^[Yy]$ ]]; then
            rm -f "$BINDIR/tokenless"
            echo "[tokenless] Removed: $BINDIR/tokenless"
        else
            echo "[tokenless] Skipped: $BINDIR/tokenless"
        fi
    else
        rm -f "$BINDIR/tokenless"
        echo "[tokenless] Removed: $BINDIR/tokenless"
    fi
else
    echo "[tokenless] Binary not found: $BINDIR/tokenless"
fi

if [[ $DEREGISTER_FAILED -eq 1 ]]; then
    echo "[tokenless] Uninstall incomplete: the Codex registration is still in place." >&2
    echo "[tokenless] Remove it, then run this script again:" >&2
    echo "[tokenless]   codex plugin remove ${PLUGIN_ID}" >&2
    echo "[tokenless]   codex plugin marketplace remove ${MARKETPLACE_NAME}" >&2
    exit 1
fi

echo "[tokenless] Uninstall complete."
