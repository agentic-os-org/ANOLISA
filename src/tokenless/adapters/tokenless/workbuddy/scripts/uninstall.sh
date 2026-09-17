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

# Preserve the existing file mode AND ownership on rewrite: settings.json
# sits beside settings.json.env, where CodeBuddy officially allows
# CODEBUDDY_API_KEY and auth tokens, so a default-mode temp file (0644
# under umask 022) must not widen a 0600 config on replace. mkstemp
# creates the inode with the installer's UID/GID; the chown restores the
# existing owner/group so a root- or cross-account run cannot reassign
# the file, and the staged inode is verified afterwards so a restore that
# did not take refuses the replace instead of silently adopting it.
existing = os.stat(config_path)
fd, tmp_path = tempfile.mkstemp(
    dir=codebuddy_home, prefix=".settings.json.", suffix=".tmp"
)
try:
    # See install.sh: fchmod/fchown/fstat act on the inode this run created,
    # so a symlink swapped in by the owner of codebuddy_home cannot redirect
    # a privileged chmod/chown onto an arbitrary file.
    with os.fdopen(fd, "w", encoding="utf-8", closefd=False) as handle:
        json.dump(config, handle, ensure_ascii=False, indent=2)
        handle.write("\n")
    os.fchmod(fd, stat.S_IMODE(existing.st_mode))
    try:
        os.fchown(fd, existing.st_uid, existing.st_gid)
    except OSError:
        # The verification below decides whether the replace may proceed;
        # swallowing the error here only keeps the diagnostic readable.
        pass
    staged = os.fstat(fd)
    wanted = (existing.st_uid, existing.st_gid)
    if (staged.st_uid, staged.st_gid) != wanted:
        # See install.sh: an un-preserveable owner must abort the rewrite,
        # not hand the config to the installer account. The BaseException
        # handler below removes the staged temp file.
        print(
            f"cannot preserve the ownership of {config_path}: staged "
            f"{staged.st_uid}:{staged.st_gid}, existing "
            f"{wanted[0]}:{wanted[1]}. Re-run as the file's owner, or with "
            "privilege to chown.",
            file=sys.stderr,
        )
        sys.exit(1)
    # os.replace re-resolves tmp_path, and codebuddy_home belongs to the
    # account being configured, so its owner can swap the staged name for a
    # symlink or for a file of their own between any two calls. rename(2)
    # never follows a symlink source, which is what keeps a late swap from
    # moving an unrelated file, but quietly replacing settings.json with an
    # impostor would still be corruption: match the directory entry against
    # the inode this run staged and refuse the rewrite on a mismatch.
    entry = os.lstat(tmp_path)
    swapped = (
        not stat.S_ISREG(entry.st_mode)
        or (entry.st_dev, entry.st_ino) != (staged.st_dev, staged.st_ino)
    )
    if swapped:
        print(
            f"the staged temp file for {config_path} is no longer the inode "
            f"this run created: {tmp_path} was replaced underneath the "
            "rewrite. Refusing to touch the config.",
            file=sys.stderr,
        )
        sys.exit(1)
    os.replace(tmp_path, config_path)
except BaseException:
    try:
        os.unlink(tmp_path)
    except OSError:
        pass
    raise
finally:
    os.close(fd)
print(f"[tokenless] tokenless hooks removed from {config_path}.")
PYEOF

echo "[${COMPONENT}] ${AGENT} hooks uninstalled."
