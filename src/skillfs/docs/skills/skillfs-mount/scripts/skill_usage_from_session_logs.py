#!/usr/bin/env python3
"""Skill-usage counter for openclaw-format session logs.

Reads agent session JSONL files (one event per line) and tallies how often
each skill is invoked. Output helps decide which skills belong in the
default SkillFS view.

Expected log layout (openclaw 1.x):
    <logs-dir>/agents/<agent-id>/sessions/*.jsonl
    <logs-dir>/openclaw.json   (optional, supplies agent -> workspace mapping)

Pass --logs-dir to point at any directory that follows this layout. The
default is ~/.openclaw, matching openclaw's out-of-the-box location.
"""

from __future__ import annotations

import json
import re
import argparse
from collections import defaultdict
from pathlib import Path
from datetime import datetime


def load_config_workspaces(config_file: Path) -> dict:
    """Read agent_id -> workspace mapping from openclaw.json."""
    workspaces = {}
    if config_file.exists():
        try:
            with open(config_file) as f:
                config = json.load(f)
            agents = config.get("agents", {})
            for agent in agents.get("list", []):
                agent_id = agent.get("id", "")
                workspace = agent.get("workspace", "")
                if agent_id and workspace:
                    workspaces[agent_id] = workspace
        except Exception as e:
            print(f"Warning: Could not read config {config_file}: {e}")
    return workspaces


def get_workspace_from_session(session_file: Path) -> str | None:
    """Read cwd from the first event of a session file."""
    try:
        with open(session_file) as f:
            first_line = f.readline()
            if first_line.strip():
                data = json.loads(first_line)
                cwd = data.get("cwd", "")
                if cwd:
                    return cwd
    except Exception:
        pass
    return None


def get_skill_paths(workspace: str) -> dict:
    """Return {skill_name: absolute_path} discovered under <workspace>/skills."""
    skill_paths = {}
    ws_path = Path(workspace)
    skills_dir = ws_path / "skills"

    if skills_dir.exists():
        for skill_dir in skills_dir.iterdir():
            if skill_dir.is_dir():
                skill_paths[skill_dir.name] = str(skill_dir)

    return skill_paths


def find_skill_in_command(command: str, skill_paths: dict) -> str | None:
    """Identify which skill (if any) is referenced by a shell command."""
    if not command:
        return None

    for skill_name, skill_path in skill_paths.items():
        if skill_path in command:
            return skill_name

    match = re.search(r'skills/([^/\s&|;]+)/', command)
    if match:
        skill_name = match.group(1)
        if skill_name in skill_paths:
            return skill_name
        return skill_name

    return None


def extract_skill_from_command_direct(command: str) -> str | None:
    """Fallback: extract skill name purely from /skills/<name>/ in the command."""
    if not command:
        return None

    match = re.search(r'/skills/([^/\s&|;]+)/', command)
    if match:
        return match.group(1)

    return None


def parse_timestamp(ts) -> str:
    try:
        if isinstance(ts, str):
            dt = datetime.fromisoformat(ts.replace('Z', '+00:00'))
            return dt.strftime('%Y-%m-%d')
        elif isinstance(ts, (int, float)):
            dt = datetime.fromtimestamp(ts / 1000)
            return dt.strftime('%Y-%m-%d')
    except Exception:
        pass
    return "unknown"


def analyze_session_file(session_file: Path) -> tuple[str, dict, dict]:
    by_skill = defaultdict(int)
    by_date = defaultdict(lambda: defaultdict(int))
    session_date = "unknown"

    workspace = get_workspace_from_session(session_file)
    if workspace:
        skill_paths = get_skill_paths(workspace)
    else:
        skill_paths = {}

    try:
        with open(session_file) as f:
            for line in f:
                if not line.strip():
                    continue
                try:
                    data = json.loads(line)
                except json.JSONDecodeError:
                    continue

                ts = data.get("timestamp", "")
                if ts:
                    session_date = parse_timestamp(ts)

                message = data.get("message", {})
                content = message.get("content", [])

                if isinstance(content, list):
                    for item in content:
                        if item.get("type") == "toolCall":
                            args = item.get("arguments", {})
                            command = args.get("command", "")

                            skill_name = find_skill_in_command(command, skill_paths)
                            if not skill_name:
                                skill_name = extract_skill_from_command_direct(command)

                            if skill_name:
                                by_skill[skill_name] += 1
                                by_date[session_date][skill_name] += 1

    except Exception as e:
        print(f"Error reading {session_file}: {e}")

    return workspace or "unknown", dict(by_skill), {k: dict(v) for k, v in by_date.items()}


