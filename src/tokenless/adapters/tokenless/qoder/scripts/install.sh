#!/usr/bin/env bash
# install.sh — Install the native Tokenless plugin for Qoder CLI.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-qoder}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
PLUGIN_DIR="$ADAPTER_DIR/qoder"
PLUGIN_JSON="$PLUGIN_DIR/.qoder-plugin/plugin.json"

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
if [ -z "$QODERCLI" ]; then
    echo "[${COMPONENT}] ERROR: qodercli not found, aborting plugin registration" >&2
    echo "    Install Qoder CLI first: https://qoder.com/cli" >&2
    exit 1
fi

for subcommand in install list uninstall; do
    if ! "$QODERCLI" plugins "$subcommand" --help >/dev/null 2>&1; then
        echo "[${COMPONENT}] ERROR: qodercli lacks required 'plugins ${subcommand}' support" >&2
        exit 1
    fi
done

if ! command -v python3 >/dev/null 2>&1; then
    echo "[${COMPONENT}] ERROR: python3 is required by the Tokenless hooks" >&2
    exit 1
fi
if [ ! -f "$PLUGIN_JSON" ]; then
    echo "[${COMPONENT}] ERROR: Qoder plugin manifest not found: ${PLUGIN_JSON}" >&2
    exit 1
fi

VERSION="$(PLUGIN_JSON="$PLUGIN_JSON" python3 - <<'PYEOF'
import json
import os

with open(os.environ["PLUGIN_JSON"], encoding="utf-8") as manifest_file:
    print(json.load(manifest_file).get("version", "unknown"))
PYEOF
)"

echo "[${COMPONENT}] Installing ${AGENT} plugin v${VERSION}..."

# Validation was added after the core plugin lifecycle commands. Keep it
# optional so older compatible clients can still install, while treating a
# validation failure as a malformed bundle when the capability exists.
if "$QODERCLI" plugins validate --help >/dev/null 2>&1; then
    if ! VALIDATE_OUT="$("$QODERCLI" plugins validate "$PLUGIN_DIR" 2>&1)"; then
        echo "[${COMPONENT}] ERROR: qodercli rejected the native plugin bundle" >&2
        echo "    Output: $VALIDATE_OUT" >&2
        exit 1
    fi
fi

echo "[${COMPONENT}] Registering native plugin with qodercli..."
if ! INSTALL_OUT="$("$QODERCLI" plugins install "$PLUGIN_DIR" --scope user 2>&1)"; then
    echo "[${COMPONENT}] ERROR: qodercli plugins install failed" >&2
    echo "    Output: $INSTALL_OUT" >&2
    exit 1
fi
[ -z "$INSTALL_OUT" ] || echo "$INSTALL_OUT"

if ! PLUGIN_LIST_OUT="$("$QODERCLI" plugins list --json)"; then
    echo "[${COMPONENT}] ERROR: qodercli plugins list failed after installation" >&2
    exit 1
fi

if ! QODER_PLUGIN_LIST="$PLUGIN_LIST_OUT" python3 - <<'PYEOF'
import json
import os
import sys

expected_id = "tokenless@local"
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
    if isinstance(item, dict) and item.get("id") == expected_id
]
if len(matches) != 1:
    print(
        f"expected exactly one {expected_id} inventory entry, found {len(matches)}",
        file=sys.stderr,
    )
    sys.exit(1)

plugin = matches[0]
if plugin.get("scope") != "user":
    print(f"{expected_id} has unexpected scope: {plugin.get('scope')!r}", file=sys.stderr)
    sys.exit(1)
if plugin.get("enabled") is not True:
    print(f"{expected_id} is not enabled", file=sys.stderr)
    sys.exit(1)

resources = plugin.get("resources")
hooks = resources.get("hooks") if isinstance(resources, dict) else None
if not isinstance(hooks, list) or not hooks:
    print(f"{expected_id} loaded no hook resources", file=sys.stderr)
    sys.exit(1)
PYEOF
then
    echo "[${COMPONENT}] ERROR: installed Qoder plugin failed inventory verification" >&2
    exit 1
fi

echo "[${COMPONENT}] ${AGENT} plugin v${VERSION} installed and activated."
