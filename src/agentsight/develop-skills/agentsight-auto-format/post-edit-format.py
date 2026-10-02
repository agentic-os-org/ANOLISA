#!/usr/bin/env python3
"""Post-edit auto-format for AI agents.

Reads edited file paths from stdin and runs the appropriate formatter.
Failures are logged to stderr but never block the agent workflow.

Stdin formats (auto-detected):
  - Hook JSON: {"tool_name":"Edit","tool_input":{"file_path":"..."},...}
  - Plain text: one file path per line

Supported formatters:
  .rs          -> rustfmt
  .py          -> ruff format (fallback: black)
  .ts/.tsx     -> prettier --write (must be locally installed)
"""

import json
import os
import shutil
import subprocess
import sys


def format_rust(path):
    rustfmt = shutil.which("rustfmt")
    if rustfmt:
        subprocess.run(
            [rustfmt, path],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=30,
        )


def format_python(path):
    ruff = shutil.which("ruff")
    if ruff:
        subprocess.run(
            [ruff, "format", path],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=30,
        )
        return
    black = shutil.which("black")
    if black:
        subprocess.run(
            [black, "--quiet", path],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=30,
        )


def format_typescript(path):
    prettier = shutil.which("prettier")
    if prettier:
        subprocess.run(
            [prettier, "--write", path],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=30,
        )


FORMATTERS = {
    ".rs": format_rust,
    ".py": format_python,
    ".ts": format_typescript,
    ".tsx": format_typescript,
}


def extract_paths(raw):
    """Extract file paths from stdin (hook JSON or plain text)."""
    paths = []
    try:
        data = json.loads(raw)
        if isinstance(data, dict):
            # Hook format: {"tool_input": {"file_path": "..."}, ...}
            tool_input = data.get("tool_input", {})
            if isinstance(tool_input, dict) and "file_path" in tool_input:
                paths.append(tool_input["file_path"])
            # Legacy: {"file_path": "..."}
            elif "file_path" in data:
                paths.append(data["file_path"])
            # Legacy MultiEdit: {"edits": [{"file_path": "..."}, ...]}
            for edit in data.get("edits", []):
                if isinstance(edit, dict) and "file_path" in edit:
                    paths.append(edit["file_path"])
    except (json.JSONDecodeError, TypeError):
        paths = [line.strip() for line in raw.splitlines() if line.strip()]
    return paths


def main():
    raw = sys.stdin.read().strip()
    if not raw:
        return

    for path in extract_paths(raw):
        if not os.path.isfile(path):
            continue
        ext = os.path.splitext(path)[1].lower()
        formatter = FORMATTERS.get(ext)
        if formatter:
            try:
                formatter(path)
            except Exception as e:
                print("auto-format: %s: %s" % (path, e), file=sys.stderr)


if __name__ == "__main__":
    main()
