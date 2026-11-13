#!/usr/bin/env python3
"""Select the Skill directories and adapter entries used by default delivery."""

import argparse
from pathlib import Path
import re
import shutil
from typing import List


CATALOG = Path("packaging/released-skills.txt")
SKILL_PATH = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]*/[A-Za-z0-9][A-Za-z0-9_.-]*")
TABLE = re.compile(r"(?m)^[ \t]*\[")
ADAPTER_SKILL = re.compile(r"\[\[adapters\.(openclaw|hermes)\.skills\]\][ \t]*(?:#.*)?")
NAME = re.compile(
    r"""(?m)^[ \t]*name[ \t]*=[ \t]*["']([A-Za-z0-9][A-Za-z0-9_.-]*)["'][ \t]*(?:#.*)?$"""
)
SOURCE = re.compile(
    r"""(?m)^[ \t]*source[ \t]*=[ \t]*["']\{datadir\}/skills/([A-Za-z0-9][A-Za-z0-9_.-]*)/["'][ \t]*(?:#.*)?$"""
)
SKILLS_KEY = re.compile(
    r"""(?m)^[ \t]*(?:(?:[\w-]+|["'][^"'\n]+["'])[ \t]*\.[ \t]*)*["']?skills["']?[ \t]*="""
)
ADAPTER_TABLE = re.compile(r"\[\[adapters\]\][ \t]*(?:#.*)?")
ADAPTER_VALUE = re.compile(
    r'(?P<key>framework|name|display_name|adapter_type|trust|dest)[ \t]*=[ \t]*"(?P<value>[^"\\\r\n]+)"'
    r"[ \t]*(?:#.*)?"
)
ADAPTER_DETECT = re.compile(
    r'detect[ \t]*=[ \t]*\{[ \t]*binary[ \t]*=[ \t]*"[\w.-]+"[ \t]*\}[ \t]*(?:#.*)?'
)


def read_catalog(source: Path) -> List[Path]:
    """Read the release list and verify every entry names a real Skill."""
    result = []
    names = set()
    for line in (source / CATALOG).read_text(encoding="utf-8").splitlines():
        item = line.split("#", 1)[0].strip()
        if not item:
            continue
        if not SKILL_PATH.fullmatch(item):
            raise ValueError(f"invalid released Skill directory: {item}")
        path = Path(item)
        folder = source / path
        manifest = folder / "SKILL.md"
        if (
            (source / path.parent).is_symlink()
            or folder.is_symlink()
            or not manifest.is_file()
            or manifest.is_symlink()
        ):
            raise ValueError(f"released Skill has no regular SKILL.md: {path}")
        if path.name in names:
            raise ValueError(f"duplicate released Skill name: {path.name}")
        result.append(path)
        names.add(path.name)
    if not result:
        raise ValueError("the OS Skills release list is empty")
    return result


def release_manifest(text: str, released: List[Path]) -> str:
    """Filter the manifest's two adapter arrays while preserving unrelated TOML."""
    names = {path.name for path in released}
    starts = [match.start() for match in TABLE.finditer(text)]
    output = [text[: starts[0]]] if starts else [text]
    if SKILLS_KEY.search(output[0]):
        raise ValueError("unsupported top-level OS Skills adapter declaration")
    seen = set()
    framework = None
    for start, end in zip(starts, starts[1:] + [len(text)]):
        section = text[start:end]
        header, _, contents = section.lstrip().partition("\n")
        lines = [
            line.strip()
            for line in contents.splitlines()
            if line.strip() and not line.lstrip().startswith("#")
        ]
        skill_table = ADAPTER_SKILL.fullmatch(header)
        if ADAPTER_TABLE.fullmatch(header):
            values = {}
            for line in lines:
                field = ADAPTER_VALUE.fullmatch(line)
                if field:
                    if field["key"] in values:
                        raise ValueError(
                            f"duplicate OS Skills adapter field: {field['key']}"
                        )
                    values[field["key"]] = field["value"]
                elif not ADAPTER_DETECT.fullmatch(line):
                    raise ValueError("unsupported field in OS Skills adapter table")
            framework = values.get("framework")
            if framework not in {"openclaw", "hermes"}:
                raise ValueError(
                    f"unsupported OS Skills adapter framework: {framework}"
                )
        elif skill_table:
            entry_names = NAME.findall(contents)
            sources = SOURCE.findall(contents)
            if (
                skill_table[1] != framework
                or len(lines) != 2
                or len(entry_names) != 1
                or sources != entry_names
            ):
                raise ValueError(f"invalid OS Skills adapter entry in {header}")
            key = (skill_table[1], entry_names[0])
            if key in seen:
                raise ValueError(
                    f"duplicate OS Skills adapter entry in {header}: {entry_names[0]}"
                )
            seen.add(key)
            if entry_names[0] not in names:
                continue
        elif "adapters" in header or SKILLS_KEY.search(contents):
            raise ValueError(f"unsupported OS Skills adapter declaration in {header}")
        output.append(section)
    return "".join(output)


def stage(source: Path, destination: Path, released: List[Path], layout: str) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    if destination.is_symlink() or any(destination.iterdir()):
        raise ValueError(f"OS Skills staging destination must be empty: {destination}")
    for path in released:
        target = destination / (path if layout == "source" else path.name)
        shutil.copytree(source / path, target)


def check_payload(source: Path, released: List[Path]) -> None:
    """Do not let a released directory pull in unlisted Skill roots or links."""
    for path in released:
        folder = source / path
        for child in folder.rglob("*"):
            if child.is_symlink():
                raise ValueError(
                    f"released Skill payload contains a symlink: {child.relative_to(source)}"
                )
            if child.name == "SKILL.md" and child.parent != folder:
                raise ValueError(
                    f"released Skill payload contains another Skill: {child.relative_to(source)}"
                )
            if not (child.is_file() or child.is_dir()):
                raise ValueError(
                    f"released Skill payload is not a regular file or directory: {child.relative_to(source)}"
                )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action")
    commands.required = True  # Python 3.6 does not accept required as a keyword.
    commands.add_parser("paths").add_argument("source", type=Path)
    manifest = commands.add_parser("manifest")
    manifest.add_argument("source", type=Path)
    manifest.add_argument("output", type=Path)
    staging = commands.add_parser("stage")
    staging.add_argument("source", type=Path)
    staging.add_argument("destination", type=Path)
    staging.add_argument("--layout", required=True, choices=("source", "flat"))
    args = parser.parse_args()
    try:
        released = read_catalog(args.source)
        if args.action == "paths":
            check_payload(args.source, released)
            for path in released:
                print(f"./{path.as_posix()}/SKILL.md")
        elif args.action == "manifest":
            text = (args.source / "component.toml").read_text(encoding="utf-8")
            args.output.write_text(release_manifest(text, released), encoding="utf-8")
        else:
            check_payload(args.source, released)
            stage(args.source, args.destination, released, args.layout)
    except (OSError, ValueError) as error:
        parser.exit(1, f"os-skills release catalog: {error}\n")


if __name__ == "__main__":
    main()
