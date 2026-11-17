#!/usr/bin/env python3
"""Update or verify the checked-in COSH Skill lists against the release catalog."""

import argparse
import json
from pathlib import Path
from typing import List

from release_catalog import read_catalog


def sync_manifests(source: Path, *, write: bool) -> List[Path]:
    """Return manifests whose Skill lists differ from the release catalog."""
    released = read_catalog(source)
    categories = {path.parent for path in released}
    categories.update(
        path.parent.relative_to(source) for path in source.glob("*/cosh-extension.json")
    )
    expected = {Path("."): [path.as_posix() for path in released]}
    for category in sorted(categories):
        expected[category] = [path.name for path in released if path.parent == category]

    changed = []
    for directory, skills in expected.items():
        path = source / directory / "cosh-extension.json"
        manifest = json.loads(path.read_text(encoding="utf-8"))
        if manifest.get("skills") != skills:
            changed.append(path)
            if write:
                manifest["skills"] = skills
                path.write_text(
                    json.dumps(manifest, ensure_ascii=False, indent=2) + "\n",
                    encoding="utf-8",
                )
    return changed


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--write", action="store_true", help="Update each manifest's Skill list."
    )
    args = parser.parse_args()
    source = Path(__file__).resolve().parents[1]
    try:
        changed = sync_manifests(source, write=args.write)
    except (OSError, ValueError) as error:
        parser.exit(1, f"{error}\n")
    if changed and not args.write:
        print(
            "COSH manifests differ from packaging/released-skills.txt. Run this script with --write."
        )
        for path in changed:
            print(path.relative_to(source))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
