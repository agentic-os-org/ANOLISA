#!/usr/bin/env python3
"""Relative markdown link checker for the documentation tree.

Scans docs/**/*.md, contribution guides, archived Copilot Shell docs, and README entry points,
extracts relative links, and verifies each target exists in the repository.
External URLs, mailto and pure in-page anchors are skipped.

Inline links and the CommonMark reference forms are both checked. A reference definition is
only checked when some link or image uses it, mirroring the inline rule: an unused definition
renders no link, so it cannot render a broken one.
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
# Link reference definitions: "[label]: target" with an optional title.
DEFINITION_RE = re.compile(
    r"""^ {0,3}\[([^\]\n]+)\]:[ \t]*(?:<([^>\n]+)>|([^\s]+))"""
    r"""(?:[ \t]+(?:"[^"]*"|'[^']*'|\([^)]*\)))?[ \t]*$""",
    re.MULTILINE,
)
# Full and collapsed references: [text][label], [text][], ![alt][label].
FULL_REFERENCE_RE = re.compile(r"!?\[([^\[\]]*)\]\[([^\[\]]*)\]")
# Shortcut references: [label] / ![label] that are not inline links and not the
# text half of a full reference.
SHORTCUT_REFERENCE_RE = re.compile(r"!?\[([^\[\]]+)\](?![\[(])")
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


def normalize_label(label: str) -> str:
    """Fold a reference label the way CommonMark matches one: whitespace, then case."""
    return " ".join(label.split()).casefold()


def relative_targets(text: str) -> list[str]:
    """Return every relative link target the document would render, in order.

    Definitions themselves are not links; only a definition that some full, collapsed or
    shortcut reference actually uses contributes a target.
    """
    definitions = {
        normalize_label(match.group(1)): match.group(2) or match.group(3)
        for match in DEFINITION_RE.finditer(text)
    }
    # A definition line is not a link, and its "[label]" must not be read as a
    # shortcut reference either.
    text = DEFINITION_RE.sub(lambda match: " " * len(match.group(0)), text)

    targets = [match.group(1) for match in LINK_RE.finditer(text)]
    # Blank what has been consumed so reference scanning cannot read link text,
    # alt text or an inline target as another kind of reference.
    text = LINK_RE.sub(lambda match: " " * len(match.group(0)), text)

    for match in FULL_REFERENCE_RE.finditer(text):
        # A collapsed reference repeats its own text as the label.
        target = definitions.get(normalize_label(match.group(2) or match.group(1)))
        if target is not None:
            targets.append(target)
    text = FULL_REFERENCE_RE.sub(lambda match: " " * len(match.group(0)), text)

    for match in SHORTCUT_REFERENCE_RE.finditer(text):
        target = definitions.get(normalize_label(match.group(1)))
        if target is not None:
            targets.append(target)

    checked = []
    for target in targets:
        if target.startswith(SKIP_PREFIXES) or not unquote(target.split("#", 1)[0]):
            continue
        checked.append(target)
    return list(dict.fromkeys(checked))


def check_file(md: Path, root: Path) -> list[str]:
    text = md.read_text(encoding="utf-8", errors="replace")
    # Strip fenced code blocks so shell snippets are not parsed as links.
    text = re.sub(r"```.*?```", "", text, flags=re.DOTALL)
    errors = []
    for target in relative_targets(text):
        resolved = (md.parent / unquote(target.split("#", 1)[0])).resolve()
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
