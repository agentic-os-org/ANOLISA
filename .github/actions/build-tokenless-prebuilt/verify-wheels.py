#!/usr/bin/env python3
"""Validate the complete Tokenless Python wheel release contract."""

from __future__ import annotations

import argparse
import hashlib
import re
import struct
import zipfile
from dataclasses import dataclass
from email.parser import BytesParser
from email.policy import default
from pathlib import Path


RUNTIME_NAME = "anolisa-tokenless"
RUNTIME_FILENAME = "anolisa_tokenless"
AGENTSCOPE_NAME = "anolisa-tokenless-agentscope"
AGENTSCOPE_FILENAME = "anolisa_tokenless_agentscope"
WHEEL_PATTERN = re.compile(
    r"^(anolisa_tokenless(?:_agentscope)?)-([^-]+)-([^-]+)-([^-]+)-([^-]+)\.whl$"
)
PYTHON_VERSION_PATTERN = re.compile(
    r"^(?P<release>[0-9]+\.[0-9]+\.[0-9]+)"
    r"(?:(?:[-_.]?)(?P<pre>alpha|beta|preview|pre|rc|a|b|c)"
    r"(?:[-_.]?(?P<pre_number>[0-9]+))?)?"
    r"(?:(?:-(?P<post_number1>[0-9]+))|"
    r"(?:[-_.]?(?P<post>post|rev|r)(?:[-_.]?(?P<post_number2>[0-9]+))?))?"
    r"(?:[-_.]?(?P<dev>dev)(?:[-_.]?(?P<dev_number>[0-9]+))?)?$",
    re.IGNORECASE,
)


@dataclass(frozen=True)
class NativeTarget:
    os_name: str
    architecture: str
    platform_tags: frozenset[str]
    binary_format: str


TARGETS = {
    ("linux", "x86_64"): NativeTarget(
        "linux",
        "x86_64",
        frozenset({"manylinux_2_17_x86_64", "manylinux2014_x86_64"}),
        "elf",
    ),
    ("linux", "aarch64"): NativeTarget(
        "linux",
        "aarch64",
        frozenset({"manylinux_2_17_aarch64", "manylinux2014_aarch64"}),
        "elf",
    ),
    ("macos", "aarch64"): NativeTarget(
        "macos", "aarch64", frozenset({"macosx_11_0_arm64"}), "macho"
    ),
}


def fail(message: str) -> None:
    raise ValueError(message)


def normalize_name(value: str) -> str:
    return re.sub(r"[-_.]+", "-", value).lower()


def normalize_python_version(value: str) -> str:
    match = PYTHON_VERSION_PATTERN.fullmatch(value)
    if not match:
        fail(f"version is not supported by the wheel verifier: {value}")
    normalized = match.group("release")
    if phase := match.group("pre"):
        phase = {
            "alpha": "a",
            "beta": "b",
            "c": "rc",
            "pre": "rc",
            "preview": "rc",
        }.get(phase.lower(), phase.lower())
        normalized += f"{phase}{int(match.group('pre_number') or 0)}"
    post_number = match.group("post_number1") or match.group("post_number2")
    if match.group("post") or post_number:
        normalized += f".post{int(post_number or 0)}"
    if match.group("dev"):
        normalized += f".dev{int(match.group('dev_number') or 0)}"
    return normalized


def parse_filename(path: Path) -> tuple[str, str, str, str, frozenset[str]]:
    match = WHEEL_PATTERN.fullmatch(path.name)
    if not match:
        fail(f"unexpected wheel filename: {path.name}")
    distribution, version, python_tag, abi_tag, platform = match.groups()
    return distribution, version, python_tag, abi_tag, frozenset(platform.split("."))


def platform_matches(actual: frozenset[str], target: NativeTarget) -> bool:
    if target.os_name == "macos":
        return actual == target.platform_tags
    return bool(actual) and actual.issubset(target.platform_tags)


def identify_native_target(platform_tags: frozenset[str]) -> NativeTarget:
    matches = [target for target in TARGETS.values() if platform_matches(platform_tags, target)]
    if len(matches) != 1:
        fail(f"unsupported or ambiguous native platform tags: {sorted(platform_tags)}")
    return matches[0]


def single_member(archive: zipfile.ZipFile, suffix: str) -> str:
    matches = [name for name in archive.namelist() if name.endswith(suffix)]
    if len(matches) != 1:
        fail(f"wheel must contain exactly one {suffix}, found {len(matches)}")
    return matches[0]


def wheel_headers(archive: zipfile.ZipFile) -> tuple[object, object]:
    metadata_path = single_member(archive, ".dist-info/METADATA")
    wheel_path = single_member(archive, ".dist-info/WHEEL")
    parser = BytesParser(policy=default)
    metadata = parser.parsebytes(archive.read(metadata_path))
    wheel = parser.parsebytes(archive.read(wheel_path))
    return metadata, wheel


