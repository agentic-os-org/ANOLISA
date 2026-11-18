#!/usr/bin/env python3
"""
validate_mcp.py - Merge and check MCP server config for cosh.

Usage:
  python3 validate_mcp.py '<json>' --merge PATH   # Merge into config file
  python3 validate_mcp.py --check PATH             # Check existing config
"""

import json
import os
import shutil
import stat
import sys

FORBIDDEN_FIELDS = {"type", "transport", "disabled", "alwaysAllow", "scope"}


def merge(json_str, config_path):
    """Parse input JSON, merge mcpServers into existing config, write back.

    An existing config this script cannot read is not treated as an empty one:
    the merge rewrites the whole file, so falling back would delete whatever
    the file held that is not `mcpServers`.
    """
    try:
        new = json.loads(json_str)
    except json.JSONDecodeError as e:
        print(f"ERROR: Invalid JSON: {e}", file=sys.stderr)
        sys.exit(1)

    servers = new.get("mcpServers", {})
    if not isinstance(servers, dict):
        print("ERROR: mcpServers must be a JSON object", file=sys.stderr)
        sys.exit(1)
    if not servers:
        print("ERROR: No mcpServers found in input", file=sys.stderr)
        sys.exit(1)

    # Read existing config
    existing = {}
    if os.path.exists(config_path):
        try:
            with open(config_path, "r", encoding="utf-8") as f:
                existing = json.load(f)
        except UnicodeDecodeError as e:
            # This merge rewrites the file, so an undecodable one is fatal:
            # the settings it holds may be ones this script cannot represent,
            # and rewriting would drop them with no trace. Fail closed and
            # leave the file byte-identical.
            print(f"ERROR: Cannot decode {config_path} as UTF-8: {e}", file=sys.stderr)
            sys.exit(1)
        except json.JSONDecodeError as e:
            # Recoverable -- the caller can repair the JSON -- but the
            # rewrite still replaces it, so keep the bytes and the mode it is
            # about to take the place of. `copy2` is inside its own handler
            # because an exception raised by a sibling clause is not caught by
            # this try's other handlers.
            print(f"WARNING: Could not parse {config_path}: {e}", file=sys.stderr)
            backup = f"{config_path}.bak"
            try:
                shutil.copy2(config_path, backup)
            except OSError as backup_error:
                print(f"ERROR: Cannot back up {config_path}: {backup_error}", file=sys.stderr)
                sys.exit(1)
            print(f"WARNING: Original kept at {backup}", file=sys.stderr)
        except OSError as e:
            print(f"ERROR: Cannot read {config_path}: {e}", file=sys.stderr)
            sys.exit(1)

    if not isinstance(existing, dict):
        existing = {}

    # Merge. A non-object mcpServers in the existing config follows the same
    # lenient policy as an unparseable config file: warn and start fresh.
    if not isinstance(existing.get("mcpServers"), dict):
        if "mcpServers" in existing:
            print(f"WARNING: mcpServers in {config_path} is not an object; resetting it",
                  file=sys.stderr)
        existing["mcpServers"] = {}
    existing["mcpServers"].update(servers)

    if "mcp" in new and isinstance(new["mcp"], dict):
        if "mcp" not in existing:
            existing["mcp"] = {}
        existing["mcp"].update(new["mcp"])

    # Write. `open(config_path, "w")` truncates before it writes, so a failure
    # part-way would leave a half-written settings file in place of the
    # original; stage beside it and rename instead.
    # The rename publishes a new inode, so it also publishes that inode's
    # mode: a fresh staging file is `0666 & ~umask`, which would collapse a
    # 0600 config to 0644 on every merge. This file holds headers and `env`
    # values, so carry the mode the user chose instead, and start a new file
    # private rather than umask-default. `realpath` first, because renaming
    # onto a symlink replaces the link and leaves its target behind.
    target = os.path.realpath(config_path)
    os.makedirs(os.path.dirname(target) or ".", exist_ok=True)
    try:
        keep_mode = stat.S_IMODE(os.stat(target).st_mode)
    except OSError:
        keep_mode = 0o600
    staging = f"{target}.tmp"
    try:
        with open(staging, "w", encoding="utf-8") as f:
            json.dump(existing, f, indent=2, ensure_ascii=False)
            f.write("\n")
        os.chmod(staging, keep_mode)
        os.replace(staging, target)
    except OSError as e:
        print(f"ERROR: Cannot write {config_path}: {e}", file=sys.stderr)
        sys.exit(1)
    finally:
        if os.path.exists(staging):
            try:
                os.remove(staging)
            except OSError:
                pass

    print(json.dumps(existing, indent=2, ensure_ascii=False))
    print(f"\nWritten to: {config_path}", file=sys.stderr)


def check(config_path):
    """Validate an existing config file."""
    if not os.path.exists(config_path):
        print(f"ERROR: File not found: {config_path}", file=sys.stderr)
        sys.exit(1)

    try:
        with open(config_path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except (UnicodeDecodeError, json.JSONDecodeError) as e:
        print(f"ERROR: Invalid JSON: {e}", file=sys.stderr)
        sys.exit(1)

    servers = data.get("mcpServers", {})
    if not isinstance(servers, dict):
        print(f"ERROR: mcpServers in {config_path} is not a JSON object", file=sys.stderr)
        sys.exit(1)
    if not servers:
        print(f"No mcpServers in {config_path}")
        return

    ok = True
    for name, cfg in servers.items():
        if not isinstance(cfg, dict):
            print(f"ERROR: [{name}] not a JSON object", file=sys.stderr)
            ok = False
            continue

        # Must have exactly one transport indicator
        has = [f for f in ("command", "httpUrl", "url") if f in cfg]
        if not has:
            print(f"ERROR: [{name}] missing transport field (command/httpUrl/url)", file=sys.stderr)
            ok = False
        elif len(has) > 1:
            print(f"ERROR: [{name}] has multiple transport fields ({', '.join(has)}); "
                  f"use exactly one of command/httpUrl/url", file=sys.stderr)
            ok = False

        # No forbidden fields
        bad = FORBIDDEN_FIELDS & cfg.keys()
        if bad:
            print(f"WARNING: [{name}] has unsupported fields: {bad}", file=sys.stderr)

    if ok:
        print(f"OK: {len(servers)} server(s) in {config_path}")
    sys.exit(0 if ok else 1)


def main():
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        sys.exit(1)

    if args[0] == "--check":
        check(os.path.expanduser(args[1]) if len(args) > 1 else "")
    elif len(args) >= 3 and args[1] == "--merge":
        merge(args[0], os.path.expanduser(args[2]))
    else:
        print(__doc__)
        sys.exit(1)


if __name__ == "__main__":
    main()
