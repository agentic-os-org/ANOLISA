#!/usr/bin/env python3
"""Regression tests for the Tokenless wheel release verifier."""

from __future__ import annotations

import shutil
import struct
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

VERSION = "1.2.3"
ACTION_DIR = Path(__file__).resolve().parent
VERIFIER = ACTION_DIR / "verify-wheels.py"
PLATFORMS = {
    ("windows", "x86_64"): "win_amd64",
    ("linux", "x86_64"): "manylinux_2_17_x86_64.manylinux2014_x86_64",
    ("linux", "aarch64"): "manylinux_2_17_aarch64.manylinux2014_aarch64",
    ("macos", "aarch64"): "macosx_11_0_arm64",
    ("macos", "x86_64"): "macosx_11_0_x86_64",
}


def binary_payload(os_name: str, architecture: str) -> bytes:
    if os_name == "windows":
        payload = bytearray(128)
        payload[:2] = b"MZ"
        struct.pack_into("<I", payload, 0x3C, 64)
        payload[64:68] = b"PE\0\0"
        struct.pack_into("<H", payload, 68, 0x8664 if architecture == "x86_64" else 0xAA64)
        struct.pack_into("<H", payload, 88, 0x20B)
        return bytes(payload)
    if os_name == "macos":
        payload = bytearray(32)
        payload[:4] = b"\xcf\xfa\xed\xfe"
        struct.pack_into("<I", payload, 4, 0x0100000C if architecture == "aarch64" else 0x01000007)
        return bytes(payload)
    payload = bytearray(64)
    payload[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<H", payload, 18, 62 if architecture == "x86_64" else 183)
    return bytes(payload)


def add_member(archive: zipfile.ZipFile, name: str, payload: bytes, mode: int = 0o100644) -> None:
    info = zipfile.ZipInfo(name)
    info.create_system = 3
    info.external_attr = mode << 16
    archive.writestr(info, payload)


def create_runtime(
    directory: Path,
    os_name: str,
    architecture: str,
    *,
    version: str = VERSION,
    platform: str | None = None,
    binary_architecture: str | None = None,
    executables: tuple[str, ...] | None = None,
) -> Path:
    platform = platform or PLATFORMS[(os_name, architecture)]
    path = directory / f"anolisa_tokenless-{version}-cp311-abi3-{platform}.whl"
    dist_info = f"anolisa_tokenless-{version}.dist-info"
    payload = binary_payload(os_name, binary_architecture or architecture)
    with zipfile.ZipFile(path, "w") as archive:
        add_member(
            archive,
            f"{dist_info}/METADATA",
            f"Metadata-Version: 2.4\nName: anolisa-tokenless\nVersion: {version}\n\n".encode(),
        )
        add_member(
            archive,
            f"{dist_info}/WHEEL",
            (
                "Wheel-Version: 1.0\n"
                + "".join(
                    f"Tag: cp311-abi3-{platform_tag}\n"
                    for platform_tag in platform.split(".")
                )
                + "\n"
            ).encode(),
        )
        extension = "_native.pyd" if os_name == "windows" else "_native.abi3.so"
        add_member(archive, f"anolisa_tokenless/{extension}", payload, 0o100755)
        add_member(archive, "anolisa_tokenless/_bin/__init__.py", b"")
        if executables is None:
            executables = ("tokenless.exe",) if os_name == "windows" else ("tokenless",)
        for executable in executables:
            add_member(archive, f"anolisa_tokenless/_bin/{executable}", payload, 0o100755)
    return path


def create_agentscope(
    directory: Path,
    *,
    version: str = VERSION,
    dependency_version: str | None = None,
) -> Path:
    dependency_version = dependency_version or version
    path = directory / f"anolisa_tokenless_agentscope-{version}-py3-none-any.whl"
    dist_info = f"anolisa_tokenless_agentscope-{version}.dist-info"
    with zipfile.ZipFile(path, "w") as archive:
        add_member(
            archive,
            f"{dist_info}/METADATA",
            (
                "Metadata-Version: 2.4\n"
                "Name: anolisa-tokenless-agentscope\n"
                f"Version: {version}\n"
                f"Requires-Dist: anolisa-tokenless=={dependency_version}\n\n"
            ).encode(),
        )
        add_member(
            archive,
            f"{dist_info}/WHEEL",
            b"Wheel-Version: 1.0\nTag: py3-none-any\n\n",
        )
        add_member(archive, "tokenless_agentscope/__init__.py", b"")
    return path


def run_verifier(
    directory: Path,
    *arguments: str,
    version: str = VERSION,
    expect_success: bool = True,
) -> None:
    result = subprocess.run(
        [
            sys.executable,
            str(VERIFIER),
            "--directory",
            str(directory),
            "--version",
            version,
            *arguments,
        ],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    if (result.returncode == 0) != expect_success:
        raise AssertionError(
            f"verifier returned {result.returncode}, expected success={expect_success}:\n"
            f"{result.stdout}"
        )


def populate_aggregate(directory: Path) -> None:
    for os_name, architecture in PLATFORMS:
        create_runtime(directory, os_name, architecture)
    create_agentscope(directory)


def main() -> int:
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        for os_name, architecture in PLATFORMS:
            flat = root / f"flat-{os_name}-{architecture}"
            flat.mkdir()
            create_runtime(flat, os_name, architecture)
            if (os_name, architecture) == ("linux", "x86_64"):
                create_agentscope(flat)
            run_verifier(flat, "--layout", "flat", "--os", os_name, "--arch", architecture)

        aggregate = root / "aggregate"
        aggregate.mkdir()
        populate_aggregate(aggregate)
        manifest = aggregate / "SHA256SUMS-python-wheels.txt"
        run_verifier(
            aggregate,
            "--layout",
            "aggregate",
            "--write-checksums",
            str(manifest),
        )
        run_verifier(
            aggregate,
            "--layout",
            "aggregate",
            "--checksum-file",
            str(manifest),
        )

        prerelease = root / "prerelease"
        prerelease.mkdir()
        python_prerelease = "1.2.4rc1"
        for os_name, architecture in PLATFORMS:
            create_runtime(prerelease, os_name, architecture, version=python_prerelease)
        create_agentscope(
            prerelease,
            version=python_prerelease,
            dependency_version="1.2.4-rc.1",
        )
        run_verifier(
            prerelease,
            "--layout",
            "aggregate",
            version="1.2.4-rc.1",
        )

        wrong_version = root / "wrong-version"
        wrong_version.mkdir()
        populate_aggregate(wrong_version)
        runtime = next(wrong_version.glob("anolisa_tokenless-*manylinux*x86_64.whl"))
        runtime.unlink()
        create_runtime(wrong_version, "linux", "x86_64", version="1.2.4")
        run_verifier(wrong_version, "--layout", "aggregate", expect_success=False)

        missing = root / "missing"
        shutil.copytree(aggregate, missing)
        next(missing.glob("*aarch64*.whl")).unlink()
        run_verifier(missing, "--layout", "aggregate", expect_success=False)

        missing_intel = root / "missing-intel"
        shutil.copytree(aggregate, missing_intel)
        next(missing_intel.glob("*macosx_11_0_x86_64.whl")).unlink()
        run_verifier(missing_intel, "--layout", "aggregate", expect_success=False)

        missing_windows = root / "missing-windows"
        shutil.copytree(aggregate, missing_windows)
        next(missing_windows.glob("*win_amd64.whl")).unlink()
        run_verifier(missing_windows, "--layout", "aggregate", expect_success=False)

        next(missing_intel.glob("*win_amd64.whl")).unlink()
        # Historical trees predate both the shared matrix and the linked RTK library.
        historical_source = root / "historical-source"
        source_manifest = historical_source / "src/tokenless/Cargo.toml"
        source_manifest.parent.mkdir(parents=True)
        source_manifest.write_text('[workspace]\nmembers = ["crates/tokenless-cli"]\n')
        historical = root / "historical"
        historical.mkdir()
        for os_name, architecture in (("linux", "x86_64"), ("linux", "aarch64"), ("macos", "aarch64")):
            create_runtime(historical, os_name, architecture, executables=("rtk",))
        create_agentscope(historical)
        run_verifier(
            historical,
            "--layout",
            "aggregate",
            "--tokenless-source-root",
            str(historical_source),
        )
        for wheels in (missing_intel, aggregate):
            run_verifier(
                wheels,
                "--layout",
                "aggregate",
                "--tokenless-source-root",
                str(historical_source),
                expect_success=False,
            )
        historical_flat = root / "historical-flat"
        historical_flat.mkdir()
        create_runtime(historical_flat, "macos", "aarch64", executables=("rtk",))
        flat_macos = ("--layout", "flat", "--os", "macos", "--arch", "aarch64")
        run_verifier(historical_flat, *flat_macos, expect_success=False)
        run_verifier(
            historical_flat, *flat_macos, "--tokenless-source-root", str(historical_source)
        )
        linked_source = root / "linked-source"
        source_manifest = linked_source / "src/tokenless/Cargo.toml"
        source_manifest.parent.mkdir(parents=True)
        source_manifest.write_text(
            '[workspace.dependencies]\nrtk = { path = "third_party/rtk" }\n'
        )
        run_verifier(
            historical_flat,
            *flat_macos,
            "--tokenless-source-root",
            str(linked_source),
            expect_success=False,
        )
        linked_flat = root / "linked-flat"
        linked_flat.mkdir()
        create_runtime(linked_flat, "macos", "aarch64")
        run_verifier(linked_flat, *flat_macos, "--tokenless-source-root", str(linked_source))

        duplicate = root / "duplicate"
        duplicate.mkdir()
        populate_aggregate(duplicate)
        next(duplicate.glob("*aarch64*.whl")).unlink()
        create_runtime(duplicate, "linux", "x86_64", platform="manylinux2014_x86_64")
        run_verifier(duplicate, "--layout", "aggregate", expect_success=False)

        macos_x86 = root / "macos-x86"
        macos_x86.mkdir()
        populate_aggregate(macos_x86)
        next(macos_x86.glob("*macosx_11_0_x86_64.whl")).unlink()
        create_runtime(
            macos_x86,
            "macos",
            "x86_64",
            platform="macosx_10_9_x86_64",
        )
        run_verifier(macos_x86, "--layout", "aggregate", expect_success=False)

        for architecture, wrong_architecture in (("x86_64", "aarch64"), ("aarch64", "x86_64")):
            bad_macos = root / f"bad-macos-{architecture}"
            bad_macos.mkdir()
            create_runtime(bad_macos, "macos", architecture, binary_architecture=wrong_architecture)
            run_verifier(
                bad_macos,
                "--layout",
                "flat",
                "--os",
                "macos",
                "--arch",
                architecture,
                expect_success=False,
            )

        bad_windows = root / "bad-windows"
        bad_windows.mkdir()
        create_runtime(bad_windows, "windows", "x86_64", binary_architecture="aarch64")
        run_verifier(bad_windows, "--layout", "flat", "--os", "windows", "--arch", "x86_64", expect_success=False)

        for os_name, executables in (
            ("windows", ("tokenless",)),
            ("macos", ("tokenless.exe",)),
            # A binary left over from the separate RTK layout is stale.
            ("macos", ("tokenless", "rtk")),
        ):
            wrong_executables = root / f"wrong-executables-{os_name}-{len(executables)}"
            wrong_executables.mkdir()
            create_runtime(wrong_executables, os_name, "x86_64", executables=executables)
            run_verifier(
                wrong_executables,
                "--layout",
                "flat",
                "--os",
                os_name,
                "--arch",
                "x86_64",
                expect_success=False,
            )

        bad_architecture = root / "bad-architecture"
        bad_architecture.mkdir()
        populate_aggregate(bad_architecture)
        runtime = next(bad_architecture.glob("*manylinux*x86_64.whl"))
        runtime.unlink()
        create_runtime(
            bad_architecture,
            "linux",
            "x86_64",
            binary_architecture="aarch64",
        )
        run_verifier(bad_architecture, "--layout", "aggregate", expect_success=False)

        manifest.write_text("0" * 64 + "  invalid.whl\n", encoding="utf-8")
        run_verifier(
            aggregate,
            "--layout",
            "aggregate",
            "--checksum-file",
            str(manifest),
            expect_success=False,
        )

    print("Tokenless wheel verifier tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
