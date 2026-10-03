#!/usr/bin/env python3
"""Stage the in-repository AW protocol dependency inside sec-core Source0."""

import argparse
import shutil
import subprocess
from pathlib import Path


def stage(repository: Path, destination: Path) -> None:
    """Copy tracked AW sources and rewrite only the staged sec-core path."""
    manifest = destination / "v2/Cargo.toml"
    original = 'aw-provider = { path = "../../aw/crates/aw-provider" }'
    replacement = 'aw-provider = { path = "../third_party/aw/crates/aw-provider" }'
    contents = manifest.read_text()
    if contents.count(original) != 1:
        raise ValueError("expected exactly one repository-local AW Provider dependency")
    # Container-mounted checkouts may have another UID. Trust only the explicit
    # source checkout for this read, without changing global Git configuration.
    output = subprocess.run(
        ["git", "-c", f"safe.directory={repository}", "ls-files", "-z", "src/aw"],
        cwd=repository,
        stdout=subprocess.PIPE,
        check=True,
        timeout=30,
    )
    files = [Path(item.decode()) for item in output.stdout.split(b"\0") if item]
    if Path("src/aw/Cargo.toml") not in files:
        raise ValueError("AW workspace is absent from the source checkout")
    target = destination / "third_party/aw"
    target.mkdir(parents=True, exist_ok=False)
    # Keep the complete tracked workspace, including inherited Cargo settings,
    # schemas and fixtures. Exclude untracked build output without guessing a
    # dependency closure that would drift as the shared contract evolves.
    for file in files:
        staged = target / file.relative_to("src/aw")
        staged.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(repository / file, staged, follow_symlinks=True)
    manifest.write_text(contents.replace(original, replacement))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repository", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    stage(args.repository.resolve(), args.destination.resolve())


if __name__ == "__main__":
    main()
