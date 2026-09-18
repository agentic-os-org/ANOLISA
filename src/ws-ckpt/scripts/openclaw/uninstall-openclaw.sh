#!/bin/bash

set -euo pipefail

# shellcheck source=lib-openclaw.sh
source "$(dirname "$0")/lib-openclaw.sh"

OPENCLAW_HOME="${OPENCLAW_HOME:-$HOME/.openclaw}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-$OPENCLAW_HOME}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"
OPENCLAW_HOME="${OPENCLAW_HOME%/}"
OPENCLAW_BIN="${OPENCLAW_BIN:-openclaw}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"
SKILL_DST="${OPENCLAW_STATE_DIR%/}/skills/ws-ckpt"
PLUGIN_ID="ws-ckpt"
OPENCLAW_CONFIG="${OPENCLAW_CONFIG_PATH:-${OPENCLAW_STATE_DIR}/openclaw.json}"
if [[ "$OPENCLAW_CONFIG" == "~" || "$OPENCLAW_CONFIG" == "~/"* ]]; then
    OPENCLAW_CONFIG="${OPENCLAW_CONFIG/#\~/$HOME}"
fi

if [ "$DRY_RUN" = "1" ]; then
    echo "DRY-RUN: probe '$OPENCLAW_BIN --version' before config cleanup"
    echo "DRY-RUN: when the detected version and root config permit safe writes, run 'env -u OPENCLAW_HOME OPENCLAW_STATE_DIR=$OPENCLAW_STATE_DIR $OPENCLAW_BIN plugins uninstall $PLUGIN_ID --force'"
    echo "DRY-RUN: rm -rf ${OPENCLAW_STATE_DIR%/}/extensions/ws-ckpt/"
    echo "DRY-RUN: read effective tools.allow and tools.alsoAllow via '$OPENCLAW_BIN config get <path> --json'"
    echo "DRY-RUN: write the filtered active allowlist via '$OPENCLAW_BIN config set <path> <filtered-json>' when safe for the detected version"
    echo "DRY-RUN: rm -rf $SKILL_DST"
    exit 0
fi

openclaw_config_mutation_ok=0
unsafe_cleanup_reason=""
if command -v "$OPENCLAW_BIN" &>/dev/null; then
    if ! probe_openclaw_compat "$OPENCLAW_BIN" "$OPENCLAW_STATE_DIR"; then
        unsafe_cleanup_reason="$OPENCLAW_COMPAT_ERROR"
    elif [ "$OPENCLAW_COMPAT_MODE" = "plain-json" ] \
        && root_config_uses_include "$OPENCLAW_CONFIG"; then
        unsafe_cleanup_reason="$OPENCLAW_CONFIG uses \$include, but OpenClaw $OPENCLAW_VERSION cannot update it safely"
    else
        openclaw_config_mutation_ok=1
    fi

    if [ "$openclaw_config_mutation_ok" = "1" ]; then
        if ! env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
            "$OPENCLAW_BIN" plugins uninstall "$PLUGIN_ID" --force 2>/dev/null; then
            echo "WARN: OpenClaw could not unregister the ws-ckpt plugin; plugin configuration may remain" >&2
        fi
    else
        echo "WARN: skipping OpenClaw plugin unregister and allowlist cleanup: $unsafe_cleanup_reason. If OpenClaw is still in use, remove its ws-ckpt plugin registration and ws-ckpt-* entries from tools.allow/tools.alsoAllow manually, or upgrade OpenClaw and re-run this uninstall." >&2
    fi
else
    echo "WARN: openclaw CLI ('$OPENCLAW_BIN') not found; skipping plugin unregister and allowlist cleanup. If OpenClaw is still in use, remove its ws-ckpt plugin registration and ws-ckpt-* entries from tools.allow/tools.alsoAllow manually, or re-run this uninstall with the CLI on PATH." >&2
fi
rm -rf "${OPENCLAW_STATE_DIR%/}/extensions/ws-ckpt/"
echo "openclaw ws-ckpt plugin files removed"

# 2. Remove ws-ckpt-* entries only when the detected version has a safe write path.
if [ "$openclaw_config_mutation_ok" = "1" ]; then
    allow_value="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
        "$OPENCLAW_BIN" config get tools.allow --json 2>&1)" && allow_rc=0 || allow_rc=$?
    if [ "$allow_rc" = "0" ]; then
        allow_state="value"
    elif is_unset_config_path "$allow_value"; then
        allow_state="absent"
        allow_value="[]"
    else
        allow_state="error"
    fi

    also_allow_value=""
    also_allow_state="error"
    if [ "$allow_state" != "error" ]; then
        also_allow_value="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
            "$OPENCLAW_BIN" config get tools.alsoAllow --json 2>&1)" && also_allow_rc=0 || also_allow_rc=$?
        if [ "$also_allow_rc" = "0" ]; then
            also_allow_state="value"
        elif is_unset_config_path "$also_allow_value"; then
            also_allow_state="absent"
            also_allow_value="[]"
        fi
    fi

    if [ "$allow_state" = "error" ] || [ "$also_allow_state" = "error" ]; then
        echo "WARN: could not read effective tools allowlists via 'openclaw config get'; ws-ckpt-* entries may remain" >&2
    else
        selection="$(node -e '
function parse(state, raw) {
    if (state === "absent") return [];
    var value = JSON.parse(raw);
    if (!Array.isArray(value)) throw new Error("not an array");
    return value.map(String);
}
try {
    var allow = parse(process.argv[1], process.argv[2]);
    var alsoAllow = parse(process.argv[3], process.argv[4]);
    var field;
    var current;
    if (allow.some(function (entry) { return entry.startsWith("ws-ckpt-"); })) {
        field = "tools.allow";
        current = allow;
    } else if (alsoAllow.some(function (entry) { return entry.startsWith("ws-ckpt-"); })) {
        field = "tools.alsoAllow";
        current = alsoAllow;
    } else {
        process.exit(2);
    }
    var filtered = current.filter(function (entry) { return !entry.startsWith("ws-ckpt-"); });
    process.stdout.write([field, JSON.stringify(current), JSON.stringify(filtered)].join("\n"));
} catch (error) { process.exit(3); }
' "$allow_state" "$allow_value" "$also_allow_state" "$also_allow_value" 2>/dev/null)" && rc=0 || rc=$?
        if [ "$rc" = "0" ]; then
            mapfile -t selected <<<"$selection"
            config_field="${selected[0]}"
            current_allow="${selected[1]}"
            filtered_allow="${selected[2]}"
            if ! prepare_openclaw_config_set \
                "$OPENCLAW_BIN" "$OPENCLAW_STATE_DIR" "$OPENCLAW_CONFIG" \
                "$config_field" "$filtered_allow" "value" "$current_allow"; then
                echo "WARN: $OPENCLAW_CONFIG_WRITE_ERROR; ws-ckpt-* entries may remain" >&2
            elif env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
                "$OPENCLAW_BIN" "${OPENCLAW_CONFIG_SET_ARGS[@]}"; then
                echo "removed ws-ckpt entries from $config_field"
            else
                echo "WARN: could not remove ws-ckpt entries from $config_field via 'openclaw config set'" >&2
            fi
        elif [ "$rc" != "2" ]; then
            echo "WARN: 'openclaw config get' returned an invalid tools allowlist value; ws-ckpt-* entries may remain" >&2
        fi
    fi
fi

# 3. Remove skill if exists
if [ -d "$SKILL_DST" ]; then
    rm -rf "$SKILL_DST"
    echo "skill removed from $SKILL_DST"
fi
