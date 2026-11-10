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

import argparse
import json
import re
from collections import defaultdict
from datetime import datetime
from pathlib import Path


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
        # Binary readline keeps decoding confined to the header itself.
        with open(session_file, "rb") as f:
            first_line = f.readline()
            if first_line.strip():
                data = json.loads(first_line.decode("utf-8"))
                cwd = data.get("cwd", "")
                if isinstance(cwd, str) and cwd:
                    return cwd
    except Exception:
        pass
    return None


def get_skill_paths(workspace: str) -> dict | None:
    """Return discovered skill paths, or ``None`` when discovery is unavailable.

    An empty dict means the skills directory was successfully inspected and
    contained no directories; missing or unreadable paths remain unknown.
    """
    try:
        skill_paths = {}
        ws_path = Path(workspace)
        skills_dir = ws_path / "skills"

        for skill_dir in skills_dir.iterdir():
            if skill_dir.is_dir():
                skill_paths[skill_dir.name] = str(skill_dir)

        return skill_paths
    except OSError:
        # Session headers can point at expired or inaccessible workspaces.
        # Preserve the unknown state so bare-directory fallbacks still work.
        return None


#: Characters that end a shell word in the commands this script parses.
#: The set mirrors the name class of the ``skills/([^/\s&|;]+)/`` fallback
#: below (a skill directory name may contain anything except ``/`` and
#: these) plus the quoting characters a shell wraps a path in, which are
#: syntax, never part of a directory name here. The grouping and
#: redirection operators ``( ) < >`` and the backquote are syntax as well:
#: a path can sit directly against them (``(cd /ws/skills/web)``,
#: ``ls /ws/skills/web>listing.txt``), so they are word boundaries on
#: either side exactly like whitespace.
_SHELL_WORD_DELIMITERS = " \t\n\r&|;()<>'\"`"

#: The two fallback patterns differ only in how tightly they anchor ``skills/``
#: at the start of the match; ``find_skill_in_command`` has always accepted a
#: bare ``skills/``, ``extract_skill_from_command_direct`` has always required
#: the leading slash, and neither should drift while this is edited.
_NAVIGATION = (".", "..")
_GLOB_CHARS = "*?[]{}"

_FALLBACK_SKILL = re.compile(r'skills/([^/\s&|;()<>"\x27`]+)')
_DIRECT_SKILL = re.compile(r'/skills/([^/\s&|;()<>"\x27`]+)')


def _is_named_skill(name: str, known: dict | None) -> bool:
    """Whether a bare ``skills/<name>`` names a skill directory.

    A component followed by ``/`` needs no check: naming a *file* with a
    trailing slash is not something a shell command does, and the base
    extractor required exactly that.  A component at the end of a shell word is
    ambiguous -- ``ls /ws/skills/web`` names a directory, ``cat
    /ws/skills/LICENSE`` names a file -- so it is accepted only when the answer
    is known: ``known`` holds every directory discovery found under ``skills/``,
    and a name missing from it is a file -- an empty dict means discovery
    answered and found none, which is different from ``None``, where the
    workspace was unusable and nothing was learned.  Only the ``None`` case
    falls back to the weaker test this function can still make on its own.
    """
    if name in _NAVIGATION:
        return False
    if any(char in name for char in _GLOB_CHARS):
        return False
    if known is not None:
        return name in known
    return "." not in name


def _first_skill_dir_name(pattern: re.Pattern, command: str,
                          skill_paths: dict | None = None) -> str | None:
    """First ``skills/``-relative component in ``command`` that names a skill.

    Directory references win over bare ones, in command order.  ``cat
    /ws/skills/LICENSE && python /ws/skills/web/run.py`` therefore attributes to
    ``web`` whatever discovery found, because ``skills/web/`` carries the
    trailing slash that made it unambiguous -- the same reading the extractor
    had before bare references were supported.  A rejected component never ends
    the scan, so a real skill later in the command still counts.
    """
    bare = None
    for match in pattern.finditer(command):
        name = match.group(1)
        if name in _NAVIGATION or any(char in name for char in _GLOB_CHARS):
            continue
        if command[match.end():match.end() + 1] == "/":
            return name
        if bare is None and _is_named_skill(name, skill_paths):
            bare = name
    return bare


