#!/usr/bin/env python3
"""Compare package contents across prefixes without comparing installation identities."""

import json
from pathlib import Path
import sys


def packages(path: Path) -> dict:
    """Require a successful single-prefix query and retain the package build facts."""
    report = json.loads(path.read_text(encoding="utf-8"))
    if report["format"] != 1 or report["status"] != "succeeded":
        raise ValueError("installed-package query did not succeed")
    inventory = report["result"]["inventory"]
    if inventory["source"] != "database" or len(inventory["installations"]) != 1:
        raise ValueError("expected one registered installation")
    result = {}
    installation = inventory["installations"][0]
    for package in installation["packages"]:
        name = package["package_id"]
        if name in result:
            raise ValueError(f"duplicate installed package: {name}")
        result[name] = {
            key: package[key] for key in ("version", "os", "arch", "files")
        }
        result[name]["source_commit"] = package["source"].get("source_commit")
        result[name]["provider"] = package.get("provider")
        if any(dependency["installation_id"] != installation["installation_id"]
               for dependency in package["dependencies"]):
            raise ValueError("acceptance bundle dependency leaves the selected prefix")
        result[name]["dependencies"] = sorted(
            (dependency["package_id"], dependency["version"])
            for dependency in package["dependencies"]
        )
    if not result:
        raise ValueError("installed package comparison must not be empty")
    return result


def main(arguments: list[str]) -> int:
    if len(arguments) != 2:
        raise ValueError("expected two aw list --installed JSON reports")
    if packages(Path(arguments[0])) != packages(Path(arguments[1])):
        raise ValueError("installed package contents differ across prefixes")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
