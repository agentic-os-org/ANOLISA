#!/usr/bin/env python3
"""Build the small first-party optional component; dependencies stay external."""

import argparse
import gzip
import hashlib
import json
import re
import shutil
import tarfile
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PAYLOAD = Path("lib/anolisa/sec-core-skill-score")
UV = {
    "version": "0.8.17",
    "file": "uv-x86_64-unknown-linux-gnu.tar.gz",
    "url": "https://github.com/astral-sh/uv/releases/download/0.8.17/uv-x86_64-unknown-linux-gnu.tar.gz",
    "sha256": "920cbcaad514cc185634f6f0dcd71df5e8f4ee4456d440a22e0f8c0f142a8203",
}


def file_record(path: Path, name: str | None = None) -> dict:
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"file": name or path.name, "bytes": path.stat().st_size, "sha256": digest}


def write_archive(stage: Path, output: Path, epoch: int) -> None:
    with (
        output.open("wb") as target,
        gzip.GzipFile(
            filename="", mode="wb", fileobj=target, mtime=epoch
        ) as compressed,
    ):
        with tarfile.open(fileobj=compressed, mode="w") as archive:
            for path in sorted(stage.rglob("*")):
                if path.is_symlink():
                    raise ValueError("optional raw payload cannot contain symlinks")
                member = archive.gettarinfo(path, str(path.relative_to(stage)))
                member.uid = member.gid = 0
                member.uname = member.gname = ""
                member.mtime = epoch
                if path.is_file():
                    with path.open("rb") as source:
                        archive.addfile(member, source)
                else:
                    archive.addfile(member)


def package(output: Path, version: str, commit: str, assets: str, epoch: int) -> Path:
    if not re.fullmatch(r"[a-f0-9]{40}", commit):
        raise ValueError("full source commit is required")
    if not re.fullmatch(
        r"\d+\.\d+\.\d+-skillscore\." + commit[:12] + r"(?:\.base\.[a-f0-9]{12})?",
        version,
    ):
        raise ValueError("preview version must bind the exact source commit")
    if not assets.startswith("https://") or "?" in assets or "#" in assets:
        raise ValueError("release asset base must use HTTPS without a query")
    image_metadata = json.loads((output / "images.json").read_text())
    base = image_metadata["work_base_image"]
    if (
        not re.fullmatch(r"sha256:[a-f0-9]{64}", base.get("image_id", ""))
        or base.get("platform") != "linux/amd64"
        or json.loads((output / "tool-inventory.json").read_text()).get(
            "work_base_image"
        )
        != base
    ):
        raise ValueError("work base image identity differs from inventory")
    default_base = (
        "ghcr.io/protectskills/claude-skill-sandbox:lite@sha256:"
        "f6f993df6f9c3f8b608c50548da598f4be862e020f204afa3d653701e95e07bc"
    )
    if (
        base.get("reference") != default_base
        and not version.endswith(".base." + base["image_id"][7:19])
        or base.get("reference") == default_base
        and ".base." in version
    ):
        raise ValueError("custom base version differs from work base image ID")
    images = image_metadata["image_details"]
    expected = {
        "work": f"agent-sec-skill-score:{version}",
        "egress": f"agent-sec-skill-score-egress:{version}",
    }
    if len(images) != 2 or {item["name"] for item in images} != set(expected.values()):
        raise ValueError("images must be tagged with this component release")
    image_records = []
    for role, name in expected.items():
        item = next(item for item in images if item["name"] == name)
        if any(
            not re.fullmatch(r"sha256:[a-f0-9]{64}", item.get(key, ""))
            for key in ("id", "manifest_id")
        ):
            raise ValueError("image is missing its immutable identity")
        if item.get("platform") != "linux/amd64":
            raise ValueError("image has an unsupported platform")
        image_records.append(
            {
                "role": role,
                "name": name,
                "id": item["id"],
                "manifest_id": item["manifest_id"],
                "platform": item["platform"],
            }
        )
    with tempfile.TemporaryDirectory(prefix="asc-optional-package-") as directory:
        stage = Path(directory)
        payload = stage / PAYLOAD
        payload.mkdir(parents=True)
        for path in (ROOT / "packaging/skill-score/optional").iterdir():
            if path.is_file():
                shutil.copyfile(path, payload / path.name)
        (payload / "prepare.sh").chmod(0o755)
        (payload / "uninstall.py").chmod(0o755)
        shutil.copytree(
            ROOT / "skill-score-runtime/src",
            payload / "worker",
            ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
        )
        shared = payload / "shared/agent_sec_cli/skill_score"
        (shared / "evidence").mkdir(parents=True)
        source = ROOT / "agent-sec-cli/src/agent_sec_cli"
        shutil.copyfile(source / "__init__.py", shared.parent / "__init__.py")
        for name in (
            "__init__.py",
            "contracts.py",
            "fixture_formats.py",
            "config.py",
            "io.py",
        ):
            shutil.copyfile(source / "skill_score" / name, shared / name)
        for name in ("__init__.py", "redact.py"):
            shutil.copyfile(
                source / "skill_score/evidence" / name, shared / "evidence" / name
            )
        shutil.copyfile(
            ROOT / "skill-score-runtime/requirements.txt", payload / "requirements.lock"
        )
        shutil.copyfile(ROOT / "LICENSE", payload / "LICENSE")
        python = json.loads(
            (ROOT / "skill-score-runtime/python-provenance.json").read_text()
        )
        python["file"] = "python-3.12.11-20250902.tar.gz"
        archive = file_record(output / "skill-score-images.tar.gz")
        archive["url"] = assets.rstrip("/") + "/" + archive["file"]
        worker = tomllib.loads(
            (ROOT / "skill-score-runtime/pyproject.toml").read_text()
        )
        manifest = {
            "schema": "agent-sec-skill-score/runtime-v1",
            "component": "sec-core-skill-score",
            "version": version,
            "source_commit": commit,
            "platform": "linux/amd64",
            "protocol_version": "agent-sec-skill-score/worker-v1",
            "worker_version": worker["project"]["version"],
            "python": python,
            "uv": UV,
            "harbor_version": "0.22.0",
            "pypi_index": "https://pypi.org/simple",
            "image_archive": archive,
            "images": image_records,
            "work_base_image": base,
            "files": [
                file_record(path, str(path.relative_to(payload)))
                for path in sorted(payload.rglob("*"))
                if path.is_file()
            ],
        }
        manifest_bytes = (json.dumps(manifest, indent=2) + "\n").encode()
        (payload / "runtime-manifest.json").write_bytes(manifest_bytes)
        (output / "runtime-manifest.json").write_bytes(manifest_bytes)
        contract = (
            (ROOT / ".anolisa/skill-score.toml")
            .read_text()
            .replace('version = "1.1.0"', f'version = "{version}"', 1)
        )
        (stage / ".anolisa").mkdir()
        (stage / ".anolisa/component.toml").write_text(contract)
        target = output / f"sec-core-skill-score-{version}-linux-x86_64.tar.gz"
        write_archive(stage, target, epoch)
        return target


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--asset-base", required=True)
    parser.add_argument("--epoch", type=int, required=True)
    args = parser.parse_args()
    print(
        package(
            args.output, args.version, args.source_commit, args.asset_base, args.epoch
        )
    )


if __name__ == "__main__":
    main()