def _references_path(command: str, skill_path: str) -> bool:
    """Whether ``command`` references ``skill_path`` as a whole path.

    A bare substring test misattributes a call to whichever skill the dict
    iterates first whenever one skill's path is a prefix of another's
    (``/ws/skills/web`` inside ``/ws/skills/web-search/run.py``), so the
    reference must start and end at a shell-word boundary: ``web`` no
    longer matches ``web-search/...``, while ``ls /ws/skills/web`` (the
    command ends right after the directory) and
    ``python /ws/skills/web/run.py`` still count. A reference also counts
    when it sits directly against a grouping or redirection operator —
    ``(cd /ws/skills/web)`` or ``ls /ws/skills/web>listing.txt`` — since
    those characters are shell syntax, never name content.
    """
    start = 0
    while True:
        idx = command.find(skill_path, start)
        if idx < 0:
            return False
        end = idx + len(skill_path)
        starts_at_boundary = (
            idx == 0
            or command[idx - 1] == "/"
            or command[idx - 1] in _SHELL_WORD_DELIMITERS
        )
        ends_at_boundary = (
            end == len(command)
            or command[end] == "/"
            or command[end] in _SHELL_WORD_DELIMITERS
        )
        if starts_at_boundary and ends_at_boundary:
            return True
        start = idx + 1


def find_skill_in_command(command: str,
                          skill_paths: dict | None = None) -> str | None:
    """Identify which skill (if any) is referenced by a shell command."""
    if not command:
        return None

    # Longest (most specific) path first. With the boundary check above a
    # command that references exactly one skill matches it no matter the
    # dict order; when a command genuinely references several skills, the
    # most specific path wins so the attribution stays deterministic
    # across readdir orders.
    for skill_name, skill_path in sorted(
        (skill_paths or {}).items(), key=lambda item: (-len(item[1]), item[0])
    ):
        if _references_path(command, skill_path):
            return skill_name

    return _first_skill_dir_name(_FALLBACK_SKILL, command, skill_paths)


def extract_skill_from_command_direct(command: str,
                                      skill_paths: dict | None = None) -> str | None:
    """Fallback: extract skill name purely from /skills/<name>/ in the command."""
    if not command:
        return None

    return _first_skill_dir_name(_DIRECT_SKILL, command, skill_paths)


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
    # ``None`` means "discovery could not answer"; an empty dict means it
    # answered and there are no skill directories.  The difference decides
    # whether a bare ``skills/<name>`` may be taken on shape alone.
    skill_paths = None
    if workspace:
        try:
            skill_paths = get_skill_paths(workspace)
        except (OSError, RuntimeError, ValueError):
            # A stale or malformed workspace must not discard the rest of a session.
            skill_paths = None

    try:
        # Decode each physical line separately so corrupt bytes cannot abort the file.
        with open(session_file, "rb") as f:
            for line in f:
                if not line.strip():
                    continue
                try:
                    data = json.loads(line.decode("utf-8"))
                except (UnicodeDecodeError, json.JSONDecodeError):
                    continue

                # A non-object line (array/scalar) has no event fields; skip it
                # instead of aborting the whole session at the first such line.
                if not isinstance(data, dict):
                    continue

                ts = data.get("timestamp", "")
                if ts:
                    session_date = parse_timestamp(ts)

                message = data.get("message", {})
                if not isinstance(message, dict):
                    continue
                content = message.get("content", [])

                if isinstance(content, list):
                    for item in content:
                        if not isinstance(item, dict):
                            continue
                        if item.get("type") == "toolCall":
                            args = item.get("arguments", {})
                            if not isinstance(args, dict):
                                continue
                            command = args.get("command", "")

                            # Only strings can be matched against skill paths;
                            # a list/number/object command must not reach
                            # re.search, which would abort the whole file and
                            # drop every later valid skill call.
                            if not isinstance(command, str):
                                continue

                            skill_name = find_skill_in_command(command, skill_paths)
                            if not skill_name:
                                skill_name = extract_skill_from_command_direct(
                                    command, skill_paths
                                )

                            if skill_name:
                                by_skill[skill_name] += 1
                                by_date[session_date][skill_name] += 1

    except OSError as e:
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

    total_calls = sum(
        sum(agent_stats.values()) for agent_stats in results["by_agent"].values()
    )

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

