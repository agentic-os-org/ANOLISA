#!/usr/bin/env python3
"""Publish the tagged component CHANGELOG version entry as release notes."""

import argparse
import json
import posixpath
import re
import subprocess
from collections.abc import Iterator
from pathlib import Path
from urllib.parse import quote, urlsplit

VersionKey = tuple[tuple[int, ...], bool, tuple[tuple[int, int | str], ...]]


def version_key(version: str) -> VersionKey | None:
    """Compare SemVer precedence, ignoring build metadata."""
    match = re.fullmatch(
        r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
        r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
        r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?",
        version,
    )
    if match is None:
        return None
    identifiers = match[4].split(".") if match[4] else []
    if any(part.isdigit() and len(part) > 1 and part.startswith("0") for part in identifiers):
        return None
    prerelease = tuple((0, int(part)) if part.isdigit() else (1, part) for part in identifiers)
    return tuple(int(match[index]) for index in (1, 2, 3)), not identifiers, prerelease


def git(*arguments: str) -> str:
    return subprocess.check_output(
        ["git", *arguments], text=True, encoding="utf-8", timeout=60
    ).strip()


def previous_tag(tag: str, prefix: str) -> str | None:
    current_version = version_key(tag[len(prefix) :])
    if current_version is None:
        raise ValueError(f"Release tag is not SemVer: {tag}")
    candidates = []
    for candidate in git("tag", "--merged", tag, "--list", prefix + "*").splitlines():
        version = version_key(candidate[len(prefix) :])
        if version is not None and version < current_version:
            candidates.append((version, candidate))
    previous = max(candidates, default=None)
    return previous[1] if previous else None


def markdown_lines(text: str) -> Iterator[tuple[str, bool]]:
    """Identify fenced examples so their headings and links stay literal."""
    fence = ""
    for line in text.splitlines(keepends=True):
        marker = re.match(r" {0,3}(`{3,}|~{3,})", line)
        literal = bool(fence or marker)
        if fence:
            if re.fullmatch(
                r" {0,3}" + re.escape(fence[0]) + r"{" + str(len(fence)) + r",}[ \t]*\n?",
                line,
            ):
                fence = ""
        elif marker:
            fence = marker[1]
        yield line, literal


def changelog_entry(text: str, version: str) -> str:
    """Extract an exact version section, preserving its user-facing Markdown."""
    headings = []
    offset = 0
    for line, literal in markdown_lines(text):
        heading = re.match(r" {0,3}##[ \t]+(.+?)[ \t]*#*[ \t]*$", line)
        if heading and not literal:
            headings.append((offset, offset + len(line), heading[1]))
        offset += len(line)
    pattern = r"(?:\[v?" + re.escape(version) + r"\]|v?" + re.escape(version) + r")(?:[ \t]+.*)?"
    matches = [i for i, (_, _, title) in enumerate(headings) if re.fullmatch(pattern, title)]
    if len(matches) != 1:
        raise ValueError(
            f"Expected exactly one CHANGELOG section for {version}; found {len(matches)}"
        )
    index = matches[0]
    end = headings[index + 1][0] if index + 1 < len(headings) else len(text)
    body = text[headings[index][1] : end].strip()
    visible = re.sub(r"<!--.*?-->", "", body, flags=re.DOTALL)
    visible = re.sub(r"(?m)^#{1,6}[ \t]+.*$|^\[[^\]]+\]:.*$|^(?:[-*_][ \t]*){3,}$", "", visible)
    if not visible.strip():
        raise ValueError(f"Empty CHANGELOG section for {version}")
    # Reference definitions can live after all version sections in Keep a Changelog.
    definitions = dict(
        (match[1].casefold(), match[0])
        for line, literal in markdown_lines(text)
        if not literal
        if (match := re.match(r" {0,3}\[([^\]]+)\]:[ \t]*.+", line))
    )
    for match in re.finditer(r"\[([^\]]+)\](?:\[([^\]]*)\])?", body):
        label = (match[2] or match[1]).casefold()
        definition = definitions.get(label)
        if definition and definition not in body:
            body += "\n\n" + definition
    return body


