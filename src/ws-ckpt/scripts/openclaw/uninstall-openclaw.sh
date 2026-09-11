#!/bin/bash

set -euo pipefail

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

help_lists_flag() {
    local help_text="$1"
    local flag="$2"

    grep -Eq "(^|[^[:alnum:]_.-])${flag}([^[:alnum:]_.-]|$)" <<<"$help_text"
}

is_unset_config_path() {
    local output="$1"

    [[ "$output" == *"valid but unset"* ]] || [[ "$output" == *"Config path not found"* ]]
}

if [ "$DRY_RUN" = "1" ]; then
    echo "DRY-RUN: env -u OPENCLAW_HOME OPENCLAW_STATE_DIR=$OPENCLAW_STATE_DIR $OPENCLAW_BIN plugins uninstall $PLUGIN_ID --force"
    echo "DRY-RUN: rm -rf ${OPENCLAW_STATE_DIR%/}/extensions/ws-ckpt/"
    echo "DRY-RUN: read effective tools.allow and tools.alsoAllow via '$OPENCLAW_BIN config get <path> --json'"
    echo "DRY-RUN: write the filtered active allowlist via '$OPENCLAW_BIN config set <path> <filtered-json>' (conditional when supported; skipped for legacy \$include configs)"
    echo "DRY-RUN: rm -rf $SKILL_DST"
    exit 0
fi

# 1. Uninstall plugin if openclaw is available
if command -v "$OPENCLAW_BIN" &>/dev/null; then
    if ! env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
        "$OPENCLAW_BIN" plugins uninstall "$PLUGIN_ID" --force 2>/dev/null; then
        echo "WARN: OpenClaw could not unregister the ws-ckpt plugin; plugin configuration may remain" >&2
    fi
fi
rm -rf "${OPENCLAW_STATE_DIR%/}/extensions/ws-ckpt/"
echo "openclaw ws-ckpt plugin files removed"

# 2. Remove ws-ckpt-* entries through OpenClaw's conditional config mutation path.
if command -v "$OPENCLAW_BIN" &>/dev/null; then
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
            config_set_args=(config set "$config_field" "$filtered_allow")
            config_set_help="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
                "$OPENCLAW_BIN" config set --help 2>/dev/null)" && help_rc=0 || help_rc=$?
            if [ "$help_rc" != "0" ]; then
                config_set_args=()
                echo "WARN: could not determine OpenClaw config write capabilities; ws-ckpt-* entries may remain" >&2
            elif help_lists_flag "$config_set_help" "--expect-current-json"; then
                config_set_args+=(--expect-current-json "$current_allow")
            elif help_lists_flag "$config_set_help" "--expect-current-absent"; then
                config_set_args=()
                echo "WARN: OpenClaw lacks the conditional config mode required for $config_field; ws-ckpt-* entries may remain" >&2
            elif ! help_lists_flag "$config_set_help" "--json"; then
                config_set_args=()
                echo "WARN: OpenClaw lacks a supported JSON config write mode; ws-ckpt-* entries may remain" >&2
            elif [ -f "$OPENCLAW_CONFIG" ] && grep -Fq '$include' "$OPENCLAW_CONFIG"; then
                config_set_args=()
                echo "WARN: $OPENCLAW_CONFIG uses \$include but this OpenClaw version lacks safe conditional config writes; remove ws-ckpt entries from the owning include file manually" >&2
            else
                config_set_args+=(--json)
            fi
            if [ "${#config_set_args[@]}" -gt 0 ]; then
                if env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
                    "$OPENCLAW_BIN" "${config_set_args[@]}"; then
                    echo "removed ws-ckpt entries from $config_field"
                else
                    echo "WARN: could not remove ws-ckpt entries from $config_field via 'openclaw config set'" >&2
                fi
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
