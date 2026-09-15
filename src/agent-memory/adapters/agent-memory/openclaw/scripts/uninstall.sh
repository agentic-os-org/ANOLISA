#!/usr/bin/env bash
# uninstall.sh — Remove agent-memory plugin via OpenClaw CLI + clean config.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-openclaw}"
COMPONENT="${ANOLISA_COMPONENT:-agent-memory}"
PLUGIN_ID="memory-anolisa"
OPENCLAW_HOME="${OPENCLAW_HOME:-$HOME/.openclaw}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-$OPENCLAW_HOME}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"
OPENCLAW_HOME="${OPENCLAW_HOME%/}"
# Resolve the CLI exactly as install.sh does. An operator who installed with a
# supported OPENCLAW_BIN override — an absolute path to a CLI that is not on
# PATH as the literal `openclaw` — must get the same binary back here, or the
# restore below reports "CLI not found" and silently leaves memory-core
# disabled on a host where the configured executable is available.
OPENCLAW_BIN="${OPENCLAW_BIN:-openclaw}"
OPENCLAW_CFG="${OPENCLAW_STATE_DIR}/openclaw.json"

# install.sh disables OpenClaw's bundled memory-core so this plugin can own the
# memory_get / memory_search tool names (#3218) and records that it did so in
# this marker. Read it before anything is removed: it is the only evidence that
# restoring memory-core is this script's job rather than an operator's own
# earlier choice. Skipping the restore would leave the host with no memory
# plugin at all — `plugins uninstall` resets the memory slot to its default
# (memory-core) while config still says enabled=false.
MEMORY_CORE_ID="memory-core"
MEMORY_CORE_MARKER="${OPENCLAW_STATE_DIR}/.anolisa-memory-anolisa-disabled-${MEMORY_CORE_ID}"
RESTORE_MEMORY_CORE=0
if [ -f "$MEMORY_CORE_MARKER" ]; then
    RESTORE_MEMORY_CORE=1
fi

# The marker records that install.sh disabled memory-core. It does not record
# that this plugin still owns the memory slot, and `plugins enable` re-runs
# OpenClaw's exclusive slot selection — so a restore driven by the marker alone
# drags the slot back from a backend the operator chose *after* that install
# (`plugins enable memory-lancedb`, or an explicit plugins.slots.memory edit),
# and the backend they picked then silently stops serving retrieval (#3222).
#
# Read the slot's owner here instead, before `plugins uninstall` runs and before
# the config cleanup below deletes this plugin's own slot entry: after them,
# "this plugin owned it" and "nobody owns it" read the same, and only the former
# is a slot this script may hand back.
#
# Only a positively identified third owner blocks the restore. An empty or
# unreadable answer keeps it — install.sh's asymmetry again, since the slot is
# then either this plugin's (about to be vacated) or nobody's, and skipping the
# restore in that case strands the host with no memory backend at all, whereas
# an unwanted restore costs one `plugins disable`.
MEMORY_SLOT_KEY="plugins.slots.memory"
MEMORY_SLOT_OWNER=""
MEMORY_SLOT_TAKEN_BY=""

