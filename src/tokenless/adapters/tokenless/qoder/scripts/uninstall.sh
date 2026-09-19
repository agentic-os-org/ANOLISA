#!/usr/bin/env bash
# uninstall.sh — Remove the native Tokenless plugin from Qoder CLI.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-qoder}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"

find_qodercli() {
    local versioned_glob latest_versioned candidate
    versioned_glob="$HOME/.qoder/bin/qodercli/qodercli-${ANOLISA_QODER_VERSION:-*}"
    # shellcheck disable=SC2086 # Intentional version glob expansion.
    latest_versioned="$(ls -d $versioned_glob 2>/dev/null | sort -V | tail -1 || true)"

    for candidate in "$latest_versioned" \
                     "$HOME/.qoder/bin/qodercli/qodercli" \
                     "qodercli"; do
        [ -n "$candidate" ] || continue
        if [ -x "$candidate" ] || command -v "$candidate" >/dev/null 2>&1; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

QODERCLI="$(find_qodercli || true)"
echo "[${COMPONENT}] Removing ${AGENT} plugin..."

# Whether a user-scope tokenless registration is still on disk. Qoder keeps it in
# its own store under ~/.qoder, and it references this adapter directory, so the
# CLI is the only way to remove it — but its absence is not evidence about it.
qoder_registration_present() {
    local qoder_home="${HOME}/.qoder"
    [ -d "$qoder_home" ] || return 1
    # bin/ holds the CLI itself, never a plugin registration.
    grep -rq --exclude-dir=bin "tokenless@local" "$qoder_home" 2>/dev/null
}

if [ -z "$QODERCLI" ]; then
    # qodercli being unresolvable right now does not mean Qoder is gone, and the
    # caller deletes the shared adapter resources as soon as this script returns 0.
    # Reporting success while a registration survives would leave it aimed at a
    # path that no longer exists, and take the receipt — the only way back — with
    # it. Fail closed instead, and keep the plugin files so this can be retried.
    if qoder_registration_present; then
        echo "[${COMPONENT}] ERROR: qodercli not found and a tokenless@local registration is still" >&2
        echo "[${COMPONENT}] ERROR: present under ${HOME}/.qoder, so it cannot be confirmed removed." >&2
        echo "[${COMPONENT}] Plugin files were left in place; put qodercli back on PATH and re-run:" >&2
        echo "[${COMPONENT}]   qodercli plugins uninstall tokenless --scope user" >&2
        exit 1
    fi
    echo "[${COMPONENT}] qodercli not found and no tokenless registration is present under"
    echo "[${COMPONENT}] ${HOME}/.qoder; nothing to unregister."
    exit 0
fi
for subcommand in list uninstall; do
    if ! "$QODERCLI" plugins "$subcommand" --help >/dev/null 2>&1; then
        echo "[${COMPONENT}] ERROR: qodercli lacks required 'plugins ${subcommand}' support" >&2
        exit 1
    fi
done
if ! command -v python3 >/dev/null 2>&1; then
    echo "[${COMPONENT}] ERROR: python3 is required to verify Qoder plugin state" >&2
    exit 1
fi

read_plugin_state() {
    local plugin_list_out
    if ! plugin_list_out="$("$QODERCLI" plugins list --json 2>&1)"; then
        echo "[${COMPONENT}] ERROR: qodercli plugins list failed during uninstall" >&2
        echo "    Output: $plugin_list_out" >&2
        return 1
    fi

    QODER_PLUGIN_LIST="$plugin_list_out" python3 - <<'PYEOF'
import json
import os
import sys

try:
    inventory = json.loads(os.environ["QODER_PLUGIN_LIST"])
except json.JSONDecodeError as error:
    print(f"invalid JSON from qodercli plugins list: {error}", file=sys.stderr)
    sys.exit(1)

if isinstance(inventory, dict):
    inventory = inventory.get("plugins")
if not isinstance(inventory, list):
    print("qodercli plugin inventory is not a list", file=sys.stderr)
    sys.exit(1)

matches = [
    item
    for item in inventory
    if isinstance(item, dict)
    and item.get("id") == "tokenless@local"
    and item.get("scope") == "user"
]
if len(matches) > 1:
    print("multiple user-scoped tokenless@local entries found", file=sys.stderr)
    sys.exit(1)
print("present" if matches else "absent")
PYEOF
}

if ! PLUGIN_STATE="$(read_plugin_state)"; then
    exit 1
fi
if [ "$PLUGIN_STATE" = "absent" ]; then
    echo "[${COMPONENT}] ${AGENT} plugin is already absent."
    exit 0
fi

if ! UNINSTALL_OUT="$("$QODERCLI" plugins uninstall tokenless --scope user 2>&1)"; then
    echo "[${COMPONENT}] ERROR: qodercli plugins uninstall failed" >&2
    echo "    Output: $UNINSTALL_OUT" >&2
    exit 1
fi
[ -z "$UNINSTALL_OUT" ] || echo "$UNINSTALL_OUT"

if ! PLUGIN_STATE="$(read_plugin_state)"; then
    exit 1
fi
if [ "$PLUGIN_STATE" != "absent" ]; then
    echo "[${COMPONENT}] ERROR: user-scoped tokenless plugin remains after uninstall" >&2
    exit 1
fi

echo "[${COMPONENT}] ${AGENT} plugin removed."