def verify_wheel_tags(
    headers: object, python_tag: str, abi_tag: str, platforms: frozenset[str]
) -> None:
    tags = headers.get_all("Tag", [])
    if not tags:
        fail("WHEEL metadata has no Tag header")
    metadata_platforms: set[str] = set()
    for tag in tags:
        parts = tag.split("-")
        if len(parts) != 3:
            fail(f"invalid WHEEL Tag header: {tag}")
        tag_platforms = frozenset(parts[2].split("."))
        if parts[:2] != [python_tag, abi_tag] or not tag_platforms.issubset(platforms):
            fail(f"WHEEL Tag {tag} does not match the wheel filename")
        metadata_platforms.update(tag_platforms)
    if metadata_platforms != set(platforms):
        fail("WHEEL Tag headers do not cover every filename platform tag")


def verify_elf(payload: bytes, architecture: str, member: str) -> None:
    if len(payload) < 20 or payload[:4] != b"\x7fELF" or payload[4] != 2:
        fail(f"{member} is not a 64-bit ELF binary")
    byte_order = {1: "<", 2: ">"}.get(payload[5])
    if byte_order is None:
        fail(f"{member} has an invalid ELF byte order")
    machine = struct.unpack_from(f"{byte_order}H", payload, 18)[0]
    expected = {"x86_64": 62, "aarch64": 183}[architecture]
    if machine != expected:
        fail(f"{member} has ELF machine {machine}, expected {expected}")


def verify_macho(payload: bytes, member: str) -> None:
    if len(payload) < 8 or payload[:4] != b"\xcf\xfa\xed\xfe":
        fail(f"{member} is not a thin 64-bit little-endian Mach-O binary")
    cpu_type = struct.unpack_from("<I", payload, 4)[0]
    if cpu_type != 0x0100000C:
        fail(f"{member} has Mach-O CPU type {cpu_type:#x}, expected arm64")


def verify_binary(payload: bytes, target: NativeTarget, member: str) -> None:
    if target.binary_format == "elf":
        verify_elf(payload, target.architecture, member)
    else:
        verify_macho(payload, member)


def verify_runtime(path: Path, version: str) -> NativeTarget:
    distribution, filename_version, python_tag, abi_tag, platforms = parse_filename(path)
    if distribution != RUNTIME_FILENAME:
        fail(f"{path.name} is not a {RUNTIME_NAME} wheel")
    if filename_version != version:
        fail(f"{path.name} has version {filename_version}, expected {version}")
    if (python_tag, abi_tag) != ("cp311", "abi3"):
        fail(f"{path.name} must use cp311-abi3")
    target = identify_native_target(platforms)

    with zipfile.ZipFile(path) as archive:
        corrupt = archive.testzip()
        if corrupt is not None:
            fail(f"{path.name} has a corrupt ZIP member: {corrupt}")
        metadata, wheel = wheel_headers(archive)
        if normalize_name(metadata.get("Name", "")) != RUNTIME_NAME:
            fail(f"{path.name} has the wrong METADATA Name")
        if metadata.get("Version") != version:
            fail(f"{path.name} has the wrong METADATA Version")
        verify_wheel_tags(wheel, python_tag, abi_tag, platforms)

        native_members = [
            name
            for name in archive.namelist()
            if name.startswith("anolisa_tokenless/_native") and name.endswith(".so")
        ]
        if len(native_members) != 1:
            fail(f"{path.name} must contain exactly one native extension")
        rtk_members = [
            name for name in archive.namelist() if name == "anolisa_tokenless/_bin/rtk"
        ]
        if len(rtk_members) != 1:
            fail(f"{path.name} must contain exactly one embedded RTK binary")
        rtk_info = archive.getinfo(rtk_members[0])
        if not ((rtk_info.external_attr >> 16) & 0o111):
            fail(f"{path.name} embeds a non-executable RTK binary")
        for member in (native_members[0], rtk_members[0]):
            verify_binary(archive.read(member), target, member)
    return target