# Reduce a config answer to its bare value token: last line, lower-cased, the
# trailing token after any `=`/`:`, quoting and punctuation stripped — the same
# reduction install.sh applies to plugins.entries.memory-core.enabled, so one
# host's rendering classifies identically in both scripts. The `|| ...=""` arms
# keep a missing tail/tr/sed from aborting the run under `set -euo pipefail`: an
# unreadable answer has to degrade to "restore", not to a half-finished
# uninstall.
slot_value() {
    printf '%s' "$1" | tail -n 1 | tr '[:upper:]' '[:lower:]' \
        | sed -e 's/.*[=:]//' -e 's/[]["'\''`,;]//g' -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'
}

if [ "$RESTORE_MEMORY_CORE" = "1" ] && command -v "$OPENCLAW_BIN" &>/dev/null; then
    _slot_raw="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
        "$OPENCLAW_BIN" config get "$MEMORY_SLOT_KEY" 2>/dev/null)" || _slot_raw=""
    MEMORY_SLOT_OWNER="$(slot_value "$_slot_raw")" || MEMORY_SLOT_OWNER=""
fi
# A CLI that cannot answer (no `config get`, unreadable config) is not the same
# as an empty slot, and openclaw.json is the persisted slot the cleanup below
# edits anyway — so read it before concluding that nothing owns the slot.
if [ "$RESTORE_MEMORY_CORE" = "1" ] && [ -z "$MEMORY_SLOT_OWNER" ] && [ -f "$OPENCLAW_CFG" ]; then
    if command -v jq &>/dev/null; then
        _slot_raw="$(jq -r '((.plugins.slots // {}).memory) // empty' \
            "$OPENCLAW_CFG" 2>/dev/null)" || _slot_raw=""
    elif command -v python3 &>/dev/null; then
        _slot_raw="$(python3 -c 'import json, sys
plugins = json.load(open(sys.argv[1])).get("plugins") or {}
print((plugins.get("slots") or {}).get("memory") or "")' \
            "$OPENCLAW_CFG" 2>/dev/null)" || _slot_raw=""
    else
        _slot_raw=""
    fi
    MEMORY_SLOT_OWNER="$(slot_value "$_slot_raw")" || MEMORY_SLOT_OWNER=""
fi
case "$MEMORY_SLOT_OWNER" in
    # Nothing identified as somebody else's: no owner at all, a host that
    # renders "unset" as a bare word, this plugin's own entry, or memory-core
    # itself. Every one of them leaves the restore as the safe default.
    ''|null|none|nil|undefined|nan|'(empty)'|"$PLUGIN_ID"|"$MEMORY_CORE_ID") ;;
    *)
        RESTORE_MEMORY_CORE=0
        MEMORY_SLOT_TAKEN_BY="$MEMORY_SLOT_OWNER"
        ;;
esac

echo "[${COMPONENT}] Removing ${AGENT} plugin..."

if ! command -v "$OPENCLAW_BIN" &>/dev/null; then
    echo "[${COMPONENT}] openclaw CLI not found (OPENCLAW_BIN=${OPENCLAW_BIN}) — removing plugin files manually."
    rm -rf "${OPENCLAW_STATE_DIR}/plugins/${PLUGIN_ID}" 2>/dev/null || true
    rm -rf "${OPENCLAW_STATE_DIR}/extensions/memory-anolisa" 2>/dev/null || true
else
    env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" plugins uninstall "$PLUGIN_ID" --force || true
fi

# Clean openclaw.json config entries (plugins.allow + plugins.entries + plugins.slots).
# Only this plugin's own keys go: a slot the operator moved to another backend
# is theirs, and the slot-owner read above depends on that staying true.
if [ -f "$OPENCLAW_CFG" ]; then
    if command -v jq &>/dev/null; then
        jq '(.plugins.allow // [] | map(select(. != "'"$PLUGIN_ID"'"))) as $allow |
            (.plugins.entries // {} | del(.["'"$PLUGIN_ID"'"])) as $entries |
            (.plugins.slots // {} | to_entries | map(select(.value != "'"$PLUGIN_ID"'")) | from_entries) as $slots |
            .plugins.allow = $allow | .plugins.entries = $entries | .plugins.slots = $slots' \
            "$OPENCLAW_CFG" > "${OPENCLAW_CFG}.tmp" && mv "${OPENCLAW_CFG}.tmp" "$OPENCLAW_CFG"
        echo "[${COMPONENT}] Cleaned ${AGENT} config entries from openclaw.json (via jq)."
    elif command -v python3 &>/dev/null; then
        # Fallback when jq isn't installed: same edit with stdlib JSON.
        python3 - "$OPENCLAW_CFG" "$PLUGIN_ID" <<'PYEOF'
import json, sys
cfg_path, pid = sys.argv[1], sys.argv[2]
with open(cfg_path) as f:
    cfg = json.load(f)
plugins = cfg.setdefault("plugins", {})
plugins["allow"] = [x for x in plugins.get("allow", []) if x != pid]
plugins["entries"] = {k: v for k, v in plugins.get("entries", {}).items() if k != pid}
plugins["slots"] = {k: v for k, v in plugins.get("slots", {}).items() if v != pid}
with open(cfg_path + ".tmp", "w") as f:
    json.dump(cfg, f, indent=2)
import os
os.replace(cfg_path + ".tmp", cfg_path)
PYEOF
        echo "[${COMPONENT}] Cleaned ${AGENT} config entries from openclaw.json (via python3)."
    else
        echo "[${COMPONENT}] WARN: neither jq nor python3 found — openclaw.json may still" >&2
        echo "[${COMPONENT}]       reference '${PLUGIN_ID}'. Edit ${OPENCLAW_CFG} manually." >&2
    fi
fi

# Hand the memory slot back to the backend this plugin displaced, but only when
# install.sh is what disabled it *and* nothing else owns the slot now.
# `plugins enable` also re-runs OpenClaw's exclusive slot selection, so the slot
# returns to its memory-core default in the same step.
if [ "$RESTORE_MEMORY_CORE" = "1" ]; then
    if command -v "$OPENCLAW_BIN" &>/dev/null; then
        if env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" plugins enable "$MEMORY_CORE_ID"; then
            rm -f "$MEMORY_CORE_MARKER"
            echo "[${COMPONENT}] Re-enabled ${MEMORY_CORE_ID}, which install.sh had disabled to free the"
            echo "[${COMPONENT}]       memory_get/memory_search tool names. Run '${OPENCLAW_BIN} gateway restart' to apply."
        else
            echo "[${COMPONENT}] WARNING: '${OPENCLAW_BIN} plugins enable ${MEMORY_CORE_ID}' failed, so the memory slot" >&2
            echo "[${COMPONENT}]          has no plugin behind it. Re-enable ${MEMORY_CORE_ID} manually, then delete" >&2
            echo "[${COMPONENT}]          ${MEMORY_CORE_MARKER}." >&2
        fi
    else
        # Keep the marker: it is the only record left that memory-core is still
        # disabled on this host because of an install this script just undid.
        echo "[${COMPONENT}] WARNING: openclaw CLI not found (OPENCLAW_BIN=${OPENCLAW_BIN}), so ${MEMORY_CORE_ID} is" >&2
        echo "[${COMPONENT}]          still disabled by the install this script just undid. Run '${OPENCLAW_BIN} plugins" >&2
        echo "[${COMPONENT}]          enable ${MEMORY_CORE_ID}' once the CLI is available, then delete" >&2
        echo "[${COMPONENT}]          ${MEMORY_CORE_MARKER}." >&2
    fi
elif [ -n "$MEMORY_SLOT_TAKEN_BY" ]; then
    # A deliberate skip, not a failure — this is the operator's later choice
    # winning, which is the point. The marker is kept on purpose: it still
    # records that memory-core is disabled *because of an anolisa install*, and
    # dropping it would leave a host that later removes ${MEMORY_SLOT_TAKEN_BY}
    # with no memory backend and no record of why.
    echo "[${COMPONENT}] Left ${MEMORY_CORE_ID} disabled: ${MEMORY_SLOT_KEY} now belongs to"
    echo "[${COMPONENT}]       '${MEMORY_SLOT_TAKEN_BY}', which was selected after install.sh disabled ${MEMORY_CORE_ID}."
    echo "[${COMPONENT}]       '${OPENCLAW_BIN} plugins enable ${MEMORY_CORE_ID}' re-runs OpenClaw's exclusive slot"
    echo "[${COMPONENT}]       selection, so it would take the memory slot — and the memory_get/memory_search"
    echo "[${COMPONENT}]       tool names — back from '${MEMORY_SLOT_TAKEN_BY}'. Run it yourself if that is what"
    echo "[${COMPONENT}]       you want, then delete ${MEMORY_CORE_MARKER}."
fi

echo "[${COMPONENT}] ${AGENT} plugin removed."
