#!/usr/bin/env python3
"""Package explicit, hash-bound agent-memory build outputs without rebuilding."""

import argparse
import gzip
import hashlib
import importlib.util
import io
import json
import os
import struct
import tarfile
import tempfile
import tomllib
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "release", Path(__file__).with_name("verify-release.py")
)
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

adapter_spec = importlib.util.spec_from_file_location(
    "adapter_inventory", Path(__file__).resolve().parents[1] / "stage-adapter.py"
)
adapter_inventory = importlib.util.module_from_spec(adapter_spec)
adapter_spec.loader.exec_module(adapter_inventory)


def package(args: argparse.Namespace) -> Path:
    """Validate input bytes and write a deterministic raw archive."""
    if "AWPRJ" in os.environ and os.environ["AWPRJ"] != "anolisa":
        raise ValueError(
            "raw payload requires AWPRJ=anolisa; "
            "use make install DESTDIR=<temporary-root> for independent layouts"
        )
    root = args.source_dir
    version = release.verify(root)
    binary = args.bin_dir / "agent-memory"
    blob = binary.read_bytes()
    if not os.access(binary, os.X_OK):
        raise ValueError("agent-memory binary is not executable")
    if len(blob) < 64 or blob[:6] != b"\x7fELF\x02\x01":
        raise ValueError("agent-memory is not a 64-bit little-endian ELF")
    if struct.unpack_from("<H", blob, 18)[0] != {"x86_64": 62, "aarch64": 183}[args.target_arch]:
        raise ValueError("ELF architecture does not match target")
    plugin = (args.plugin_dir / "index.js").read_bytes()
    if not plugin:
        raise ValueError("plugin bundle is empty")
    metadata = tomllib.loads(args.metadata.read_text())
    for key, expected in (
        ("version", version),
        ("target_os", "linux"),
        ("target_arch", args.target_arch),
    ):
        if metadata[key] != expected:
            raise ValueError(f"build metadata {key} does not match {expected}")
    if metadata["binaries"]["agent-memory"] != hashlib.sha256(blob).hexdigest():
        raise ValueError("agent-memory hash does not match build metadata")
    if metadata["plugin"]["sha256"] != hashlib.sha256(plugin).hexdigest():
        raise ValueError("plugin hash does not match build metadata")
    contract = (
        (root / "packaging/anolisa/raw/component.toml.in").read_text().replace("@VERSION@", version)
    )
    platform = tomllib.loads(contract)["component"]["platform"]
    if "linux" not in platform["os"] or args.target_arch not in platform["arch"]:
        raise ValueError("raw contract does not support target")
    files = {
        ".anolisa/component.toml": (contract.encode(), 0o644),
        "bin/agent-memory": (blob, 0o755),
        "adapters/agent-memory/openclaw/dist/index.js": (plugin, 0o644),
    }
    sources = {
        "share/anolisa/agent-memory/default.toml": "config/default.toml",
        "share/anolisa/agent-memory/anolisa-memory@.service": "packaging/systemd/anolisa-memory@.service.in",
    }
    for relative, source in adapter_inventory.runtime_files(
        root / "adapters/agent-memory", args.plugin_dir
    ).items():
        # Keep the metadata-verified index bytes even if the build tree changes later.
        if relative == Path("openclaw/dist/index.js"):
            continue
        files[f"adapters/agent-memory/{relative}"] = (
            source.read_bytes(),
            0o755 if source.suffix == ".sh" else 0o644,
        )
    for destination, source in sources.items():
        files[destination] = (
            (root / source).read_bytes(),
            0o755 if source.endswith(".sh") else 0o644,
        )
    mcp = json.loads((root / "config/mcp-server.json").read_text())
    mcp["command"] = "{bindir}/agent-memory"
    files["share/anolisa/mcp-servers/agent-memory.json"] = (
        (json.dumps(mcp, indent=2, ensure_ascii=False) + "\n").encode(),
        0o644,
    )
    directories = {"."}
    for name in files:
        directories.update(str(parent) for parent in Path(name).parents)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    output = args.output_dir / f"agent-memory-{version}-linux-{args.target_arch}.tar.gz"
    # Write atomically so a failed package cannot be mistaken for a usable archive.
    with tempfile.TemporaryDirectory(dir=args.output_dir) as work:
        temporary = Path(work) / output.name
        with temporary.open("wb") as raw, gzip.GzipFile(
            filename="", mode="wb", fileobj=raw, mtime=0
        ) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as tar:
                for name in sorted(directories | files.keys()):
                    entry = tarfile.TarInfo("." if name == "." else f"./{name}")
                    entry.mtime = args.epoch
                    if name in directories:
                        entry.type, entry.mode = tarfile.DIRTYPE, 0o755
                        tar.addfile(entry)
                    else:
                        data, entry.mode = files[name]
                        entry.size = len(data)
                        tar.addfile(entry, io.BytesIO(data))
        temporary.replace(output)
    return output


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--bin-dir", required=True, type=Path)
    parser.add_argument("--plugin-dir", required=True, type=Path)
    parser.add_argument("--metadata", required=True, type=Path)
    parser.add_argument("--target-os", choices=("linux",), default="linux")
    parser.add_argument("--target-arch", choices=("x86_64", "aarch64"), required=True)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--epoch", required=True, type=int)
    args = parser.parse_args()
    try:
        if args.epoch < 0:
            raise ValueError("epoch must be non-negative")
        print(package(args))
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"ERROR: {error}\n")


if __name__ == "__main__":
    main()
