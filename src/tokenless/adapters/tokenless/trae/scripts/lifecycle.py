#!/usr/bin/env python3
"""Edit a Trae edition's global hooks.json on behalf of the Tokenless adapter.

Trae has no plugin system for hooks, so the adapter merges its hook groups
straight into ``~/.trae-cn/hooks.json`` (CN edition) and ``~/.trae/hooks.json``
(international edition) — files it shares with the user and with every other
hook source Trae loads. This module is the single place that decides what the
adapter owns and how the shared file is rewritten, so ``install.sh``,
``uninstall.sh`` and ``detect.sh`` cannot drift apart:

* **Ownership** is the exact command shape the template generates, never a
  marker substring. A user hook that merely mentions ``TOKENLESS_AGENT_ID=trae``
  is user-owned and survives the merge, the removal and the readiness report.
* **The managed path is preserved.** A dotfiles-managed ``hooks.json`` symlink
  is followed, so the link stays a link pointing at the same target and the real
  configuration is what changes. A link whose target is missing is rejected
  before anything is written, leaving both the stale link and the absent target
  exactly as they were.
* **The access mode is preserved.** The replacement is written to a temporary in
  the resolved file's own directory and chmod'ed to the mode the file already
  carried before the atomic rename. A first-time create still gets the
  umask-derived mode.

Subcommands: ``merge`` (install one edition), ``strip`` (uninstall one edition),
``owned-count`` (report how many adapter-owned entries a hooks.json holds).
"""

from __future__ import annotations

# Direct Python invocations must be as read-only as the wrappers' python3 -B.
import sys

sys.dont_write_bytecode = True

import argparse
import json
import os
import shlex
import stat
import tempfile

MARKER = "TOKENLESS_AGENT_ID=trae"
PLACEHOLDER = "@TOKENLESS_DISPATCHER@"
# Trailing path segments that identify our dispatcher. Matching the shape of the
# generated command instead of a marker substring keeps an adapter tree that was
# relocated after install still uninstallable, while never claiming a user
# command as ours.
DISPATCHER_SUFFIX = "trae/hooks/run-hook.sh"
# Hook scripts this adapter registers, and the events it registers them on.
# Keep in sync with trae/hooks/hooks.json; the adapter test suite asserts it.
OWNED_SCRIPTS = frozenset(
    {"rewrite_hook.py", "tool_ready_hook.sh", "compress_response_hook.py"}
)
MANAGED_EVENTS = ("PreToolUse", "PostToolUse")


class ConfigError(Exception):
    """A hooks.json the adapter must not touch, or cannot parse."""