def analyze_all_sessions(logs_dir: Path) -> dict:
    """Walk every agent's session files under logs_dir/agents/*."""
    agents_dir = logs_dir / "agents"
    config_file = logs_dir / "openclaw.json"

    results = {
        "by_agent": defaultdict(lambda: defaultdict(int)),
        "by_date": defaultdict(lambda: defaultdict(int)),
        "by_session": [],
        "workspaces": load_config_workspaces(config_file),
    }

    if not agents_dir.exists():
        print(f"Agents directory not found: {agents_dir}")
        return results

    for agent_dir in agents_dir.iterdir():
        if not agent_dir.is_dir():
            continue

        agent_id = agent_dir.name
        sessions_dir = agent_dir / "sessions"

        if not sessions_dir.exists():
            continue

        session_files = list(sessions_dir.glob("*.jsonl"))
        print(f"Analyzing agent '{agent_id}': {len(session_files)} session files")

        for session_file in session_files:
            workspace, by_skill, by_date = analyze_session_file(session_file)

            if workspace != "unknown" and agent_id not in results["workspaces"]:
                results["workspaces"][agent_id] = workspace

            total_calls = sum(by_skill.values())

            for skill_name, count in by_skill.items():
                results["by_agent"][agent_id][skill_name] += count

            for date, skills in by_date.items():
                for skill_name, count in skills.items():
                    results["by_date"][date][skill_name] += count

            if total_calls > 0:
                results["by_session"].append({
                    "agent": agent_id,
                    "session": session_file.name,
                    "workspace": workspace,
                    "calls": total_calls,
                    "skills": by_skill,
                    "dates": by_date,
                })

    return results


def print_report(results: dict, mode: str = "summary"):
    print("\n" + "=" * 60)
    print("Skill Usage Report (session logs)")
    print("=" * 60)

    total_calls = 0

    print("\nWorkspace mapping:")
    print("-" * 40)
    for agent_id, workspace in sorted(results["workspaces"].items()):
        print(f"  {agent_id}: {workspace}")

    if mode in ("summary", "all"):
        print("\nBy agent:")
        print("-" * 40)
        for agent_id in sorted(results["by_agent"].keys()):
            agent_stats = results["by_agent"][agent_id]
            if not agent_stats:
                continue
            print(f"\n  Agent: {agent_id}")
            for skill_name in sorted(agent_stats.keys(), key=lambda x: agent_stats[x], reverse=True):
                count = agent_stats[skill_name]
                print(f"    {skill_name}: {count}")
                total_calls += count

    if mode in ("date", "all"):
        print("\nBy date:")
        print("-" * 40)
        for date in sorted(results["by_date"].keys()):
            date_stats = results["by_date"][date]
            if not date_stats:
                continue
            print(f"\n  {date}")
            for skill_name in sorted(date_stats.keys(), key=lambda x: date_stats[x], reverse=True):
                count = date_stats[skill_name]
                print(f"    {skill_name}: {count}")

    if mode in ("session", "all"):
        print("\nBy session:")
        print("-" * 40)
        for session in sorted(results["by_session"], key=lambda x: x["calls"], reverse=True):
            print(f"\n  {session['agent']}/{session['session']}")
            print(f"    Workspace: {session['workspace']}")
            print(f"    Calls: {session['calls']}")
            for skill_name, count in sorted(session["skills"].items(), key=lambda x: x[1], reverse=True):
                print(f"      {skill_name}: {count}")

    print("\n" + "=" * 60)
    print(f"Total skill calls: {total_calls}")
    print("=" * 60)


def main():
    parser = argparse.ArgumentParser(
        description="Count skill invocations across openclaw-format session logs."
    )
    parser.add_argument(
        "--logs-dir",
        type=Path,
        default=Path.home() / ".openclaw",
        help="Root directory containing agents/<id>/sessions/*.jsonl "
             "(default: ~/.openclaw)",
    )
    parser.add_argument("--mode", "-m",
                        choices=["summary", "date", "session", "all"],
                        default="summary",
                        help="Report mode")
    parser.add_argument("--output", "-o",
                        help="Output file (JSON)")
    args = parser.parse_args()

    print(f"Analyzing session logs under: {args.logs_dir}")
    results = analyze_all_sessions(args.logs_dir)

    if args.output:
        output = {
            "workspaces": results["workspaces"],
            "by_agent": {k: dict(v) for k, v in results["by_agent"].items()},
            "by_date": {k: dict(v) for k, v in results["by_date"].items()},
            "by_session": results["by_session"],
        }
        with open(args.output, "w") as f:
            json.dump(output, f, indent=2, ensure_ascii=False)
        print(f"Report saved to: {args.output}")

    print_report(results, args.mode)


if __name__ == "__main__":
    main()
