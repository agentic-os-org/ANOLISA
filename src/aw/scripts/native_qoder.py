"""Install the inventory's checksummed Qoder CLI without changing the user's CLI."""

import argparse
import hashlib
import json
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import urllib.request


def validate_pin(pin):
    if (
        not re.fullmatch(r"\d+\.\d+\.\d+", pin.get("version", ""))
        or tuple(map(int, pin["version"].split("."))) < (1, 1, 64)
        or set(pin.get("downloads", {})) != {"x86_64", "aarch64"}
    ):
        raise ValueError("Qoder native acceptance requires an exact supported Linux release")
    for arch, suffix in [("x86_64", "x64"), ("aarch64", "arm64")]:
        row = pin["downloads"][arch]
        expected = (
            "https://qoder-ide.oss-accelerate.aliyuncs.com/qodercli/releases/"
            f"{pin['version']}/qodercli-linux-{suffix}.tar.gz"
        )
        if row.get("url") != expected or not re.fullmatch("[0-9a-f]{64}", row.get("sha256", "")):
            raise ValueError("Qoder download must have an exact official URL and checksum")


def install(pin, destination):
    validate_pin(pin)
    row = pin["downloads"][platform.machine()]
    destination.mkdir(mode=0o700)
    archive = destination / "qodercli.tar.gz"
    try:
        with urllib.request.urlopen(row["url"], timeout=30) as response, archive.open(
            "wb"
        ) as stream:
            shutil.copyfileobj(response, stream)
        if hashlib.file_digest(archive.open("rb"), "sha256").hexdigest() != row["sha256"]:
            raise ValueError("Qoder archive checksum mismatch")
        binary = destination / "qodercli"
        with tarfile.open(archive) as package:
            member = package.getmember("./qodercli")
            if not member.isfile():
                raise ValueError("Qoder archive entry must be a regular executable")
            with package.extractfile(member) as source, binary.open("wb") as output:
                shutil.copyfileobj(source, output)
        binary.chmod(0o700)
        version = subprocess.check_output([str(binary), "--version"], text=True, timeout=10).strip()
        if version != pin["version"]:
            raise ValueError("Qoder CLI version does not match its inventory pin")
        return {
            **row,
            "version": version,
            "architecture": platform.machine(),
            "binary_sha256": hashlib.file_digest(binary.open("rb"), "sha256").hexdigest(),
        }
    finally:
        archive.unlink(missing_ok=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inventory", required=True, type=Path)
    parser.add_argument("--destination", required=True, type=Path)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    pin = json.loads(args.inventory.read_text())["agents"]["qoder"]["qoder_cli"]
    args.report.write_text(json.dumps(install(pin, args.destination), indent=2) + "\n")
