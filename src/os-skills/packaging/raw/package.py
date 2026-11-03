#!/usr/bin/env python3
"""Build deterministic raw packages from the tracked OS Skills payload."""

import argparse
import datetime
import gzip
import hashlib
import io
import json
import os
import subprocess
import tarfile
import tempfile
import tomllib
import uuid
from pathlib import Path


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def package(source: Path, output: Path, version: str, arch: str, epoch: int) -> Path:
    source_contract = (source / "component.toml").read_text()
    manifest = tomllib.loads(source_contract)
    if manifest["component"]["name"] != "os-skills" or manifest["component"]["version"] != version:
        raise ValueError("component.toml identity does not match the requested release")
    old_adapter = 'trust = "first-party"\ndest = "{datadir}/skills"'
    new_adapter = (
        'trust = "first-party"\nsource = ".anolisa"\n'
        'dest = "{datadir}/adapters/os-skills/{framework}"'
    )
    if (
        source_contract.count('anolisa_min_version = "0.1.15"') != 1
        or source_contract.count('modes = ["system"]') != 1
        or source_contract.count(old_adapter) != 2
    ):
        raise ValueError("component.toml raw contract source has changed")
    contract = source_contract.replace(
        'anolisa_min_version = "0.1.15"', 'anolisa_min_version = "0.1.16"'
    )
    contract = contract.replace('modes = ["system"]', 'modes = ["user", "system"]')
    for framework in ("openclaw", "hermes"):
        contract = contract.replace(old_adapter, new_adapter.replace("{framework}", framework), 1)
    contract += "\n" + (Path(__file__).with_name("layout.toml")).read_text()
    raw_manifest = tomllib.loads(contract)
    platform = raw_manifest["component"]["platform"]
    if "linux" not in platform["os"] or arch not in platform["arch"]:
        raise ValueError(f"raw contract does not support linux/{arch}")
    for adapter in raw_manifest["adapters"]:
        if adapter["source"] != ".anolisa" or adapter["dest"] != (
            f"{{datadir}}/adapters/os-skills/{adapter['framework']}"
        ):
            raise ValueError("raw adapter path does not match the package layout")
    result = subprocess.run(
        ["git", "ls-files", "-z", "--", "."], cwd=source, check=True, capture_output=True
    )
    tracked = sorted(Path(os.fsdecode(name)) for name in result.stdout.split(b"\0") if name)
    skills = {path.parent.name: path.parent for path in tracked if path.name == "SKILL.md"}
    if len(skills) != sum(path.name == "SKILL.md" for path in tracked) or not skills:
        raise ValueError("skill names are missing or duplicated")
    files = {".anolisa/component.toml": contract.encode()}
    for name, directory in skills.items():
        for path in tracked:
            if not path.is_relative_to(directory):
                continue
            source_file = source / path
            if not source_file.is_file() or source_file.is_symlink():
                raise ValueError(f"skill payload must be a regular file: {path}")
            destination = f"share/anolisa/skills/{name}/{path.relative_to(directory)}"
            if destination in files:
                raise ValueError(f"duplicate package path: {destination}")
            files[destination] = source_file.read_bytes()
    archive_name = f"os-skills-{version}-linux-{arch}.tar.gz"
    output.mkdir(parents=True, exist_ok=True)
    archive = output / archive_name
    directories = {"."}
    for name in files:
        directories.update(str(parent) for parent in Path(name).parents)
    with tempfile.TemporaryDirectory(dir=output) as temporary:
        staged = Path(temporary) / archive_name
        with staged.open("wb") as raw, gzip.GzipFile(
            filename="", mode="wb", fileobj=raw, mtime=0
        ) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as tar:
                for name in sorted(directories | files.keys()):
                    entry = tarfile.TarInfo("." if name == "." else f"./{name}")
                    entry.mtime = epoch
                    if name in directories:
                        entry.type, entry.mode = tarfile.DIRTYPE, 0o755
                        tar.addfile(entry)
                    else:
                        data = files[name]
                        entry.mode = 0o755 if name.endswith((".sh", ".py")) else 0o644
                        entry.size = len(data)
                        tar.addfile(entry, io.BytesIO(data))
        staged.replace(archive)
    archive_hash = sha256(archive.read_bytes())
    (output / f"{archive_name}.sha256").write_text(f"{archive_hash}  {archive_name}\n")
    identity = f"os-skills-{version}-linux-{arch}-tar"
    components = [
        {
            "type": "file",
            "name": name,
            "bom-ref": f"{identity}/{name}",
            "hashes": [{"alg": "SHA-256", "content": sha256(data)}],
        }
        for name, data in sorted(files.items())
    ]
    sbom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "serialNumber": f"urn:uuid:{uuid.uuid5(uuid.NAMESPACE_URL, archive_hash)}",
        "version": 1,
        "metadata": {
            "timestamp": datetime.datetime.fromtimestamp(epoch, datetime.UTC).isoformat(),
            "tools": [{"vendor": "ANOLISA", "name": "os-skills-packager", "version": "1"}],
            "component": {
                "type": "application",
                "name": "os-skills",
                "version": version,
                "bom-ref": identity,
                "hashes": [{"alg": "SHA-256", "content": archive_hash}],
                "properties": [
                    {"name": "anolisa:artifact:name", "value": archive_name},
                    {"name": "anolisa:target:os", "value": "linux"},
                    {"name": "anolisa:target:arch", "value": arch},
                ],
            },
        },
        "components": components,
        "dependencies": [{"ref": identity, "dependsOn": [c["bom-ref"] for c in components]}]
        + [{"ref": c["bom-ref"], "dependsOn": []} for c in components],
    }
    sbom_path = output / f"{archive_name}.cdx.json"
    sbom_path.write_text(json.dumps(sbom, sort_keys=True, separators=(",", ":")) + "\n")
    (output / f"{sbom_path.name}.sha256").write_text(
        f"{sha256(sbom_path.read_bytes())}  {sbom_path.name}\n"
    )
    return archive


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--target-arch", choices=("x86_64", "aarch64"), required=True)
    parser.add_argument("--epoch", type=int, required=True)
    args = parser.parse_args()
    if args.epoch < 0:
        parser.error("epoch must be non-negative")
    try:
        print(package(args.source_dir, args.output_dir, args.version, args.target_arch, args.epoch))
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"ERROR: {error}\n")
