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
    text = strip_fenced_code(text)
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


def strip_fenced_code(text: str) -> str:
    """Exclude line-delimited code fences, including unclosed examples."""
    prose: list[str] = []
    fence_char = ""
    fence_length = 0
    quote_depth = 0
    list_indent = 0
    list_indents: list[int] = []
    list_quote_depth = 0
    list_paragraph = False
    for line in text.splitlines(keepends=True):
        content = line.rstrip("\r\n").expandtabs(4)
        quote = re.match(r"^(?: {0,3}> ?)*", content).group()
        depth = quote.count(">")
        content = content[len(quote) :]
        if fence_char and (
            depth < quote_depth
            or (list_indent and content.strip() and not content.startswith(" " * list_indent))
        ):
            # Leaving a container also ends its unclosed fenced block.
            fence_char = ""
            list_paragraph = False
        if fence_char:
            if list_indent:
                content = content[list_indent:]
            match = re.match(r"^ {0,3}(`{3,}|~{3,})(.*)$", content)
            if (
                match
                and depth == quote_depth
                and match.group(1)[0] == fence_char
                and len(match.group(1)) >= fence_length
                and not match.group(2).strip()
            ):
                fence_char = ""
            continue
        if depth != list_quote_depth:
            list_indents.clear()
            list_quote_depth = depth
            list_paragraph = False
        marker = re.match(r"^( *)(?:[-+*]|[0-9]{1,9}[.)]) {1,4}(?=\S)", content)
        if marker:
            indentation = len(marker.group(1))
            allowed_indent = (list_indents[-1] if list_indents else 0) + 3
            if indentation > allowed_indent:
                marker = None
        if marker:
            while list_indents and indentation < list_indents[-1]:
                list_indents.pop()
            list_indents.append(marker.end())
        elif content.strip() and (not list_paragraph or starts_markdown_block(content)):
            while list_indents and not content.startswith(" " * list_indents[-1]):
                list_indents.pop()
        list_indent = list_indents[-1] if list_indents else 0
        if list_indent and (marker or content.startswith(" " * list_indent)):
            content = content[list_indent:]
        match = re.match(r"^ {0,3}(`{3,}|~{3,})(.*)$", content)
        if match and (match.group(1)[0] == "~" or "`" not in match.group(2)):
            fence_char = match.group(1)[0]
            fence_length = len(match.group(1))
            quote_depth = depth
            list_paragraph = False
        else:
            prose.append(line)
            # An outdented paragraph can lazily continue a list item. A blank
            # line or another block ends that allowance, but retains the list
            # indentation for a subsequent indented fence.
            list_paragraph = bool(list_indents and content.strip()) and not starts_markdown_block(
                content
            )
    return "".join(prose)


def starts_markdown_block(content: str) -> bool:
    """Recognize blocks that terminate a lazy list paragraph continuation."""
    if re.match(r"^ {0,3}(?:#{1,6}(?:\s|$)|`{3,}[^`]*$|~{3,})", content):
        return True
    if re.fullmatch(r" {0,3}(?:(?:\*\s*){3,}|(?:-\s*){3,}|(?:_\s*){3,})", content):
        return True
    if re.match(r"^ {0,3}<(?:!--|\?|!\[CDATA\[|![A-Z])", content):
        return True
    # HTML block tags can start a block with text after the opening tag.
    block_tags = (
        "address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|"
        "details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|"
        "h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|nav|noframes|ol|"
        "optgroup|option|p|param|search|section|summary|table|tbody|td|tfoot|th|thead|title|tr|"
        "track|ul|pre|script|style|textarea"
    )
    if re.match(rf"^ {{0,3}}</?(?:{block_tags})(?:\s|/?>|$)", content, re.IGNORECASE):
        return True
    # A complete HTML tag on its own line also leaves the list container.
    return bool(re.fullmatch(r" {0,3}</?[A-Za-z][A-Za-z0-9-]*(?:\s+[^<>]*)?/?>\s*", content))


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
