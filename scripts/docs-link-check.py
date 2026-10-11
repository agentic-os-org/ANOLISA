#!/usr/bin/env python3
"""Relative markdown link checker for the documentation tree.

Scans docs/**/*.md plus the root and component README files, extracts
relative links, and verifies each target exists in the repository.
External URLs, mailto and pure in-page anchors are skipped.
"""

from __future__ import annotations

import re
import subprocess
import sys
from bisect import bisect_right
from pathlib import Path

# Inline links and images: [text](target) / ![alt](target) — both follow the
# same relative path resolution rules, so a single pattern covers them.
LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
SKIP_PREFIXES = ("http://", "https://", "mailto:", "#", "<")


def inline_code_spans(text: str) -> list[tuple[int, int]]:
    """Locate matching backtick runs without crossing paragraph or block breaks."""
    cuts = {0, len(text)}
    offset = 0
    block = re.compile(
        r"^(?:#{1,6}(?:[ \t]|$)|(?:\*[ \t]*){3,}$|(?:-[ \t]*){3,}$"
        r"|(?:_[ \t]*){3,}$|(?:=+|-+)[ \t]*$)"
    )
    item = re.compile(r"^([-+*]|[0-9]{1,9}[.)])[ \t]+[^ \t]")
    paragraph_active = False
    list_delimiter = None
    for line in text.splitlines(keepends=True):
        content = re.sub(r"^ {0,3}(?:>[ \t]?)+", "", line).rstrip("\r\n")
        content = re.sub(r"^ {0,3}(?=\S)", "", content)
        if not content.strip(" \t") or block.match(content):
            cuts.update((offset, offset + len(line)))
            paragraph_active = False
            list_delimiter = None
        else:
            marker = item.match(content)
            # Only ordered lists starting at 1 interrupt an ordinary paragraph.
            same_list = marker and list_delimiter and marker[1].endswith(list_delimiter)
            if marker and (
                same_list
                or not paragraph_active
                or marker[1] in ("-", "+", "*", "1.", "1)")
            ):
                cuts.add(offset)
                list_delimiter = marker[1][-1] if marker[1][0].isdigit() else None
            paragraph_active = True
        offset += len(line)

    spans = []
    destinations = [(match.start(1), match.end(1)) for match in LINK_RE.finditer(text)]
    destination_starts = [start for start, _ in destinations]
    boundaries = sorted(cuts)
    for begin, end in zip(boundaries, boundaries[1:]):
        positions: dict[int, list[int]] = {}
        for run in re.finditer(r"`+", text[begin:end]):
            positions.setdefault(len(run.group()), []).append(begin + run.start())
        cursor = begin
        while cursor < end:
            if text[cursor] == "\\":
                cursor += 2
                continue
            if text[cursor] != "`":
                cursor += 1
                continue
            run_end = cursor + 1
            while run_end < end and text[run_end] == "`":
                run_end += 1
            size = run_end - cursor
            destination_index = bisect_right(destination_starts, cursor) - 1
            if destination_index >= 0 and cursor < destinations[destination_index][1]:
                cursor = run_end
                continue
            candidates = positions.get(size, [])
            next_index = bisect_right(candidates, cursor)
            if next_index == len(candidates):
                cursor = run_end
                continue
            closer = candidates[next_index]
            spans.append((cursor, closer + size))
            cursor = closer + size
    return spans


def repo_root() -> Path:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=True,
    )
    return Path(out.stdout.strip())


def files_to_check(root: Path) -> list[Path]:
    # Scope: the published docs tree, root-level docs, and component README
    # entry points. Deep in-tree docs (design notes, SKILL.md assets) are
    # intentionally excluded to keep the gate focused on reader-facing paths.
    out = subprocess.run(
        [
            "git",
            "ls-files",
            "docs/**/*.md",
            ":(glob)docs/*.md",
            ":(glob)*.md",
            ":(glob)src/*/README*.md",
            ":(glob)src/*/python/*/README*.md",
        ],
        capture_output=True,
        text=True,
        check=True,
        cwd=root,
    )
    return [root / line for line in out.stdout.splitlines() if line]


def check_file(md: Path, root: Path) -> list[str]:
    errors = []
    text = md.read_text(encoding="utf-8", errors="replace")
    # Strip fenced code blocks so shell snippets are not parsed as links.
    text = re.sub(r"```.*?```", "", text, flags=re.DOTALL)
    spans = inline_code_spans(text)
    span_index = 0
    for match in LINK_RE.finditer(text):
        while span_index < len(spans) and spans[span_index][1] <= match.start():
            span_index += 1
        if span_index < len(spans) and spans[span_index][0] <= match.start():
            continue
        target = match.group(1)
        if target.startswith(SKIP_PREFIXES):
            continue
        path_part = target.split("#", 1)[0]
        if not path_part:
            continue
        resolved = (md.parent / path_part).resolve()
        if not resolved.exists():
            errors.append(f"{md.relative_to(root)}: broken link -> {target}")
    return errors


def main() -> int:
    root = repo_root()
    errors: list[str] = []
    for md in files_to_check(root):
        if not md.exists():  # deleted but still in index during rebase etc.
            continue
        errors.extend(check_file(md, root))
    if errors:
        print(f"✗ {len(errors)} broken relative link(s):")
        for err in errors:
            print(f"    {err}")
        return 1
    print("✓ All relative links resolve")
    return 0


if __name__ == "__main__":
    sys.exit(main())
