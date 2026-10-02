#!/bin/bash

set -euo pipefail

# shellcheck source=lib-discover.sh
source "$(dirname "$0")/lib-discover.sh"
# shellcheck source=lib-openclaw.sh
source "$(dirname "$0")/lib-openclaw.sh"

OPENCLAW_HOME="${OPENCLAW_HOME:-$HOME/.openclaw}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-$OPENCLAW_HOME}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"
OPENCLAW_HOME="${OPENCLAW_HOME%/}"
OPENCLAW_BIN="${OPENCLAW_BIN:-openclaw}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"
SKILL_DST="${OPENCLAW_STATE_DIR%/}/skills/ws-ckpt"
OPENCLAW_CONFIG="${OPENCLAW_CONFIG_PATH:-${OPENCLAW_STATE_DIR}/openclaw.json}"
if [[ "$OPENCLAW_CONFIG" == "~" || "$OPENCLAW_CONFIG" == "~/"* ]]; then
    OPENCLAW_CONFIG="${OPENCLAW_CONFIG/#\~/$HOME}"
fi

# 1. Check OpenClaw compatibility before any mutation. Dry-run should not require the CLI.
if [ "$DRY_RUN" != "1" ]; then
    if ! command -v "$OPENCLAW_BIN" &>/dev/null; then
        echo "ERROR: openclaw is not installed, please install openclaw first"
        exit 1
    fi
    if ! probe_openclaw_compat "$OPENCLAW_BIN" "$OPENCLAW_STATE_DIR"; then
        echo "ERROR: cannot install ws-ckpt: $OPENCLAW_COMPAT_ERROR" >&2
        exit 1
    fi
fi

# 2. Try plugin install (preferred).
if PLUGIN_SRC=$(find_plugin_src openclaw); then
    if [ "$DRY_RUN" = "1" ]; then
        echo "DRY-RUN: probe '$OPENCLAW_BIN --version' and require OpenClaw >=$OPENCLAW_MIN_VERSION"
        echo "DRY-RUN: probe '$OPENCLAW_BIN plugins install --help' for --accept-capabilities"
        echo "DRY-RUN: read effective tools.allow and tools.alsoAllow via '$OPENCLAW_BIN config get <path> --json'"
        echo "DRY-RUN: write the merged active allowlist via '$OPENCLAW_BIN config set <path> <merged-json>' (conditional on >=$OPENCLAW_CONDITIONAL_CONFIG_VERSION; plain JSON only without \$include on older supported versions)"
        echo "DRY-RUN: env -u OPENCLAW_HOME OPENCLAW_STATE_DIR=$OPENCLAW_STATE_DIR $OPENCLAW_BIN plugins install $PLUGIN_SRC --force [--accept-capabilities when supported]"
        echo "DRY-RUN: env -u OPENCLAW_HOME OPENCLAW_STATE_DIR=$OPENCLAW_STATE_DIR $OPENCLAW_BIN plugins enable ws-ckpt"
        exit 0
    fi

    install_args=(plugins install "$PLUGIN_SRC" --force)
    install_help=""
    if install_help="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
        "$OPENCLAW_BIN" plugins install --help 2>/dev/null)" \
        && help_lists_flag "$install_help" "--accept-capabilities"; then
        install_args+=(--accept-capabilities)
    fi

    # Use whichever allowlist the operator already configured; OpenClaw rejects
    # non-empty tools.allow and tools.alsoAllow in the same scope.
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
        echo "ERROR: could not read effective tools allowlists via 'openclaw config get'; refusing a partial ws-ckpt installation" >&2
        exit 1
    fi

    if [ "$OPENCLAW_COMPAT_MODE" = "plain-json" ] \
        && root_config_uses_include "$OPENCLAW_CONFIG"; then
        echo "ERROR: $OPENCLAW_CONFIG uses \$include, but OpenClaw $OPENCLAW_VERSION cannot safely write plugin or config records; refusing a partial ws-ckpt installation. Upgrade OpenClaw before installing the plugin." >&2
        exit 1
    fi

    selection="$(node -e '
var OURS = ["ws-ckpt-checkpoint","ws-ckpt-rollback","ws-ckpt-list","ws-ckpt-delete","ws-ckpt-diff","ws-ckpt-config","ws-ckpt-status"];
function parse(state, raw) {
    if (state === "absent") return [];
    var value = JSON.parse(raw);
    if (!Array.isArray(value)) throw new Error("not an array");
    return value.map(String);
}
try {
    var allow = parse(process.argv[1], process.argv[2]);
    var alsoAllow = parse(process.argv[3], process.argv[4]);
    var useAllow = allow.length > 0;
    var field = useAllow ? "tools.allow" : "tools.alsoAllow";
    var state = useAllow ? process.argv[1] : process.argv[3];
    var current = useAllow ? allow : alsoAllow;
    var missing = OURS.filter(function (tool) { return !current.includes(tool); });
    if (missing.length === 0) process.exit(2);
    process.stdout.write([field, state === "absent" ? "1" : "0", JSON.stringify(current), JSON.stringify(current.concat(missing))].join("\n"));
} catch (error) { process.exit(3); }
' "$allow_state" "$allow_value" "$also_allow_state" "$also_allow_value" 2>/dev/null)" && rc=0 || rc=$?
    if [ "$rc" = "0" ]; then
        mapfile -t selected <<<"$selection"
        config_field="${selected[0]}"
        current_allow_absent="${selected[1]}"
        current_allow="${selected[2]}"
        merged_allow="${selected[3]}"
        current_allow_state="value"
        if [ "$current_allow_absent" = "1" ]; then
            current_allow_state="absent"
        fi
        if ! prepare_openclaw_config_set \
            "$OPENCLAW_BIN" "$OPENCLAW_STATE_DIR" "$OPENCLAW_CONFIG" \
            "$config_field" "$merged_allow" "$current_allow_state" "$current_allow"; then
            echo "ERROR: $OPENCLAW_CONFIG_WRITE_ERROR; refusing a partial ws-ckpt installation" >&2
            exit 1
        fi

        if ! env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
            "$OPENCLAW_BIN" "${OPENCLAW_CONFIG_SET_ARGS[@]}"; then
            echo "ERROR: could not add ws-ckpt tools to $config_field via 'openclaw config set'; refusing a partial ws-ckpt installation" >&2
            exit 1
        fi
    elif [ "$rc" != "2" ]; then
        echo "ERROR: 'openclaw config get' returned an invalid tools allowlist value; refusing a partial ws-ckpt installation" >&2
        exit 1
    fi

    env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
        "$OPENCLAW_BIN" "${install_args[@]}"
    env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" plugins enable ws-ckpt 2>/dev/null || true
    echo "openclaw ws-ckpt plugin installed and enabled successfully (from $PLUGIN_SRC)"
    exit 0
fi

# 3. Fallback to skill install
if SKILL_SRC=$(find_skill_src); then
    if [ "$DRY_RUN" = "1" ]; then
        echo "DRY-RUN: mkdir -p $SKILL_DST"
        echo "DRY-RUN: cp -pr $SKILL_SRC/. $SKILL_DST/"
        exit 0
    fi
    mkdir -p "$SKILL_DST"
    cp -pr "$SKILL_SRC"/. "$SKILL_DST/"
    echo "skill installed to $SKILL_DST (from $SKILL_SRC)"
else
    print_search_error
    exit 1
fi
