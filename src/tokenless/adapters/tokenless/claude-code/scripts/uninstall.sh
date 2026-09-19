#!/usr/bin/env bash
# uninstall.sh — Remove the tokenless plugin and the anolisa marketplace via
# the official `claude plugin` CLI. Falls back to manual cleanup of the
# settings.json + plugin cache when claude is unavailable (e.g. RPM %preun
# running after the user uninstalled claude themselves).
set -euo pipefail

AGENT="${ANOLISA_TARGET:-claude-code}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"

MARKETPLACE_NAME="anolisa-${COMPONENT}"
PLUGIN_ID="${COMPONENT}@${MARKETPLACE_NAME}"

CLAUDE_BIN="${CLAUDE_BIN:-claude}"
SETTINGS="$HOME/.claude/settings.json"
export PATH="$HOME/.local/bin:/usr/local/bin:$PATH"

echo "[${COMPONENT}] Uninstalling ${AGENT} plugin..."

# Whether the CLI can answer at all. Without settings.json there is no second
# witness, so a CLI that cannot even report its version leaves nothing to confirm
# the removal with — and "cannot confirm" must not be reported as "removed".
claude_cli_usable() {
    "$CLAUDE_BIN" --version >/dev/null 2>&1 && return 0
    "$CLAUDE_BIN" version >/dev/null 2>&1 && return 0
    "$CLAUDE_BIN" --help >/dev/null 2>&1 && return 0
    return 1
}

if command -v "$CLAUDE_BIN" &>/dev/null; then
    "$CLAUDE_BIN" plugin uninstall "$PLUGIN_ID" 2>&1 || true
    "$CLAUDE_BIN" plugin marketplace remove "$MARKETPLACE_NAME" 2>&1 || true
    # Verify rather than trust: both statuses above are swallowed because "was not
    # installed" and "refused" are indistinguishable by exit code alone. The caller
    # deletes the adapter resources as soon as this script returns 0, so a
    # registration that survived would be left pointing at nothing.
    CLI_FAILED=0
    if [ ! -f "$SETTINGS" ] && ! claude_cli_usable; then
        echo "[${COMPONENT}] ERROR: $SETTINGS does not exist and ${CLAUDE_BIN} cannot answer --version," >&2
        echo "[${COMPONENT}] ERROR: so nothing confirms the plugin was removed. Refusing to report success." >&2
        CLI_FAILED=1
    fi
    if [ -f "$SETTINGS" ]; then
        if command -v jq &>/dev/null; then
            # jq failing means settings.json is not parseable, which is not the same
            # as "the entries are absent" — an empty answer from a broken read used
            # to be reported as a clean uninstall.
            if ! ep="$(jq -r --arg id "$PLUGIN_ID" '.enabledPlugins | type == "object" and has($id)' "$SETTINGS" 2>/dev/null)"; then
                echo "[${COMPONENT}] ERROR: $SETTINGS could not be parsed, so it cannot be confirmed whether ${PLUGIN_ID} is still enabled." >&2
                CLI_FAILED=1
            elif [ "$ep" = "true" ]; then
                echo "[${COMPONENT}] ERROR: ${PLUGIN_ID} is still in .enabledPlugins of $SETTINGS." >&2
                CLI_FAILED=1
            fi
            if ! mk="$(jq -r --arg m "$MARKETPLACE_NAME" '.extraKnownMarketplaces | type == "object" and has($m)' "$SETTINGS" 2>/dev/null)"; then
                echo "[${COMPONENT}] ERROR: $SETTINGS could not be parsed, so it cannot be confirmed whether ${MARKETPLACE_NAME} is still known." >&2
                CLI_FAILED=1
            elif [ "$mk" = "true" ]; then
                echo "[${COMPONENT}] ERROR: ${MARKETPLACE_NAME} is still in .extraKnownMarketplaces of $SETTINGS." >&2
                CLI_FAILED=1
            fi
        else
            # No jq, so no structured read of settings.json — but "cannot verify"
            # is not "verified". Fall back to a literal search for the two names
            # this script itself wrote; both are specific enough (a plugin id and
            # a marketplace name) that a hit means the entry is really there.
            if grep -Fq -- "$PLUGIN_ID" "$SETTINGS" 2>/dev/null; then
                echo "[${COMPONENT}] ERROR: ${PLUGIN_ID} still appears in $SETTINGS (jq unavailable, matched literally)." >&2
                CLI_FAILED=1
            fi
            if grep -Fq -- "$MARKETPLACE_NAME" "$SETTINGS" 2>/dev/null; then
                echo "[${COMPONENT}] ERROR: ${MARKETPLACE_NAME} still appears in $SETTINGS (jq unavailable, matched literally)." >&2
                CLI_FAILED=1
            fi
        fi
    fi
    if [ "$CLI_FAILED" = "1" ]; then
        echo "[${COMPONENT}] ${AGENT} plugin removal is incomplete; remove the entries above and run this script again." >&2
        exit 1
    fi
    echo "[${COMPONENT}] ${AGENT} plugin removed via claude CLI."
    exit 0
fi

echo "[${COMPONENT}] claude CLI not found — falling back to manual cleanup."

