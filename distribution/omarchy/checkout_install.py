#!/usr/bin/env python3
"""Download or open one exact official release and verify it before extraction."""

import argparse
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

from package import settings
from verify_release import verify


def checksum(path: Path, filename: str) -> str:
    lines = path.read_text(encoding="ascii").splitlines()
    if len(lines) != 1:
        raise ValueError("Checksum file must contain exactly one line")
    text = lines[0]
    match = re.fullmatch(r"([0-9a-f]{64})  ([^/\\\n]+)", text)
    if not match or match.group(2) != filename:
        raise ValueError("Checksum file must identify the exact installer archive")
    return match.group(1)


def download(url: str, destination: Path) -> None:
    print(f"Downloading {destination.name}...", flush=True)
    request = urllib.request.Request(url, headers={"User-Agent": "anolisa-omarchy-installer"})
    with urllib.request.urlopen(request, timeout=60) as source, destination.open("wb") as target:
        if not source.url.startswith("https://"):
            raise ValueError("Download redirected away from HTTPS")
        total = 0
        report_at = 8 * 1024 * 1024
        while data := source.read(1024 * 1024):
            target.write(data)
            total += len(data)
            if total >= report_at:
                print(f"  {total // (1024 * 1024)} MiB downloaded", flush=True)
                report_at += 8 * 1024 * 1024


def install(archive: Path | None, options: list[str]) -> int:
    if os.geteuid() == 0 or platform.system() != "Linux" or platform.machine() != "x86_64":
        raise ValueError("Run as the logged-in desktop user on Omarchy Linux x86_64")
    if not shutil.which("omarchy") or not shutil.which("omarchy-shell"):
        raise ValueError("Open an Omarchy desktop terminal and try again")
    config = settings()
    name = f"anolisa-omarchy-{config['version']}-linux-x86_64.tar.gz"
    base = (
        "https://github.com/agentic-os-org/ANOLISA/releases/download/"
        + config["tag_prefix"]
        + config["version"]
    )
    with tempfile.TemporaryDirectory(prefix="anolisa-omarchy-install-") as directory:
        work = Path(directory)
        candidate = work / name
        sidecar = work / (name + ".sha256")
        if archive:
            print("[1/4] Reading the local installer and its SHA-256 sidecar...", flush=True)
            shutil.copyfile(archive, candidate)
            shutil.copyfile(Path(str(archive) + ".sha256"), sidecar)
        else:
            print("[1/4] Opening the official versioned Release...", flush=True)
            download(base + "/" + name + ".sha256", sidecar)
            print("[2/4] Downloading the prebuilt installer; no compilation is needed.", flush=True)
            download(base + "/" + name, candidate)
        expected = checksum(sidecar, name)
        print("[3/4] Verifying archive contents, platform and version...", flush=True)
        verify(candidate, expected, config["version"])
        # Manual copying also works on Python 3.11.6, before tar extraction filters.
        with tarfile.open(candidate) as package:
            for member in package:
                destination = work / member.name
                if member.isdir():
                    destination.mkdir(parents=True, exist_ok=True)
                else:
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    with package.extractfile(member) as source, destination.open("wb") as target:
                        shutil.copyfileobj(source, target)
                    destination.chmod(member.mode)
        root = work / f"anolisa-omarchy-{config['version']}"
        print("[4/4] Installing ANOLISA Omarchy...", flush=True)
        return subprocess.call(
            [sys.executable, "-I", "-B", str(root / "install.py"), *options], cwd=root
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--archive", type=Path, help="Use a local archive with its adjacent .sha256 file"
    )
    parser.add_argument(
        "--no-enable", action="store_true", help="Install files without enabling the desktop plugin"
    )
    parser.add_argument(
        "--non-interactive", action="store_true", help="Fail rather than ask for sudo input"
    )
    parser.add_argument(
        "--recover",
        action="store_true",
        help="Restore an interrupted service transaction before retrying",
    )
    args = parser.parse_args()
    options = ["--no-enable"] if args.no_enable else []
    if args.non_interactive:
        options.append("--non-interactive")
    if args.recover:
        options.append("--recover")
    try:
        result = install(args.archive, options)
    except (OSError, ValueError, tarfile.TarError) as error:
        parser.exit(
            1,
            f"Installation could not continue: {error}\nCheck the archive, checksum sidecar and Release availability before retrying.\n",
        )
    raise SystemExit(result)


if __name__ == "__main__":
    main()