def is_tokenless_hook(entry: object) -> bool:
    """Return True only for a hook command this adapter generates.

    The generated command is exactly four shell words:
    ``TOKENLESS_AGENT_ID=trae bash <.../trae/hooks/run-hook.sh> <script>``.
    Anything else — including a user command that merely mentions the marker —
    is user-owned.
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


def owned_entry_count(hooks: object) -> int:
    """Count adapter-owned hook entries across every event in ``hooks``."""
    if not isinstance(hooks, dict):
        return 0
    owned = 0
    for groups in hooks.values():
        if not isinstance(groups, list):
            continue
        for group in groups:
            if not isinstance(group, dict):
                continue
            inner = group.get("hooks")
            if not isinstance(inner, list):
                continue
            owned += sum(1 for entry in inner if is_tokenless_hook(entry))
    return owned


def strip_tokenless(groups: list) -> list:
    """Drop adapter-owned entries from ``groups``, keeping everything else."""
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


def default_mode() -> int:
    """Mode for a brand-new hooks.json: whatever a plain write would produce.

    An existing file keeps the mode it already had (see resolve_config()); only
    a first-time create falls back to the process umask.
    """
    umask = os.umask(0)
    os.umask(umask)
    return 0o666 & ~umask


def resolve_config(path: str) -> tuple[str, int]:
    """Return ``(write_path, mode)`` for the host config at ``path``.

    A dotfiles-managed ``hooks.json`` symlink is followed so the managed path
    stays a symlink pointing at the same target. A symlink whose target does not
    exist is rejected: creating the missing file would silently adopt a stale
    link as our own and hide the fact that the user's real configuration is
    elsewhere. Anything that is not a regular file is rejected too, before a
    single byte is written.
    """
    if not os.path.lexists(path):
        return path, default_mode()
    if os.path.islink(path):
        target = os.path.realpath(path)
        if os.path.islink(target):
            raise ConfigError(
                f"refusing to write {path}: symlink target does not resolve ({target})"
            )
        if not os.path.exists(target):
            raise ConfigError(
                f"refusing to write {path}: symlink target does not exist ({target})"
            )
        write_path = target
    else:
        write_path = path
    metadata = os.stat(write_path)
    if not stat.S_ISREG(metadata.st_mode):
        raise ConfigError(f"refusing to write {path}: {write_path} is not a regular file")
    return write_path, stat.S_IMODE(metadata.st_mode)


def write_config(write_path: str, config: dict, mode: int) -> None:
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
        # carried so an edit neither widens nor narrows access to it.
        os.chmod(tmp_path, mode)
        os.replace(tmp_path, write_path)
    except BaseException:
        if os.path.lexists(tmp_path):
            os.unlink(tmp_path)
        raise


def read_config(config_path: str, write_path: str) -> dict:
    """Parse the config at ``write_path``, or return {} when it is absent."""
    config: object = {}
    if os.path.exists(write_path):
        with open(write_path, encoding="utf-8") as handle:
            raw = handle.read().strip()
        if raw:
            try:
                config = json.loads(raw)
            except json.JSONDecodeError as error:
                raise ConfigError(f"existing {config_path} is not valid JSON: {error}")
    if not isinstance(config, dict):
        raise ConfigError(f"unexpected root type in {config_path}")
    return config


def hooks_section(config: dict, config_path: str) -> dict:
    hooks = config.get("hooks")
    if hooks is None:
        hooks = {}
    if not isinstance(hooks, dict):
        raise ConfigError(f"unexpected hooks type in {config_path}")
    return hooks


def stamp(value, quoted_dispatcher: str):
    """Replace the whole-token dispatcher placeholder with a shell-literal word.

    The host runs the command string through a shell, so the dispatcher path is
    serialized as one literal word: an adapter root holding a space, a quote,
    ``$``, or a backtick must not be re-expanded at hook time.
    """
    if isinstance(value, str):
        if PLACEHOLDER not in value:
            return value
        return value.replace(PLACEHOLDER, quoted_dispatcher)
    if isinstance(value, list):
        return [stamp(item, quoted_dispatcher) for item in value]
    if isinstance(value, dict):
        return {key: stamp(item, quoted_dispatcher) for key, item in value.items()}
    return value


def merge(trae_home: str, template_path: str, dispatcher: str) -> int:
    """Merge the adapter's hook groups into one edition's hooks.json."""
    with open(template_path, encoding="utf-8") as handle:
        template = json.load(handle)
    new_groups = stamp(template.get("hooks", {}), shlex.quote(dispatcher))

    config_path = os.path.join(trae_home, "hooks.json")
    write_path, mode = resolve_config(config_path)
    config = read_config(config_path, write_path)
    hooks = hooks_section(config, config_path)

    for event, additions in new_groups.items():
        existing = hooks.get(event, [])
        if not isinstance(existing, list):
            raise ConfigError(f"unexpected {event} type in {config_path}")
        hooks[event] = strip_tokenless(existing) + additions

    config.setdefault("version", 1)
    config["hooks"] = hooks
    write_config(write_path, config, mode)

    # Verify through the managed path, i.e. exactly what Trae will read.
    with open(config_path, encoding="utf-8") as handle:
        verify = json.load(handle)
    if owned_entry_count(verify.get("hooks")) == 0:
        raise ConfigError(f"tokenless hooks missing after merge: {config_path}")
    return 0


def strip(trae_home: str) -> int:
    """Remove adapter-owned hook entries from one edition's hooks.json."""
    config_path = os.path.join(trae_home, "hooks.json")
    if not os.path.exists(config_path):
        print(f"[tokenless] no hooks.json in {trae_home} — nothing to uninstall.")
        return 0

    write_path, mode = resolve_config(config_path)
    with open(write_path, encoding="utf-8") as handle:
        raw = handle.read().strip()
    if not raw:
        print(f"[tokenless] no tokenless hooks in {config_path} — nothing to uninstall.")
        return 0
    try:
        config = json.loads(raw)
    except json.JSONDecodeError as error:
        raise ConfigError(f"existing {config_path} is not valid JSON: {error}")
    if not isinstance(config, dict):
        raise ConfigError(f"unexpected root type in {config_path}")

    hooks = config.get("hooks")
    if not isinstance(hooks, dict):
        print(f"[tokenless] no tokenless hooks in {config_path} — nothing to uninstall.")
        return 0

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
        print(f"[tokenless] no tokenless hooks in {config_path} — nothing to uninstall.")
        return 0

    write_config(write_path, config, mode)
    print(f"[tokenless] removed {removed} tokenless hook entries from {config_path}.")
    return 0


def owned_count(hooks_file: str) -> int:
    """Print how many adapter-owned entries ``hooks_file`` holds.

    Read-only and deliberately forgiving: detect.sh reports readiness, it does
    not enforce anything, so an unreadable or malformed config counts as zero
    registered hooks rather than raising.
    """
    try:
        with open(hooks_file, encoding="utf-8") as handle:
            config = json.load(handle)
    except (OSError, ValueError):
        return 0
    if not isinstance(config, dict):
        return 0
    return owned_entry_count(config.get("hooks"))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="action", required=True)

    merge_parser = subparsers.add_parser("merge", help="merge the adapter hook groups")
    merge_parser.add_argument("--trae-home", required=True)
    merge_parser.add_argument("--template", required=True)
    merge_parser.add_argument("--dispatcher", required=True)

    strip_parser = subparsers.add_parser("strip", help="remove adapter-owned entries")
    strip_parser.add_argument("--trae-home", required=True)

    count_parser = subparsers.add_parser(
        "owned-count", help="count adapter-owned entries in a hooks.json"
    )
    count_parser.add_argument("--hooks-file", required=True)

    args = parser.parse_args(argv)
    try:
        if args.action == "merge":
            return merge(args.trae_home, args.template, args.dispatcher)
        if args.action == "strip":
            return strip(args.trae_home)
        print(owned_count(args.hooks_file))
        return 0
    except ConfigError as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
