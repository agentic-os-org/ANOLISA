#!/usr/bin/env python3
"""Skill-usage counter for copilot-shell-format chat logs.

Reads chat JSONL files (one event per line) and tallies how often each
skill is invoked. Output helps decide which skills belong in the default
SkillFS view.

Expected log layout (copilot-shell / cosh):
    <logs-dir>/projects/*/chats/*.jsonl

Pass --logs-dir to point at any directory that follows this layout. The
default is ~/.copilot-shell, matching cosh's out-of-the-box location.
"""

import argparse
import glob
import json
import os
import sys
from collections import Counter
from pathlib import Path


def iter_chat_files(logs_dir: Path):
    pattern = str(logs_dir / "projects" / "*" / "chats" / "*.jsonl")
    return glob.glob(pattern)


def parse_function_calls(file_path):
    """Yield (tool_name, skill_name_or_None, timestamp) from a chat log."""
    with open(file_path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue

            parts = obj.get("message", {}).get("parts", [])
            ts = obj.get("timestamp", "")
            for part in parts:
                fc = part.get("functionCall")
                if not fc:
                    continue
                tool_name = fc.get("name", "")
                args = fc.get("args", {})
                skill_name = args.get("skill") if isinstance(args, dict) else None
                yield tool_name, skill_name, ts


def main():
    parser = argparse.ArgumentParser(
        description="Count skill invocations across copilot-shell-format chat logs."
    )
    parser.add_argument(
        "--logs-dir",
        type=Path,
        default=Path(os.path.expanduser("~/.copilot-shell")),
        help="Root directory containing projects/*/chats/*.jsonl "
             "(default: ~/.copilot-shell)",
    )
    parser.add_argument(
        "--all-tools",
        action="store_true",
        help="Print every tool call, not only skill invocations",
    )
    parser.add_argument(
        "--top",
        type=int,
        default=50,
        help="Show only the top N entries (default: 50)",
    )
    args = parser.parse_args()

    files = iter_chat_files(args.logs_dir)
    if not files:
        print(f"No chat logs found under {args.logs_dir}")
        sys.exit(1)

    skill_counter = Counter()
    tool_counter = Counter()
    total_calls = 0
    file_count = 0

    for fp in files:
        file_count += 1
        for tool_name, skill_name, _ in parse_function_calls(fp):
            total_calls += 1
            tool_counter[tool_name] += 1
            if tool_name == "skill" and skill_name:
                skill_counter[skill_name] += 1

    print(f"Analyzed {file_count} chat sessions under {args.logs_dir}, "
          f"{total_calls} tool calls total\n")

    if args.all_tools:
        print("=== All Tool Call Statistics ===")
        for name, count in tool_counter.most_common(args.top):
            print(f"  {name}: {count}")
        print()

    print("=== Skill Invocation Statistics ===")
    if not skill_counter:
        print("  (no skill invocations found)")
    else:
        for name, count in skill_counter.most_common(args.top):
            print(f"  {name}: {count}")

    print(f"\nTotal skill invocations: {sum(skill_counter.values())}")


if __name__ == "__main__":
    main()