# Three states here, and only one of them is success. Without the CLI this script
# is the only thing that can touch settings.json, so "could not edit it" and
# "could not verify it" must not be reported as "removed": the caller deletes the
# adapter resources and the receipt the moment this returns 0, which would leave a
# live .enabledPlugins entry pointing at a directory that no longer exists and no
# receipt to retry from. The gap this closes is a missing jq — the whole cleanup
# block was guarded on it, so with no CLI and no jq the script changed nothing and
# still printed "manual cleanup complete".
MANUAL_FAILED=0
if [ -f "$SETTINGS" ] && ! command -v jq &>/dev/null; then
    # No structured rewrite is possible. A literal search for the two names this
    # script itself wrote still answers the negative case soundly — if neither
    # string is in the file, no entry can reference the plugin — but a hit is a
    # registration this run can neither remove nor verify.
    if grep -Fq -- "$PLUGIN_ID" "$SETTINGS" 2>/dev/null \
       || grep -Fq -- "$MARKETPLACE_NAME" "$SETTINGS" 2>/dev/null; then
        echo "[${COMPONENT}] ERROR: $SETTINGS still references ${PLUGIN_ID} or ${MARKETPLACE_NAME}" >&2
        echo "[${COMPONENT}] ERROR: and jq is unavailable, so it cannot be removed or verified." >&2
        MANUAL_FAILED=1
    fi
fi
if command -v jq &>/dev/null && [ -f "$SETTINGS" ]; then
    # del(.[$key]) silently no-ops on non-object values. Verify shape up-front
    # so a schema change in a future Claude Code release surfaces as a clear
    # warning instead of a silent "cleaned" message that left residue behind.
    EP_TYPE=$(jq -r '.enabledPlugins | type' "$SETTINGS" 2>/dev/null || echo "missing")
    MK_TYPE=$(jq -r '.extraKnownMarketplaces | type' "$SETTINGS" 2>/dev/null || echo "missing")
    if [ "$EP_TYPE" != "object" ] && [ "$EP_TYPE" != "null" ] && [ "$EP_TYPE" != "missing" ]; then
        # Skipping the cleanup is not a clean uninstall: the entry is still there
        # under a shape this script does not understand.
        echo "[${COMPONENT}] ERROR: .enabledPlugins is ${EP_TYPE}, expected object — cleanup skipped," >&2
        echo "[${COMPONENT}] ERROR: so it cannot be confirmed that ${PLUGIN_ID} is gone." >&2
        echo "[${COMPONENT}] ERROR: remove '${PLUGIN_ID}' from $SETTINGS manually." >&2
        MANUAL_FAILED=1
    elif [ "$MK_TYPE" != "object" ] && [ "$MK_TYPE" != "null" ] && [ "$MK_TYPE" != "missing" ]; then
        echo "[${COMPONENT}] ERROR: .extraKnownMarketplaces is ${MK_TYPE}, expected object — cleanup skipped," >&2
        echo "[${COMPONENT}] ERROR: so it cannot be confirmed that ${MARKETPLACE_NAME} is gone." >&2
        echo "[${COMPONENT}] ERROR: remove '${MARKETPLACE_NAME}' from $SETTINGS manually." >&2
        MANUAL_FAILED=1
    else
        tmp="$(mktemp "${SETTINGS}.XXXXXX")"
        # The rewrite status was unchecked, so a read-only or full filesystem
        # reported "cleaned" while settings.json still enabled the plugin.
        if jq --arg id "$PLUGIN_ID" --arg mkt "$MARKETPLACE_NAME" '
            if .enabledPlugins then .enabledPlugins |= del(.[$id]) else . end
            | if .extraKnownMarketplaces then .extraKnownMarketplaces |= del(.[$mkt]) else . end
        ' "$SETTINGS" > "$tmp" 2>/dev/null && mv "$tmp" "$SETTINGS"; then
            # Verify the rewrite, do not trust it: a future schema that moves the
            # keys would otherwise be reported as a clean removal.
            if [ "$(jq -r --arg id "$PLUGIN_ID" '.enabledPlugins | type == "object" and has($id)' "$SETTINGS" 2>/dev/null)" = "true" ] \
               || [ "$(jq -r --arg m "$MARKETPLACE_NAME" '.extraKnownMarketplaces | type == "object" and has($m)' "$SETTINGS" 2>/dev/null)" = "true" ]; then
                echo "[${COMPONENT}] ERROR: ${PLUGIN_ID} or ${MARKETPLACE_NAME} is still in $SETTINGS after the rewrite." >&2
                MANUAL_FAILED=1
            else
                echo "[${COMPONENT}] cleaned $SETTINGS"
            fi
        else
            rm -f "$tmp" 2>/dev/null || true
            echo "[${COMPONENT}] ERROR: $SETTINGS could not be rewritten, so the registration" >&2
            echo "[${COMPONENT}] ERROR: cannot be confirmed removed." >&2
            MANUAL_FAILED=1
        fi
    fi
fi
# Decided *before* the cache is removed. The plugin cache is one of the resources
# the registration points at, so deleting it and then reporting "resources were
# left in place" was a lie: the retry this message promises would have found the
# settings entry still enabling a plugin whose files were already gone.
if [ "$MANUAL_FAILED" = "1" ]; then
    echo "[${COMPONENT}] ${AGENT} plugin removal is incomplete; settings and the plugin cache" >&2
    echo "[${COMPONENT}] were both left in place so this can be retried." >&2
    echo "[${COMPONENT}] Remove '${PLUGIN_ID}' and '${MARKETPLACE_NAME}' from $SETTINGS" >&2
    echo "[${COMPONENT}] (or install jq, or the claude CLI) and run this script again." >&2
    exit 1
fi
rm -rf "$HOME/.claude/plugins/cache/${MARKETPLACE_NAME}" 2>/dev/null || true
echo "[${COMPONENT}] manual cleanup complete."
