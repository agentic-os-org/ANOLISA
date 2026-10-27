#!/usr/bin/env python3
"""Package the portable Windows binaries and npm tarballs with stable ZIP metadata."""

from __future__ import annotations

import importlib.util
import os
import re
import sys
import time
import zipfile
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "verify_wheels", Path(__file__).with_name("verify-wheels.py")
)
verifier = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = verifier
spec.loader.exec_module(verifier)


def main() -> None:
    source, output = map(Path, sys.argv[1:3])
    version = sys.argv[3]
    binaries = source / "target/npm-prebuilt/win32-x64"
    files = {name: binaries / name for name in ("tokenless.exe",)}
    for name, path in files.items():
        verifier.verify_pe(path.read_bytes(), name)
    files["LICENSE"] = source / "LICENSE"
    files["third_party/rtk/LICENSE"] = source / "third_party/rtk/LICENSE"
    files["third_party/rtk/source-commit"] = source / "third_party/rtk/.anolisa-rtk-commit"
    files["third_party/rtk/patches-sha256"] = source / "third_party/rtk/.anolisa-rtk-patches"
    patch_list = re.search(r"^PATCHES=\(([^)]+)\)$", (source / "scripts/setup-rtk.sh").read_text(), re.M)
    if patch_list is None:
        raise ValueError("RTK patch list is missing")
    for patch in patch_list.group(1).split():
        files[f"third_party/rtk/{patch}"] = source / "third_party/patches" / patch
    for package, tarball in (
        ("tokenless", f"anolisa-tokenless-{version}.tgz"),
        ("tokenless-win32-x64", f"anolisa-tokenless-win32-x64-{version}.tgz"),
    ):
        files[f"npm/{tarball}"] = source / "npm/dist" / package / tarball
    # ZIP timestamps start at 1980; commit timestamps supply a stable UTC value.
    timestamp = time.gmtime(max(315532800, int(os.environ["SOURCE_DATE_EPOCH"])))[:6]
    destination = output / f"tokenless-{version}-windows-x86_64.zip"
    with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, path in sorted(files.items()):
            info = zipfile.ZipInfo(name, timestamp)
            info.create_system = 3
            info.external_attr = (0o100755 if name.endswith(".exe") else 0o100644) << 16
            archive.writestr(info, path.read_bytes(), compress_type=zipfile.ZIP_DEFLATED)
    print(destination)


if __name__ == "__main__":
    main()
