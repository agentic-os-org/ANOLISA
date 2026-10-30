#!/usr/bin/env python3
"""Check ZIP contents, reproducibility, and rejection of incompatible binaries."""

import os
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

from test_verify_wheels import binary_payload

LINKED_MANIFEST = '[workspace.dependencies]\nrtk = { path = "third_party/rtk" }\n'
HISTORICAL_MANIFEST = '[workspace]\nmembers = ["crates/tokenless-cli"]\n'
NPM_TARBALLS = {
    "npm/anolisa-tokenless-1.2.3.tgz",
    "npm/anolisa-tokenless-win32-x64-1.2.3.tgz",
}


def main() -> None:
    script = Path(__file__).with_name("package-windows.py")
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        source, output = root / "source", root / "output"
        binary_dir = source / "target/npm-prebuilt/win32-x64"
        binary_dir.mkdir(parents=True)
        output.mkdir()
        for name in ("tokenless.exe", "rtk.exe"):
            (binary_dir / name).write_bytes(binary_payload("windows", "x86_64"))
        (source / "LICENSE").write_text("license fixture")
        for directory, filename in (
            ("tokenless", "anolisa-tokenless-1.2.3.tgz"),
            ("tokenless-win32-x64", "anolisa-tokenless-win32-x64-1.2.3.tgz"),
        ):
            path = source / "npm/dist" / directory / filename
            path.parent.mkdir(parents=True)
            path.write_bytes(b"npm fixture")
        command = [sys.executable, str(script), str(source), str(output), "1.2.3"]
        environment = {**os.environ, "SOURCE_DATE_EPOCH": "1700000000"}
        archive = output / "tokenless-1.2.3-windows-x86_64.zip"

        # Trees that link RTK ship one executable, even beside a stale rtk.exe.
        (source / "Cargo.toml").write_text(LINKED_MANIFEST)
        subprocess.run(command, env=environment, check=True, stdout=subprocess.PIPE)
        with zipfile.ZipFile(archive) as package:
            assert set(package.namelist()) == {"tokenless.exe", "LICENSE", *NPM_TARBALLS}
            assert package.read("tokenless.exe") == (binary_dir / "tokenless.exe").read_bytes()

        # Historical trees still ship the separate RTK binary.
        (source / "Cargo.toml").write_text(HISTORICAL_MANIFEST)
        subprocess.run(command, env=environment, check=True, stdout=subprocess.PIPE)
        original = archive.read_bytes()
        with zipfile.ZipFile(archive) as package:
            assert set(package.namelist()) == {
                "tokenless.exe",
                "rtk.exe",
                "LICENSE",
                *NPM_TARBALLS,
            }
            assert package.read("rtk.exe") == (binary_dir / "rtk.exe").read_bytes()
        # Host mtimes and timezone must not change the archive.
        os.utime(binary_dir / "rtk.exe", (1800000000, 1800000000))
        subprocess.run(
            command, env={**environment, "TZ": "Asia/Shanghai"}, check=True, stdout=subprocess.PIPE
        )
        assert archive.read_bytes() == original
        for name in ("tokenless.exe", "rtk.exe"):
            for payload in (
                binary_payload("windows", "aarch64"),
                binary_payload("linux", "x86_64"),
                b"MZ",
            ):
                (binary_dir / name).write_bytes(payload)
                result = subprocess.run(command, env=environment, capture_output=True)
                assert result.returncode != 0, f"accepted incompatible {name}"
            (binary_dir / name).write_bytes(binary_payload("windows", "x86_64"))
        (binary_dir / "rtk.exe").unlink()
        result = subprocess.run(command, env=environment, capture_output=True)
        assert result.returncode != 0, "accepted a historical tree without rtk.exe"
        (source / "Cargo.toml").write_text(LINKED_MANIFEST)
        (source / "npm/dist/tokenless-win32-x64/anolisa-tokenless-win32-x64-1.2.3.tgz").unlink()
        result = subprocess.run(command, env=environment, capture_output=True)
        assert result.returncode != 0, "accepted missing npm platform package"
    print("Windows ZIP packaging tests passed")


if __name__ == "__main__":
    main()