def verify_agentscope(path: Path, version: str) -> None:
    distribution, filename_version, python_tag, abi_tag, platforms = parse_filename(path)
    if distribution != AGENTSCOPE_FILENAME:
        fail(f"{path.name} is not an {AGENTSCOPE_NAME} wheel")
    if filename_version != version or (python_tag, abi_tag, platforms) != (
        "py3",
        "none",
        frozenset({"any"}),
    ):
        fail(f"{path.name} must use version {version} and py3-none-any")
    with zipfile.ZipFile(path) as archive:
        corrupt = archive.testzip()
        if corrupt is not None:
            fail(f"{path.name} has a corrupt ZIP member: {corrupt}")
        metadata, wheel = wheel_headers(archive)
        if normalize_name(metadata.get("Name", "")) != AGENTSCOPE_NAME:
            fail(f"{path.name} has the wrong METADATA Name")
        if metadata.get("Version") != version:
            fail(f"{path.name} has the wrong METADATA Version")
        verify_wheel_tags(wheel, python_tag, abi_tag, platforms)
        dependencies = metadata.get_all("Requires-Dist", [])
        expected = f"{RUNTIME_NAME}=={version}"
        runtime_dependencies = [
            dependency
            for dependency in dependencies
            if normalize_name(re.split(r"[ (<>=!~;]", dependency, maxsplit=1)[0])
            == RUNTIME_NAME
        ]
        requirement = None
        if len(runtime_dependencies) == 1:
            requirement = re.fullmatch(
                r"\s*([A-Za-z0-9_.-]+)\s*==\s*([^\s;]+)\s*", runtime_dependencies[0]
            )
        if (
            requirement is None
            or normalize_name(requirement.group(1)) != RUNTIME_NAME
            or normalize_python_version(requirement.group(2)) != version
        ):
            fail(f"{path.name} must require exactly {expected}")
        native_suffixes = (".so", ".dylib", ".dll", ".exe")
        if any(name.endswith(native_suffixes) for name in archive.namelist()):
            fail(f"{path.name} must be a platform-independent wheel")


def expected_targets(
    layout: str, os_name: str | None, architecture: str | None
) -> set[tuple[str, str]]:
    if layout == "aggregate":
        if os_name is not None or architecture is not None:
            fail("--os and --arch are invalid for aggregate layout")
        return set(TARGETS)
    if os_name is None or architecture is None:
        fail("--os and --arch are required for flat layout")
    target = (os_name, architecture)
    if target not in TARGETS:
        fail(f"unsupported Tokenless wheel target: {os_name}/{architecture}")
    return {target}


def checksum_text(wheels: list[Path]) -> str:
    lines = []
    for wheel in sorted(wheels, key=lambda item: item.name):
        digest = hashlib.sha256(wheel.read_bytes()).hexdigest()
        lines.append(f"{digest}  {wheel.name}\n")
    return "".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--layout", choices=("flat", "aggregate"), required=True)
    parser.add_argument("--os", choices=("linux", "macos"))
    parser.add_argument("--arch", choices=("x86_64", "aarch64"))
    checksum_group = parser.add_mutually_exclusive_group()
    checksum_group.add_argument("--write-checksums", type=Path)
    checksum_group.add_argument("--checksum-file", type=Path)
    args = parser.parse_args()

    try:
        python_version = normalize_python_version(args.version)
        if not args.directory.is_dir():
            fail(f"wheel directory does not exist: {args.directory}")
        targets = expected_targets(args.layout, args.os, args.arch)
        wheels = sorted(args.directory.glob("*.whl"))
        expected_count = len(targets) + (1 if ("linux", "x86_64") in targets else 0)
        if len(wheels) != expected_count:
            fail(f"expected {expected_count} wheels, found {len(wheels)}")

        actual_targets: set[tuple[str, str]] = set()
        agentscope_wheels = []
        for wheel in wheels:
            if wheel.name.startswith(f"{AGENTSCOPE_FILENAME}-"):
                agentscope_wheels.append(wheel)
                continue
            target = verify_runtime(wheel, python_version)
            key = (target.os_name, target.architecture)
            if key in actual_targets:
                fail(f"duplicate runtime wheel for {target.os_name}/{target.architecture}")
            actual_targets.add(key)
        if actual_targets != targets:
            fail(f"native wheel targets are {sorted(actual_targets)}, expected {sorted(targets)}")

        requires_agentscope = ("linux", "x86_64") in targets
        if len(agentscope_wheels) != int(requires_agentscope):
            fail(
                f"expected {int(requires_agentscope)} AgentScope wheel, "
                f"found {len(agentscope_wheels)}"
            )
        if agentscope_wheels:
            verify_agentscope(agentscope_wheels[0], python_version)

        expected_checksums = checksum_text(wheels)
        if args.write_checksums:
            if args.layout != "aggregate":
                fail("checksums can only be generated for aggregate layout")
            args.write_checksums.write_text(expected_checksums, encoding="utf-8")
        if args.checksum_file:
            if args.checksum_file.read_text(encoding="utf-8") != expected_checksums:
                fail(f"checksum manifest does not match the wheel set: {args.checksum_file}")
    except (OSError, ValueError, zipfile.BadZipFile) as error:
        parser.error(str(error))

    print(f"Verified {len(wheels)} Tokenless Python wheels for {args.version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
