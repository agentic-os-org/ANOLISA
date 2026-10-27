#!/usr/bin/env python3
"""Check ZIP contents, reproducibility, and rejection of incompatible binaries."""

import os
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

from test_verify_wheels import binary_payload


def main() -> None:
    script = Path(__file__).with_name("package-windows.py")
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        source, output = root / "source", root / "output"
        binary_dir = source / "target/npm-prebuilt/win32-x64"
        binary_dir.mkdir(parents=True)
        output.mkdir()
        for name in ("tokenless.exe",):
            (binary_dir / name).write_bytes(binary_payload("windows", "x86_64"))
        (source / "LICENSE").write_text("license fixture")
        notices = ("LICENSE", ".anolisa-rtk-commit", ".anolisa-rtk-patches")
        patches = ("rtk-tokenless-stats.patch", "rtk-pytest-error-report.patch", "rtk-library.patch", "future-fix.patch")
        (source / "scripts").mkdir()
        (source / "scripts/setup-rtk.sh").write_text("PATCHES=(" + " ".join(patches) + ")\n")
        for name in notices:
            path = source / "third_party/rtk" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture")
        for name in patches:
            path = source / "third_party/patches" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture")
        for directory, filename in (
            ("tokenless", "anolisa-tokenless-1.2.3.tgz"),
            ("tokenless-win32-x64", "anolisa-tokenless-win32-x64-1.2.3.tgz"),
        ):
            path = source / "npm/dist" / directory / filename
            path.parent.mkdir(parents=True)
            path.write_bytes(b"npm fixture")
        command = [sys.executable, str(script), str(source), str(output), "1.2.3"]
        environment = {**os.environ, "SOURCE_DATE_EPOCH": "1700000000"}
        subprocess.run(command, env=environment, check=True, stdout=subprocess.PIPE)
        archive = output / "tokenless-1.2.3-windows-x86_64.zip"
        original = archive.read_bytes()
        with zipfile.ZipFile(archive) as package:
            assert set(package.namelist()) == {
                "tokenless.exe",
                "LICENSE",
                "third_party/rtk/LICENSE",
                "third_party/rtk/source-commit",
                "third_party/rtk/patches-sha256",
                *(f"third_party/rtk/{patch}" for patch in patches),
                "npm/anolisa-tokenless-1.2.3.tgz",
                "npm/anolisa-tokenless-win32-x64-1.2.3.tgz",
            }
            assert package.read("tokenless.exe") == (binary_dir / "tokenless.exe").read_bytes()
        # Host mtimes and timezone must not change the archive.
        os.utime(binary_dir / "tokenless.exe", (1800000000, 1800000000))
        subprocess.run(
            command, env={**environment, "TZ": "Asia/Shanghai"}, check=True, stdout=subprocess.PIPE
        )
        assert archive.read_bytes() == original
        for payload in (
            binary_payload("windows", "aarch64"),
            binary_payload("linux", "x86_64"),
            b"MZ",
        ):
            (binary_dir / "tokenless.exe").write_bytes(payload)
            result = subprocess.run(command, env=environment, capture_output=True)
            assert result.returncode != 0, "accepted incompatible CLI binary"
        (binary_dir / "tokenless.exe").write_bytes(binary_payload("windows", "x86_64"))
        (source / "npm/dist/tokenless-win32-x64/anolisa-tokenless-win32-x64-1.2.3.tgz").unlink()
        result = subprocess.run(command, env=environment, capture_output=True)
        assert result.returncode != 0, "accepted missing npm platform package"
    print("Windows ZIP packaging tests passed")


if __name__ == "__main__":
    main()
