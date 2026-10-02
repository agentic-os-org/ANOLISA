#!/usr/bin/env python3
"""
validate_mcp.py - Merge and check MCP server config for cosh.

Usage:
  python3 validate_mcp.py '<json>' --merge PATH   # Merge into config file
  python3 validate_mcp.py --check PATH             # Check existing config
"""

import json
import sys
import os

FORBIDDEN_FIELDS = {"type", "transport", "disabled", "alwaysAllow", "scope"}


def merge(json_str, config_path):
    """Parse input JSON, merge mcpServers into existing config, write back."""
    try:
        new = json.loads(json_str)
    except json.JSONDecodeError as e:
        print(f"ERROR: Invalid JSON: {e}", file=sys.stderr)
        sys.exit(1)

    servers = new.get("mcpServers", {})
    if not servers:
        print("ERROR: No mcpServers found in input", file=sys.stderr)
        sys.exit(1)

    # Read existing config
    existing = {}
    if os.path.exists(config_path):
        try:
            with open(config_path, "r", encoding="utf-8") as f:
                existing = json.load(f)
        except (json.JSONDecodeError, IOError) as e:
            print(f"WARNING: Could not parse {config_path}: {e}", file=sys.stderr)

    if not isinstance(existing, dict):
        existing = {}

    # Merge
    if "mcpServers" not in existing:
        existing["mcpServers"] = {}
    existing["mcpServers"].update(servers)

    if "mcp" in new and isinstance(new["mcp"], dict):
        if "mcp" not in existing:
            existing["mcp"] = {}
        existing["mcp"].update(new["mcp"])

    # Write
    os.makedirs(os.path.dirname(config_path) or ".", exist_ok=True)
    with open(config_path, "w", encoding="utf-8") as f:
        json.dump(existing, f, indent=2, ensure_ascii=False)
        f.write("\n")

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
    except json.JSONDecodeError as e:
        print(f"ERROR: Invalid JSON: {e}", file=sys.stderr)
        sys.exit(1)

    servers = data.get("mcpServers", {})
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
