#!/usr/bin/env bash
# uninstall.sh — Remove only the Tokenless-owned hook entries from Trae's
# global hooks.json. User-configured hooks are never touched: an entry counts as
# ours only when its whole command matches the shape install.sh generates, and
# only the events install.sh writes are visited. When nothing matches, the user's
# file is left byte-for-byte alone.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-trae}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"

TRAE_HOMES=()
for home in "$HOME/.trae-cn" "$HOME/.trae"; do
    [ -d "$home" ] && TRAE_HOMES+=("$home")
done
if [ ${#TRAE_HOMES[@]} -eq 0 ]; then
    echo "[${COMPONENT}] Trae not detected (no ~/.trae-cn or ~/.trae) — nothing to uninstall."
    exit 0
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "[${COMPONENT}] ERROR: python3 is required to edit hooks.json" >&2
    exit 1
fi

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    for home in "${TRAE_HOMES[@]}"; do
        echo "DRY-RUN: remove tokenless hook entries from ${home}/hooks.json"
    done
    exit 0
fi

for home in "${TRAE_HOMES[@]}"; do
    TRAE_HOME="$home" python3 - <<'PYEOF'
import json
import os
import shlex
import stat
import sys
import tempfile

MARKER = "TOKENLESS_AGENT_ID=trae"
DISPATCHER_SUFFIX = "trae/hooks/run-hook.sh"
# Keep in sync with trae/hooks/hooks.json and with install.sh: the installer
# only ever writes these scripts, and only into these events, so the removal
# must not look anywhere else.
OWNED_SCRIPTS = frozenset(
    {"rewrite_hook.py", "tool_ready_hook.sh", "compress_response_hook.py"}
)
MANAGED_EVENTS = ("PreToolUse", "PostToolUse")

trae_home = os.environ["TRAE_HOME"]
config_path = os.path.join(trae_home, "hooks.json")


def die(message):
    print(message, file=sys.stderr)
    sys.exit(1)


def nothing_to_do():
    print(f"[tokenless] no tokenless hooks in {config_path} — nothing to uninstall.")
    sys.exit(0)


def is_tokenless_hook(entry):
    """True only for a command install.sh generates.

    The generated command is exactly four shell words:
    ``TOKENLESS_AGENT_ID=trae bash <.../trae/hooks/run-hook.sh> <script>``.
    Anything else — including a user command that merely mentions the marker —
    is user-owned and stays.
    """
    if not isinstance(entry, dict):
        return False
    command = entry.get("command")
    if not isinstance(command, str):
        return False
    try:
        words = shlex.split(command)
    except ValueError:
        return False
    if len(words) != 4:
        return False
    env, interpreter, path, script = words
    if env != MARKER or interpreter != "bash" or script not in OWNED_SCRIPTS:
        return False
    return path == DISPATCHER_SUFFIX or path.endswith("/" + DISPATCHER_SUFFIX)


def default_mode():
    """Mode for a brand-new hooks.json: whatever a plain write would produce.

    An existing file keeps the mode it already had (see resolve_config()); only
    a first-time create falls back to the process umask.
    """
    umask = os.umask(0)
    os.umask(umask)
    return 0o666 & ~umask


def resolve_config(path):
    """Return ``(write_path, mode)`` for the host config at ``path``.

    Mirrors install.sh: a dotfiles-managed ``hooks.json`` symlink is followed so
    the managed path stays a symlink pointing at the same target, and the access
    mode of the file we rewrite is preserved. Anything that is not a regular
    file is rejected before a single byte is written.
    """
    if not os.path.lexists(path):
        return path, default_mode()
    if os.path.islink(path):
        target = os.path.realpath(path)
        if os.path.islink(target):
            die(f"refusing to write {path}: symlink target does not resolve ({target})")
        write_path = target
    else:
        write_path = path
    if os.path.exists(write_path):
        metadata = os.stat(write_path)
        if not stat.S_ISREG(metadata.st_mode):
            die(f"refusing to write {path}: {write_path} is not a regular file")
        return write_path, stat.S_IMODE(metadata.st_mode)
    return write_path, default_mode()


def write_config(write_path, config, mode):
    """Atomically replace ``write_path``, preserving its access mode."""
    payload = json.dumps(config, ensure_ascii=False, indent=2) + "\n"
    directory = os.path.dirname(write_path) or "."
    fd, tmp_path = tempfile.mkstemp(prefix="hooks.json.", suffix=".tmp", dir=directory)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(tmp_path, mode)
        os.replace(tmp_path, write_path)
    except BaseException:
        if os.path.lexists(tmp_path):
            os.unlink(tmp_path)
        raise


if not os.path.exists(config_path):
    nothing_to_do()

write_path, mode = resolve_config(config_path)
with open(write_path, encoding="utf-8") as handle:
    raw = handle.read().strip()
if not raw:
    nothing_to_do()
try:
    config = json.loads(raw)
except json.JSONDecodeError as error:
    die(f"existing {config_path} is not valid JSON: {error}")
if not isinstance(config, dict):
    die(f"unexpected root type in {config_path}")

hooks = config.get("hooks")
if not isinstance(hooks, dict):
    nothing_to_do()

removed = 0
for event in MANAGED_EVENTS:
    groups = hooks.get(event)
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
        removed += len(inner) - len(remaining)
        if remaining:
            pruned = dict(group)
            pruned["hooks"] = remaining
            kept.append(pruned)
        elif not any(is_tokenless_hook(entry) for entry in inner):
            kept.append(group)
    hooks[event] = kept

if removed == 0:
    # Nothing this adapter owns. Do not rewrite the file: a user hook that
    # happens to mention the marker is not ours to reformat.
    nothing_to_do()

write_config(write_path, config, mode)
print(f"[tokenless] removed {removed} tokenless hook entries from {config_path}.")
PYEOF
done

echo "[${COMPONENT}] ${AGENT} hooks uninstalled."
