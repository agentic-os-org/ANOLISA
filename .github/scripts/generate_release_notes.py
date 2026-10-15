#!/usr/bin/env python3
"""Generate release notes from the tagged component's own Git history."""

import argparse
import json
import re
import subprocess
from pathlib import Path
from urllib.parse import quote

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
    revision = f"{previous}..{args.tag}" if previous else args.tag
    # Summarize landed changes once, including merge commits that affect these paths.
    history = git(
        "log",
        "--first-parent",
        "--reverse",
        "--format=%H%x00%s%x00%an",
        revision,
        "--",
        *component["path_prefixes"],
    )
    base_url = f"https://github.com/{args.repository}"
    changes = []
    for line in history.splitlines():
        sha, subject, author = line.split("\0")
        changes.append(f"- {subject} ([{sha[:7]}]({base_url}/commit/{sha})) by {author}")
    encoded_tag = quote(args.tag, safe="/")
    if previous:
        compare_url = f"{base_url}/compare/{quote(previous, safe='/')}...{encoded_tag}"
    else:
        compare_url = f"{base_url}/commits/{encoded_tag}"
    args.output.write_text(
        f"## {component['display_name']} v{args.tag[len(prefix):]}\n\n"
        "## What's Changed\n\n"
        + ("\n".join(changes) if changes else "No changes in the component paths.")
        + f"\n\n**Full Changelog**: {compare_url}\n",
        encoding="utf-8",
    )
    print(compare_url)


if __name__ == "__main__":
    main()
