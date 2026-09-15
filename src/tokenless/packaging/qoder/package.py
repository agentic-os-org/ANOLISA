#!/usr/bin/env python3
"""Assemble a standalone Qoder plugin from version-matched prebuilt binaries."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TARGETS = {
    "linux-x64": ("linux", "x86_64"),
    "linux-arm64": ("linux", "aarch64"),
    "darwin-arm64": ("macos", "aarch64"),
    "darwin-x64": ("macos", "x86_64"),
}


def main() -> None:
    """Validate all inputs before creating a fresh plugin output directory."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prebuilt-root", type=Path, required=True)
    parser.add_argument("--target", choices=TARGETS, action="append", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    version = re.search(r'^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M)[1]
    if args.output.exists():
        parser.error("output already exists; choose a fresh directory")
    for target in dict.fromkeys(args.target):
        source = args.prebuilt_root / target
        if (source / "version.txt").read_text().strip() != version:
            parser.error(f"{target}/version.txt must match source version {version}")
        system, arch = TARGETS[target]
        subprocess.run(
            [
                sys.executable,
                str(ROOT / "packaging/raw/verify-binaries.py"),
                "--os",
                system,
                "--arch",
                arch,
                str(source / "tokenless"),
                str(source / "rtk"),
            ],
            check=True,
        )
    adapter = ROOT / "adapters/tokenless"
    shutil.copytree(
        adapter / "qoder", args.output, ignore=shutil.ignore_patterns("*.in", "scripts", "commands")
    )
    manifest = (adapter / "qoder/.qoder-plugin/plugin.json.in").read_text()
    (args.output / ".qoder-plugin/plugin.json").write_text(manifest.replace("@VERSION@", version))
    shutil.copytree(
        adapter / "common/hooks",
        args.output / "common/hooks",
        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
    )
    (args.output / "bin").mkdir()
    launcher = args.output / "bin/tokenless"
    shutil.copy2(Path(__file__).with_name("tokenless"), launcher)
    launcher.chmod(0o755)
    checksums = {}
    for target in dict.fromkeys(args.target):
        for name in ("tokenless", "rtk"):
            dest = args.output / "native" / target / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(args.prebuilt_root / target / name, dest)
            dest.chmod(0o755)
            checksums[str(dest.relative_to(args.output))] = hashlib.sha256(
                dest.read_bytes()
            ).hexdigest()
    (args.output / "bundle.json").write_text(
        json.dumps({"version": version, "sha256": checksums}, indent=2) + "\n"
    )
    shutil.copy2(ROOT / "LICENSE", args.output / "LICENSE")
    shutil.copy2(ROOT.parents[1] / "NOTICE", args.output / "NOTICE")
    print(args.output)


if __name__ == "__main__":
    main()
