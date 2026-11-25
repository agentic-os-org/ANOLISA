#!/usr/bin/env bash
# install.sh — Merge the Tokenless hook groups into Trae's global hooks.json.
#
# Trae (TraeCode) reads hooks from ~/.trae-cn/hooks.json (CN edition) and
# ~/.trae/hooks.json (international edition), merging every enabled source.
# There is no plugin-root substitution, so the hook commands are stamped with
# the absolute dispatcher path at install time. Entries owned by Tokenless are
# matched by the exact command shape this script generates — never by a marker
# substring — which keeps the merge idempotent, preserves user hooks that
# happen to mention the marker, and lets uninstall.sh remove exactly what this
# script adds.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-trae}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
PLUGIN_DIR="$ADAPTER_DIR/trae"
HOOKS_TEMPLATE="$PLUGIN_DIR/hooks/hooks.json"
DISPATCHER="$PLUGIN_DIR/hooks/run-hook.sh"

TRAE_HOMES=()
for home in "$HOME/.trae-cn" "$HOME/.trae"; do
    [ -d "$home" ] && TRAE_HOMES+=("$home")
done
if [ ${#TRAE_HOMES[@]} -eq 0 ]; then
    echo "[${COMPONENT}] Trae not detected (no ~/.trae-cn or ~/.trae) — skipping ${AGENT} hook installation."
    exit 0
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "[${COMPONENT}] ERROR: python3 is required by the Tokenless hooks" >&2
    exit 1
fi
if [ ! -f "$HOOKS_TEMPLATE" ]; then
    echo "[${COMPONENT}] ERROR: Trae hooks template not found: ${HOOKS_TEMPLATE}" >&2
    exit 1
fi
if [ ! -f "$DISPATCHER" ]; then
    echo "[${COMPONENT}] ERROR: hook dispatcher not found: ${DISPATCHER}" >&2
    exit 1
fi

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    for home in "${TRAE_HOMES[@]}"; do
        echo "DRY-RUN: merge tokenless hook groups into ${home}/hooks.json"
    done
    exit 0
fi

for home in "${TRAE_HOMES[@]}"; do
    TRAE_HOME="$home" TOKENLESS_HOOKS_TEMPLATE="$HOOKS_TEMPLATE" \
    TOKENLESS_DISPATCHER="$DISPATCHER" python3 - <<'PYEOF'
import json
import os
import shlex
import stat
import sys
import tempfile

MARKER = "TOKENLESS_AGENT_ID=trae"
PLACEHOLDER = "@TOKENLESS_DISPATCHER@"
# Trailing path segments that identify our dispatcher. Matching the shape of the
# generated command instead of a marker substring keeps a relocation of the
# adapter tree uninstallable while never claiming a user command as ours.
DISPATCHER_SUFFIX = "trae/hooks/run-hook.sh"
# Hook scripts this adapter registers. Keep in sync with trae/hooks/hooks.json
# and with uninstall.sh.
OWNED_SCRIPTS = frozenset(
    {"rewrite_hook.py", "tool_ready_hook.sh", "compress_response_hook.py"}
)

trae_home = os.environ["TRAE_HOME"]
dispatcher = os.environ["TOKENLESS_DISPATCHER"]
# The host runs the command string through a shell, so the dispatcher path is
# serialized as one literal word: an adapter root holding a space, a quote,
# `$`, or a backtick must not be re-expanded at hook time.
quoted_dispatcher = shlex.quote(dispatcher)


def die(message):
    print(message, file=sys.stderr)
    sys.exit(1)


with open(os.environ["TOKENLESS_HOOKS_TEMPLATE"], encoding="utf-8") as handle:
    template = json.load(handle)


def stamp(value):
    if isinstance(value, str):
        if PLACEHOLDER not in value:
            return value
        return value.replace(PLACEHOLDER, quoted_dispatcher)
    if isinstance(value, list):
        return [stamp(item) for item in value]
    if isinstance(value, dict):
        return {key: stamp(item) for key, item in value.items()}
    return value


new_groups = stamp(template.get("hooks", {}))
config_path = os.path.join(trae_home, "hooks.json")


def is_tokenless_hook(entry):
    """True only for a command this adapter generates.

    The generated command is exactly four shell words:
    ``TOKENLESS_AGENT_ID=trae bash <.../trae/hooks/run-hook.sh> <script>``.
    Anything else — including a user command that merely mentions the marker —
    is user-owned and must survive both the merge and the uninstall.
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


def strip_tokenless(groups):
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
            # A user group without hooks stays untouched; a group that held only
            # Tokenless hooks is dropped.
            kept.append(group)
    return kept


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

    A dotfiles-managed ``hooks.json`` is a symlink. Renaming a fresh file over
    it would replace the managed path with a regular file and leave the real
    configuration stale, so follow the link and write its target instead — the
    managed path stays a symlink pointing at the same place. Anything that is
    not a regular file is rejected before a single byte is written.
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
        # mkstemp creates 0600; restore the mode the host config already
        # carried so an install neither widens nor narrows access to it.
        os.chmod(tmp_path, mode)
        os.replace(tmp_path, write_path)
    except BaseException:
        if os.path.lexists(tmp_path):
            os.unlink(tmp_path)
        raise


write_path, mode = resolve_config(config_path)

config = {}
if os.path.exists(write_path):
    with open(write_path, encoding="utf-8") as handle:
        raw = handle.read().strip()
    if raw:
        try:
            config = json.loads(raw)
        except json.JSONDecodeError as error:
            die(f"existing {config_path} is not valid JSON: {error}")
if not isinstance(config, dict):
    die(f"unexpected root type in {config_path}")

hooks = config.get("hooks")
if hooks is None:
    hooks = {}
if not isinstance(hooks, dict):
    die(f"unexpected hooks type in {config_path}")

for event, additions in new_groups.items():
    existing = hooks.get(event, [])
    if not isinstance(existing, list):
        die(f"unexpected {event} type in {config_path}")
    hooks[event] = strip_tokenless(existing) + additions

config.setdefault("version", 1)
config["hooks"] = hooks

write_config(write_path, config, mode)

with open(config_path, encoding="utf-8") as handle:
    verify = json.load(handle)
if MARKER not in json.dumps(verify.get("hooks", {})):
    die(f"tokenless hooks missing after merge: {config_path}")
PYEOF
    echo "[${COMPONENT}] ${AGENT} hooks installed into ${home}/hooks.json."
done

echo "[${COMPONENT}] Restart Trae to load the hooks."
