#!/usr/bin/env python3
"""Relative markdown link checker for the documentation tree.

Scans docs/**/*.md plus the root and component README files, extracts
relative links, and verifies each target exists in the repository.
External URLs, mailto and pure in-page anchors are skipped.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

# Inline links and images: [text](target) / ![alt](target) — both follow the
# same relative path resolution rules, so a single pattern covers them.
LINK_RE = re.compile(
    r"""!?\[[^\]]*\]\((<[^>\n]*>|[^)\s]+)"""
    r"""(?:\s+(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\((?:\\.|[^()\\])*\)))?\)"""
)
SCHEME_RE = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")


def normalize_reference_label(label: str) -> str:
    return " ".join(label.split()).casefold()


def reference_prose(
    text: str, mask_inline: bool = True, mask_comments: bool = True
) -> str:
    """Mask code only for reference discovery; keep original character offsets."""
    lines = []
    fence = ""
    fence_size = 0
    fence_re = re.compile(r"^ {0,3}(" + chr(96) + r"{3,}|~{3,})(.*)$")
    for line in text.splitlines(keepends=True):
        content = line.rstrip("\r\n")
        # Recognize simple container fence openers without interpreting their
        # contents as reference uses. Complex container extent is conservative.
        content = re.sub(r"^(?: {0,3}>[ \t]?)+", "", content)
        content = re.sub(r"^ {0,3}(?:[-+*]|[0-9]{1,9}[.)])[ \t]{1,4}", "", content)
        match = fence_re.match(content)
        if fence:
            lines.append(re.sub(r"[^\r\n]", " ", line))
            if (
                match
                and match[1][0] == fence
                and len(match[1]) >= fence_size
                and not match[2].strip()
            ):
                fence = ""
            continue
        if match and (match[1][0] == "~" or chr(96) not in match[2]):
            fence, fence_size = match[1][0], len(match[1])
            lines.append(re.sub(r"[^\r\n]", " ", line))
        elif line.startswith(("    ", "\t")):
            lines.append(re.sub(r"[^\r\n]", " ", line))
        else:
            lines.append(line)
    prose = "".join(lines)
    masked = list(prose)
    cursor = 0
    while cursor < len(prose):
        if prose[cursor] == "\\":
            cursor += 2
            continue
        if mask_comments and prose.startswith("<!--", cursor):
            comment_end = prose.find("-->", cursor + 4)
            comment_end = len(prose) if comment_end < 0 else comment_end + 3
            for position in range(cursor, comment_end):
                if masked[position] not in "\r\n":
                    masked[position] = " "
            cursor = comment_end
            continue
        if prose[cursor] != chr(96):
            cursor += 1
            continue
        end = cursor + 1
        while end < len(prose) and prose[end] == chr(96):
            end += 1
        run = prose[cursor:end]
        closer = end
        while True:
            closer = prose.find(run, closer)
            if closer < 0 or re.search(r"\n[ \t]*\n", prose[end:closer]):
                cursor = end
                break
            if (closer == 0 or prose[closer - 1] != chr(96)) and (
                closer + len(run) == len(prose) or prose[closer + len(run)] != chr(96)
            ):
                if mask_inline:
                    for position in range(cursor, closer + len(run)):
                        if masked[position] not in "\r\n":
                            masked[position] = " "
                cursor = closer + len(run)
                break
            closer += len(run)
    return "".join(masked)


def reference_links(text: str) -> tuple[str, list[str]]:
    """Resolve references using original labels and validated definition extents."""
    prose = reference_prose(text, mask_inline=False, mask_comments=False)
    visible = reference_prose(text)
    definition_start = re.compile(r"^ {0,3}\[([^\[\]\n]+)\]:[ \t]*(.*)$")
    title = r"""(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\((?:\\.|[^()\\])*\))"""
    definition_value = re.compile(
        r"^(<[^>\n]*>|[^\s<>]+)(?:[ \t]+(" + title + r"))?[ \t]*$"
    )
    title_value = re.compile(r"^[ \t]*" + title + r"[ \t]*$")
    boundary = re.compile(
        r"^ {0,3}(?:#{1,6}(?:[ \t]|$)|(?:\*[ \t]*){3,}$|"
        r"(?:_[ \t]*){3,}$|(?:-[ \t]*){3,}$)"
    )
    definitions: dict[str, str | None] = {}
    uses = list(prose)
    inline_text = list(text)
    lines = prose.splitlines(keepends=True)
    visible_lines = visible.splitlines(keepends=True)
    continuation_until = 0
    previous_definition = False
    offset = 0
    for index, line in enumerate(lines):
        content = line.rstrip("\r\n")
        if index < continuation_until:
            valid = True
        else:
            container = re.sub(r"^(?: {0,3}>[ \t]?)+", "", content)
            container = re.sub(
                r"^ {0,3}(?:[-+*]|[0-9]{1,9}[.)])[ \t]{1,4}", "", container
            )
            complex_definition = container != content
            setext_boundary = (
                index >= 2
                and bool(lines[index - 2].strip())
                and boundary.match(lines[index - 2].rstrip("\r\n")) is None
                and re.fullmatch(r" {0,3}=+[ \t]*", lines[index - 1].rstrip("\r\n"))
                is not None
            )
            block_start = (
                index == 0
                or not lines[index - 1].strip()
                or previous_definition
                or setext_boundary
                or boundary.match(lines[index - 1].rstrip("\r\n")) is not None
            )
            match = (
                definition_start.match(container)
                if block_start or complex_definition
                else None
            )
            # Visibility excludes examples; validation and lookup retain the
            # original source, including code spans/comments inside labels.
            bracket = content.find("[")
            if match and (bracket < 0 or visible_lines[index][bracket] != "["):
                match = None
            value = definition_value.fullmatch(match[2]) if match else None
            end = index + 1
            multiline_destination = False
            if match and not match[2].strip() and end < len(lines):
                value = definition_value.fullmatch(lines[end].strip())
                if value:
                    multiline_destination = True
                    end += 1
            valid = value is not None
            if valid:
                if (
                    value[2] is None
                    and end < len(lines)
                    and lines[end].lstrip().startswith(('"', "'", "("))
                ):
                    # Only an actually terminated title belongs to this
                    # definition. An incomplete candidate remains normal prose.
                    for title_end in range(end + 1, len(lines) + 1):
                        if not lines[title_end - 1].strip():
                            break
                        candidate = "".join(lines[end:title_end]).rstrip("\r\n")
                        if title_value.fullmatch(candidate):
                            end = title_end
                            break
                label = normalize_reference_label(match[1])
                if label:
                    # Container/multiline destinations retain the existing
                    # conservative first-definition reservation.
                    target = (
                        None
                        if complex_definition or multiline_destination
                        else value[1]
                    )
                    definitions.setdefault(label, target)
                continuation_until = end
        previous_definition = valid
        if valid:
            for position in range(offset, offset + len(line)):
                if uses[position] not in "\r\n":
                    uses[position] = " "
                if inline_text[position] not in "\r\n":
                    inline_text[position] = " "
        offset += len(line)

    text_for_uses = reference_prose("".join(uses))
    targets: list[str] = []

    def close_bracket(start: int, stop: int) -> int | None:
        depth = 1
        position = start + 1
        while position < stop:
            char = text_for_uses[position]
            if char == "\\":
                position += 2
                continue
            if char == "[":
                depth += 1
            elif char == "]":
                depth -= 1
                if depth == 0:
                    return position
            if char == "\n" and re.match(
                r"[ \t]*\n", text_for_uses[position + 1 : stop]
            ):
                return None
            position += 1
        return None

    def scan(start: int, stop: int, allow_links: bool = True) -> None:
        cursor = start
        while cursor < stop:
            if text_for_uses[cursor] == "\\":
                cursor += 2
                continue
            if text_for_uses[cursor] != "[":
                cursor += 1
                continue
            end = close_bracket(cursor, stop)
            if end is None:
                cursor += 1
                continue
            image = cursor > 0 and text_for_uses[cursor - 1] == "!"
            if not allow_links and not image:
                scan(cursor + 1, end, allow_links=False)
                cursor = end + 1
                continue
            label = text[cursor + 1 : end]
            following = end + 1
            inline = None
            if following < stop and text_for_uses[following] == "(":
                # Validate the original destination/title with the unchanged
                # inline grammar, independently of nested label brackets.
                inline = LINK_RE.match("[]" + text[following:stop])
            if inline:
                scan(cursor + 1, end, allow_links=False)
                # The legacy flat inline pass cannot see a nested outer label.
                if LINK_RE.match(text, cursor) is None:
                    targets.append(inline[1])
                cursor = following + inline.end() - 2
                continue
            if following < stop and text_for_uses[following] == "[":
                reference_end = close_bracket(following, stop)
                if reference_end is not None:
                    explicit = text[following + 1 : reference_end]
                    label = explicit or label
                    following = reference_end + 1
            target = definitions.get(normalize_reference_label(label))
            if target is not None:
                targets.append(target)
                scan(cursor + 1, end, allow_links=False)
                cursor = following
            else:
                # Invalid inline candidates and unresolved outer labels must
                # not consume later references or legal inner shortcut links.
                scan(cursor + 1, end, allow_links=allow_links)
                cursor = end + 1

    scan(0, len(text_for_uses))
    return "".join(inline_text), targets


def repo_root() -> Path:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=True,
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


def check_file(md: Path, root: Path) -> list[str]:
    errors = []
    text = md.read_text(encoding="utf-8", errors="replace")
    # Strip fenced code blocks so shell snippets are not parsed as links.
    text = re.sub(r"```.*?```", "", text, flags=re.DOTALL)
    text, references = reference_links(text)
    targets = [match.group(1) for match in LINK_RE.finditer(text)]
    for target in targets + references:
        destination = target[1:-1] if target.startswith("<") else target
        if SCHEME_RE.match(destination) or destination.startswith("//"):
            continue
        path_part = unquote(urlsplit(destination).path)
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
