#!/usr/bin/env python3
"""Relative markdown link checker for the documentation tree.

Scans docs/**/*.md, contribution guides, archived Copilot Shell docs, and README entry points,
extracts relative links (inline and reference-style), and verifies each target
exists in the repository.
External URLs, mailto and pure in-page anchors are skipped.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote

# Inline links and images: [text](target) / ![alt](target) — both follow the
# same relative path resolution rules, so a single pattern covers them.
LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
# Reference links: usages [text][label] / [text][] / ![alt][label] and their
# definitions [label]: target elsewhere in the same document.
REF_USE_RE = re.compile(r"!?\[([^\]]*)\]\[([^\]]*)\]")
REF_DEF_RE = re.compile(r"^ {0,3}\[([^\]]+)\]:\s*(?:<([^<>]*)>|(\S+))", re.MULTILINE)
SKIP_PREFIXES = ("http://", "https://", "mailto:", "#", "<")


def repo_root() -> Path:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True
    )
    return Path(out.stdout.strip())


def files_to_check(root: Path) -> list[Path]:
    # Keep archived reader-facing docs checked after they leave the website.
    # Deep design notes and SKILL.md assets remain outside this gate.
    out = subprocess.run(
        [
            "git",
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "docs/**/*.md",
            "contribute/**/*.md",
            ":(glob)docs/*.md",
            ":(glob)*.md",
            ":(glob)src/*/README*.md",
            ":(glob)src/*/CONTRIBUTING*.md",
            ":(glob)src/*/python/*/README*.md",
            ":(glob)deprecated/copilot-shell/README*.md",
            ":(glob)deprecated/copilot-shell/docs/**/*.md",
        ],
        capture_output=True,
        check=True,
        cwd=root,
    )
    return [root / os.fsdecode(name) for name in out.stdout.split(b"\0") if name]


def reference_label(label: str) -> str:
    # CommonMark matches reference labels case-insensitively after trimming
    # and collapsing internal whitespace.
    return " ".join(label.strip().casefold().split())


def broken_relative_target(md: Path, root: Path, target: str) -> bool:
    if target.startswith(SKIP_PREFIXES):
        return False
    path_part = unquote(target.split("#", 1)[0])
    if not path_part:
        return False
    resolved = (md.parent / path_part).resolve()
    return not resolved.exists()


def check_file(md: Path, root: Path) -> list[str]:
    errors = []
    text = md.read_text(encoding="utf-8", errors="replace")
    # Strip fenced code blocks so shell snippets are not parsed as links.
    text = re.sub(r"```.*?```", "", text, flags=re.DOTALL)
    for match in LINK_RE.finditer(text):
        if broken_relative_target(md, root, match.group(1)):
            errors.append(f"{md.relative_to(root)}: broken link -> {match.group(1)}")
    definitions = {}
    for match in REF_DEF_RE.finditer(text):
        target = match.group(2) if match.group(2) is not None else match.group(3)
        definitions.setdefault(reference_label(match.group(1)), target)
    for match in REF_USE_RE.finditer(text):
        label = match.group(2) or match.group(1)
        target = definitions.get(reference_label(label))
        if target is None:
            # A usage without a definition renders as plain text, not a link.
            continue
        if broken_relative_target(md, root, target):
            errors.append(f"{md.relative_to(root)}: broken reference link -> {target}")
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
