#!/usr/bin/env bash
# uninstall.sh — Remove only the Tokenless-owned hook entries from
# WorkBuddy's user-level settings.json. User-configured hooks and every other
# settings key are never touched.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-workbuddy}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
CODEBUDDY_HOME="${CODEBUDDY_HOME:-$HOME/.codebuddy}"

if [ ! -d "$CODEBUDDY_HOME" ]; then
    echo "[${COMPONENT}] WorkBuddy/CodeBuddy not detected (no ${CODEBUDDY_HOME}) — nothing to uninstall."
    exit 0
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "[${COMPONENT}] ERROR: python3 is required to edit settings.json" >&2
    exit 1
fi

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    echo "DRY-RUN: remove tokenless hook entries from ${CODEBUDDY_HOME}/settings.json"
    exit 0
fi

CODEBUDDY_HOME="$CODEBUDDY_HOME" python3 - <<'PYEOF'
import json
import os
import stat
import sys
import tempfile

MARKER = "TOKENLESS_AGENT_ID=workbuddy"
codebuddy_home = os.environ["CODEBUDDY_HOME"]
config_path = os.path.join(codebuddy_home, "settings.json")

if not os.path.exists(config_path):
    print(f"[tokenless] no settings.json in {codebuddy_home} — nothing to uninstall.")
    sys.exit(0)

with open(config_path, encoding="utf-8") as handle:
    raw = handle.read().strip()
if not raw:
    sys.exit(0)
try:
    config = json.loads(raw)
except json.JSONDecodeError as error:
    print(f"existing {config_path} is not valid JSON: {error}", file=sys.stderr)
    sys.exit(1)
if not isinstance(config, dict):
    print(f"unexpected root type in {config_path}", file=sys.stderr)
    sys.exit(1)

hooks = config.get("hooks")
if not isinstance(hooks, dict) or MARKER not in json.dumps(hooks):
    print(f"[tokenless] no tokenless hooks in {config_path} — nothing to uninstall.")
    sys.exit(0)


def is_tokenless_hook(entry):
    return (
        isinstance(entry, dict)
        and isinstance(entry.get("command"), str)
        and MARKER in entry["command"]
    )


for event in list(hooks.keys()):
    groups = hooks[event]
    if not isinstance(groups, list):
        continue
    kept = []
    for group in groups:
        if not isinstance(group, dict):
            kept.append(group)
            continue
        inner = group.get("hooks")
        if not isinstance(inner, list):
            kept.append(group)
            continue
        remaining = [entry for entry in inner if not is_tokenless_hook(entry)]
        if remaining:
            pruned = dict(group)
            pruned["hooks"] = remaining
            kept.append(pruned)
        elif not any(is_tokenless_hook(entry) for entry in inner):
            kept.append(group)
    hooks[event] = kept

# Preserve the existing file mode on rewrite: settings.json sits beside
# settings.json.env, where CodeBuddy officially allows CODEBUDDY_API_KEY
# and auth tokens, so a default-mode temp file (0644 under umask 022)
# must not widen a 0600 config on replace. New files are created 0600.
existing_mode = stat.S_IMODE(os.stat(config_path).st_mode)
fd, tmp_path = tempfile.mkstemp(
    dir=codebuddy_home, prefix=".settings.json.", suffix=".tmp"
)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(config, handle, ensure_ascii=False, indent=2)
        handle.write("\n")
    os.chmod(tmp_path, existing_mode)
    os.replace(tmp_path, config_path)
except BaseException:
    try:
        os.unlink(tmp_path)
    except OSError:
        pass
    raise
print(f"[tokenless] tokenless hooks removed from {config_path}.")
PYEOF

echo "[${COMPONENT}] ${AGENT} hooks uninstalled."
