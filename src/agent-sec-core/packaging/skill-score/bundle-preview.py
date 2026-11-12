#!/usr/bin/env python3
"""Publish a lightweight base bundle and an independent optional scoring bundle."""

import argparse
import gzip
import hashlib
import json
import re
import shutil
import tarfile
import tempfile
from pathlib import Path


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def bundle(source: Path, output: Path, epoch: int) -> list[Path]:
    manifest_path = source / "release-manifest.json"
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise ValueError("release manifest must be a regular file")
    manifest = json.loads(manifest_path.read_text())
    version = manifest["version"]
    if manifest[
        "schema_version"
    ] != "agent-sec-skill-score/release-v4" or not re.fullmatch(
        r"[0-9]+\.[0-9]+\.[0-9]+-skillscore\.[a-f0-9]{12}(?:\.base\.[a-f0-9]{12})?",
        version,
    ):
        raise ValueError("unsupported release identity")
    names = set()
    for item in manifest["artifacts"]:
        name = item["file"]
        if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9._-]*", name) or name in names:
            raise ValueError("release contains an unsafe or duplicate artifact name")
        path = source / name
        if (
            path.is_symlink()
            or not path.is_file()
            or not 0 < path.stat().st_size < 2 * 1024**3
            or path.stat().st_size != item["bytes"]
            or digest(path) != item["sha256"]
        ):
            raise ValueError(f"release artifact size/checksum mismatch: {name}")
        names.add(name)
    base = {
        manifest["raw_packages"]["sec-core"],
        manifest["installer"]["file"],
        "index.toml",
        "index-v2.toml",
        "components-v2.toml",
    }
    if not base < names or "release-manifest.json" in names:
        raise ValueError("release is missing base or optional artifacts")
    groups = {
        "sec-core": base | {"release-manifest.json"},
        "skill-score": names - base,
    }
    if output.exists():
        raise ValueError("bundle output must not exist")
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = Path(tempfile.mkdtemp(prefix=".score-bundles-", dir=output.parent))
    try:
        for component, files in groups.items():
            archive = temporary / f"{component}-bundle-{version}-linux-x86_64.tar.gz"
            # Payloads are already compressed. Fast compression avoids spending
            # release time recompressing the image layers at maximum effort.
            with (
                archive.open("wb") as target,
                gzip.GzipFile(
                    filename="", mode="wb", fileobj=target, mtime=epoch, compresslevel=1
                ) as compressed,
                tarfile.open(fileobj=compressed, mode="w") as stream,
            ):
                for name in sorted(files):
                    path = source / name
                    member = tarfile.TarInfo("v1/" + name)
                    member.size = path.stat().st_size
                    member.mode = 0o644
                    member.mtime = epoch
                    with path.open("rb") as content:
                        stream.addfile(member, content)
            archive.with_name(archive.name + ".sha256").write_text(
                f"{digest(archive)}  {archive.name}\n"
            )
        temporary.rename(output)
    except BaseException:
        shutil.rmtree(temporary)
        raise
    return sorted(output.iterdir())


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("release", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--epoch", type=int, required=True)
    args = parser.parse_args()
    for path in bundle(args.release, args.output, args.epoch):
        print(path)


if __name__ == "__main__":
    main()
