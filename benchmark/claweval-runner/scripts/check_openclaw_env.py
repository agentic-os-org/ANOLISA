#!/usr/bin/env python3

# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Check if openclaw environment is clean (not polluted by ce-runner).

Usage:
    python scripts/check_openclaw_env.py          # Check only
    python scripts/check_openclaw_env.py --fix    # Check and fix

Exit codes:
    0 - Environment is clean (or cleanup succeeded)
    1 - Environment has ce-runner artifacts (check only)
    2 - Cleanup failed
"""

import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

OPENCLAW_CONFIG = Path.home() / ".openclaw" / "openclaw.json"

# Regex to match leftover mock service worker processes started by ce-runner.
# Examples:
#   python mock_services/contacts/server.py
#   /usr/bin/python3.11 mock_services/web_real/server.py
_MOCK_PROC_RE = re.compile(r"mock_services/[\w_]+/server\.py")


def check_config() -> list[str]:
    """Check openclaw.json for ce-runner artifacts."""
    issues = []

    if not OPENCLAW_CONFIG.exists():
        return [f"Config not found: {OPENCLAW_CONFIG}"]

    with open(OPENCLAW_CONFIG) as f:
        config = json.load(f)

    # Check agents
    agents = config.get("agents", {}).get("list", [])
    claweval_agents = [a["id"] for a in agents if a.get("id", "").startswith("claweval-")]
    if claweval_agents:
        issues.append(f"claweval agents in config: {claweval_agents}")

    # Check mcp.servers
    servers = config.get("mcp", {}).get("servers", {})
    for key in servers:
        if key.startswith(("claw-eval-", "ce-mock-", "ce-sb-")):
            issues.append(f"mcp.server '{key}' still present")

    # Check tools config
    tools = config.get("tools", {})
    if tools.get("profile") == "minimal":
        issues.append(f"tools.profile is 'minimal' (should be 'coding')")
    if "alsoAllow" in tools:
        issues.append(f"tools.alsoAllow still set: {tools['alsoAllow']}")

    return issues


def _find_mock_processes() -> list[tuple[int, str]]:
    """Return [(pid, cmdline)] of leftover mock_services worker processes.

    Excludes the current process and any ``grep``/``ps`` helpers.
    """
    result = subprocess.run(
        ["ps", "-eo", "pid,args"], capture_output=True, text=True, timeout=10
    )
    my_pid = os.getpid()
    found: list[tuple[int, str]] = []
    for line in result.stdout.splitlines()[1:]:  # skip header
        line = line.strip()
        if not line:
            continue
        parts = line.split(None, 1)
        if len(parts) != 2:
            continue
        try:
            pid = int(parts[0])
        except ValueError:
            continue
        cmd = parts[1]
        if pid == my_pid:
            continue
        if " grep " in cmd or cmd.startswith("grep "):
            continue
        if _MOCK_PROC_RE.search(cmd):
            found.append((pid, cmd))
    return found


def check_processes() -> list[str]:
    """Check for leftover mock_services processes occupying ports 9100-9116."""
    return [f"mock_services process pid={pid}: {cmd}"
            for pid, cmd in _find_mock_processes()]


def check_docker_containers() -> list[str]:
    """Check for leftover claw-eval Docker containers occupying sandbox ports."""
    try:
        result = subprocess.run(
            ["docker", "container", "ls", "-a",
             "--filter", "label=app=claw-eval",
             "--format", "{{.ID}} {{.Names}} {{.Status}}"],
            capture_output=True, text=True, timeout=10,
        )
    except FileNotFoundError:
        return []
    except subprocess.TimeoutExpired:
        return ["docker container ls timed out"]

    if result.returncode != 0:
        return []

    containers = []
    for line in result.stdout.strip().splitlines():
        if line.strip():
            containers.append(line.strip())
    return [f"stale container: {c}" for c in containers]


def check_filesystem() -> list[str]:
    """Check filesystem for ce-runner artifacts."""
    issues = []
    openclaw_dir = Path.home() / ".openclaw"

    if not openclaw_dir.exists():
        return []

    # Check workspace-claweval-* directories
    for d in openclaw_dir.glob("workspace-claweval-*"):
        issues.append(f"workspace dir exists: {d}")

    # Check agents/claweval-* directories
    agents_dir = openclaw_dir / "agents"
    if agents_dir.exists():
        for d in agents_dir.glob("claweval-*"):
            issues.append(f"agent dir exists: {d}")

    # Check mcporter/claw-eval-*.json files
    mcporter_dir = openclaw_dir / "mcporter"
    if mcporter_dir.exists():
        for f in mcporter_dir.glob("claw-eval-*.json"):
            issues.append(f"mcporter config exists: {f}")

    return issues


def _fix_agents(config: dict) -> list[str]:
    """Remove claweval- agents directly from config dict.

    Mutates *config* in place (does not call openclaw CLI) to avoid the
    size-drop gate that rejects writes shrinking the file by >50%.
    """
    removed = []
    agents = config.get("agents", {}).get("list", [])
    claweval_agents = [a for a in agents if a.get("id", "").startswith("claweval-")]

    if claweval_agents:
        config.setdefault("agents", {})["list"] = [
            a for a in agents if not a.get("id", "").startswith("claweval-")
        ]
        removed = [a["id"] for a in claweval_agents]

    return removed


def _fix_mcp_servers(config: dict) -> list[str]:
    """Remove ce-runner MCP servers from config."""
    removed = []
    servers = config.get("mcp", {}).get("servers", {})
    for key in list(servers):
        if key.startswith(("claw-eval-", "ce-mock-", "ce-sb-")):
            del config["mcp"]["servers"][key]
            removed.append(key)
    return removed


def _fix_tools(config: dict) -> list[str]:
    """Fix tools config: restore profile to 'coding', remove alsoAllow."""
    fixed = []
    tools = config.get("tools", {})
    if tools.get("profile") == "minimal":
        tools["profile"] = "coding"
        fixed.append("tools.profile → 'coding'")
    if "alsoAllow" in tools:
        del tools["alsoAllow"]
        fixed.append("removed tools.alsoAllow")
    return fixed


def _fix_processes() -> list[str]:
    """Kill leftover mock_services worker processes (SIGTERM, then SIGKILL)."""
    killed: list[str] = []
    procs = _find_mock_processes()
    if not procs:
        return killed

    # First pass: graceful SIGTERM
    for pid, cmd in procs:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            killed.append(f"already exited pid={pid}")
        except Exception as e:
            print(f"  ⚠️  Failed to SIGTERM {pid}: {e}")

    # Give them a moment to exit
    time.sleep(0.5)

    # Second pass: SIGKILL anything still alive
    for pid, cmd in _find_mock_processes():
        try:
            os.kill(pid, signal.SIGKILL)
            killed.append(f"killed pid={pid}: {cmd[:60]}")
        except ProcessLookupError:
            killed.append(f"already exited pid={pid}")
        except Exception as e:
            print(f"  ⚠️  Failed to SIGKILL {pid}: {e}")

    # Record gracefully-terminated ones too
    surviving_pids = {p for p, _ in _find_mock_processes()}
    for pid, cmd in procs:
        if pid not in surviving_pids and not any(str(pid) in k for k in killed):
            killed.append(f"terminated pid={pid}: {cmd[:60]}")

    return killed


def _fix_docker_containers() -> list[str]:
    """Remove leftover claw-eval Docker containers."""
    removed: list[str] = []
    try:
        result = subprocess.run(
            ["docker", "container", "ls", "-a", "-q",
             "--filter", "label=app=claw-eval"],
            capture_output=True, text=True, timeout=10,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return removed

    if result.returncode != 0 or not result.stdout.strip():
        return removed

    container_ids = result.stdout.strip().split()
    for cid in container_ids:
        try:
            rm_result = subprocess.run(
                ["docker", "rm", "-f", cid],
                capture_output=True, text=True, timeout=15,
            )
            if rm_result.returncode == 0:
                removed.append(f"removed container {cid}")
            else:
                print(f"  ⚠️  Failed to remove container {cid}: {rm_result.stderr.strip()}")
        except Exception as e:
            print(f"  ⚠️  Failed to remove container {cid}: {e}")

    return removed


def _fix_filesystem() -> list[str]:
    """Remove leftover ce-runner directories and files."""
    removed = []
    openclaw_dir = Path.home() / ".openclaw"

    if not openclaw_dir.exists():
        return []

    # Remove workspace-claweval-* directories
    for d in openclaw_dir.glob("workspace-claweval-*"):
        try:
            shutil.rmtree(d)
            removed.append(f"removed {d}")
        except Exception as e:
            print(f"  ⚠️  Failed to remove {d}: {e}")

    # Remove agents/claweval-* directories
    agents_dir = openclaw_dir / "agents"
    if agents_dir.exists():
        for d in agents_dir.glob("claweval-*"):
            try:
                shutil.rmtree(d)
                removed.append(f"removed {d}")
            except Exception as e:
                print(f"  ⚠️  Failed to remove {d}: {e}")

    # Remove mcporter/claw-eval-*.json files
    mcporter_dir = openclaw_dir / "mcporter"
    if mcporter_dir.exists():
        for f in mcporter_dir.glob("claw-eval-*.json"):
            try:
                f.unlink()
                removed.append(f"removed {f}")
            except Exception as e:
                print(f"  ⚠️  Failed to remove {f}: {e}")

    return removed


def fix_env() -> bool:
    """Perform cleanup of all ce-runner artifacts. Returns True on success."""
    actions: list[str] = []

    if not OPENCLAW_CONFIG.exists():
        print(f"  Config not found: {OPENCLAW_CONFIG}, skipping config cleanup")
    else:
        with open(OPENCLAW_CONFIG) as f:
            config = json.load(f)

        # Remove claweval- agents from config (in-place mutation)
        removed_agents = _fix_agents(config)
        for aid in removed_agents:
            actions.append(f"deleted agent: {aid}")

        # Remove claw-eval- MCP servers from config
        removed_servers = _fix_mcp_servers(config)
        for key in removed_servers:
            actions.append(f"removed mcp.server: {key}")

        # Fix tools config
        fixed_tools = _fix_tools(config)
        for fix in fixed_tools:
            actions.append(f"fixed {fix}")

        # Write back if config changed
        if removed_agents or removed_servers or fixed_tools:
            with open(OPENCLAW_CONFIG, "w") as f:
                json.dump(config, f, indent=2)
            actions.append(f"wrote updated config: {OPENCLAW_CONFIG}")

    # Kill leftover mock_services processes (must precede fs cleanup,
    # otherwise stale processes keep recreating sockets)
    proc_killed = _fix_processes()
    actions.extend(proc_killed)

    # Remove leftover Docker containers (must precede fs cleanup,
    # containers may hold ports needed by the next run)
    docker_removed = _fix_docker_containers()
    actions.extend(docker_removed)

    # Remove leftover filesystem artifacts
    fs_removed = _fix_filesystem()
    actions.extend(fs_removed)

    if not actions:
        print("  Nothing to clean up")
        return True

    print(f"  Cleanup actions ({len(actions)}):")
    for action in actions:
        print(f"    ✓ {action}")

    # Final check to verify environment is clean
    remaining_issues = (check_config() + check_filesystem()
                        + check_processes() + check_docker_containers())
    if remaining_issues:
        print(f"\n  ⚠️  {len(remaining_issues)} issue(s) remain after cleanup:")
        for issue in remaining_issues:
            print(f"    ✗ {issue}")
        return False

    return True


def main():
    import argparse

    parser = argparse.ArgumentParser(
        description="Check (and optionally fix) openclaw environment for ce-runner artifacts",
    )
    parser.add_argument("--fix", action="store_true",
                        help="Perform cleanup of ce-runner artifacts")
    args = parser.parse_args()

    if args.fix:
        print("Checking and fixing openclaw environment...")
    else:
        print("Checking openclaw environment...")
    print(f"Config: {OPENCLAW_CONFIG}")
    print()

    config_issues = check_config()
    fs_issues = check_filesystem()
    proc_issues = check_processes()
    docker_issues = check_docker_containers()
    all_issues = config_issues + fs_issues + proc_issues + docker_issues

    if config_issues:
        print("Config issues:")
        for issue in config_issues:
            print(f"  ✗ {issue}")
        print()

    if fs_issues:
        print("Filesystem issues:")
        for issue in fs_issues:
            print(f"  ✗ {issue}")
        print()

    if proc_issues:
        print("Process issues:")
        for issue in proc_issues:
            print(f"  ✗ {issue}")
        print()

    if docker_issues:
        print("Docker container issues:")
        for issue in docker_issues:
            print(f"  ✗ {issue}")
        print()

    if not all_issues:
        print("✅ Environment is clean")
        sys.exit(0)

    if args.fix:
        print(f"\n❌ Found {len(all_issues)} issue(s). Running cleanup...")
        print()
        success = fix_env()
        if success:
            print("\n✅ Environment is now clean")
            sys.exit(0)
        else:
            print("\n❌ Cleanup incomplete")
            sys.exit(2)
    else:
        print(f"❌ Found {len(all_issues)} issue(s)")
        print("\nCleanup with:")
        print("  python scripts/check_openclaw_env.py --fix")
        print("  or manually:")
        print("    openclaw agents delete claweval-<task_id> --force")
        print("    rm -rf ~/.openclaw/workspace-claweval-*")
        print("    rm -rf ~/.openclaw/agents/claweval-*")
        print("    rm -f ~/.openclaw/mcporter/claw-eval-*.json")
        print("    pkill -f mock_services")
        print("    docker rm -f $(docker container ls -aq --filter label=app=claw-eval)")
        sys.exit(1)


if __name__ == "__main__":
    main()