def release_links(body: str, path: str, commit: str, base_url: str) -> str:
    """Resolve relative Markdown destinations against the immutable source file."""

    def destination(target: str, image: bool = False) -> str:
        angle = target.startswith("<") and target.endswith(">")
        url = target[1:-1] if angle else target
        parts = urlsplit(url)
        if parts.scheme or parts.netloc:
            return target
        resolved = (
            posixpath.normpath(posixpath.join(posixpath.dirname(path), parts.path))
            if parts.path
            else path
        )
        if url.startswith("/"):
            resolved = parts.path.lstrip("/")
        route = "raw" if image else "blob"
        result = f"{base_url}/{route}/{commit}/{quote(resolved, safe='/%')}"
        if parts.query:
            result += "?" + parts.query
        if parts.fragment:
            result += "#" + parts.fragment
        return "<" + result + ">" if angle else result

    # Match inline code first, leaving example link syntax unchanged.
    inline = re.compile(
        r"(?P<code>(?P<ticks>`+)(?!`).*?(?<!`)(?P=ticks)(?!`))"
        r"|(?P<link>!?\[[^\]\n]*\]\()"
        r"(?P<target><[^>\n]+>|(?:[^()\s]+|\([^()\s]*\))+)"
        r"(?P<end>(?:[ \t]+[\"'][^\n]*?[\"'])?\))"
        r"|(?P<reference_image>!\[(?P<alt>[^\]\n]*)\](?:\[(?P<label>[^\]\n]*)\])?)"
    )
    image_labels = {
        (match["label"] or match["alt"]).casefold()
        for line, literal in markdown_lines(body)
        if not literal
        for match in inline.finditer(line)
        if match["reference_image"]
    }
    definition = re.compile(r"^( {0,3}\[([^\]]+)\]:[ \t]*)(<[^>\n]+>|\S+)")
    output = []
    for line, literal in markdown_lines(body):
        if not literal:
            line = inline.sub(
                lambda m: (
                    m[0]
                    if m["code"] or m["reference_image"]
                    else m["link"]
                    + destination(m["target"], image=m["link"].startswith("!"))
                    + m["end"]
                ),
                line,
            )
            line = definition.sub(
                lambda m: m[1] + destination(m[3], image=m[2].casefold() in image_labels), line
            )
        output.append(line)
    return "".join(output)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--prefix", required=True, help="Component release tag prefix")
    parser.add_argument("--repository", required=True, help="GitHub owner/repository")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    metadata = Path(__file__).resolve().parents[1] / "components.json"
    components = json.loads(metadata.read_text(encoding="utf-8"))["components"]
    component = next(
        (entry for entry in components if entry["release_tag_prefix"] == args.prefix),
        None,
    )
    if component is None:
        parser.error(f"Unknown component release prefix: {args.prefix}")
    if not args.tag.startswith(args.prefix):
        parser.error(f"Tag {args.tag} does not match component prefix {args.prefix}")
    prefix = component["release_tag_prefix"]
    previous = previous_tag(args.tag, prefix)
    path = component["path_prefixes"][0] + "CHANGELOG.md"
    commit = git("rev-parse", args.tag + "^{commit}")
    try:
        changelog = git("show", f"{commit}:{path}")
    except subprocess.CalledProcessError:
        parser.error(f"Cannot read {path} at {args.tag}")
    try:
        body = changelog_entry(changelog, args.tag[len(prefix) :])
    except ValueError as error:
        parser.error(f"{path} at {args.tag}: {error}")
    base_url = f"https://github.com/{args.repository}"
    body = release_links(body, path, commit, base_url)
    encoded_tag = quote(args.tag, safe="/")
    if previous:
        compare_url = f"{base_url}/compare/{quote(previous, safe='/')}...{encoded_tag}"
    else:
        compare_url = f"{base_url}/commits/{encoded_tag}"
    args.output.write_text(
        f"## {component['display_name']} v{args.tag[len(prefix):]}\n\n"
        + body
        + f"\n\n**Full Changelog**: {compare_url}\n",
        encoding="utf-8",
    )
    print(compare_url)


if __name__ == "__main__":
    main()
