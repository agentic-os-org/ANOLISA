#!/usr/bin/env python3
"""Relative markdown link checker for the documentation tree.

Scans docs/**/*.md, contribution guides, archived Copilot Shell docs, and README entry points,
extracts relative links, and verifies each target exists in the repository.
External URLs, mailto and pure in-page anchors are skipped.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

# Inline links and images: [text](target) / ![alt](target) — both follow the
# same relative path resolution rules, so a single pattern covers them.
LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
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
    for match in LINK_RE.finditer(text):
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
