"""Build a minimal, pinned public source closure from committed Cargo manifests."""

import gzip
import hashlib
import io
import json
import os
import posixpath
import re
import subprocess
import tarfile
import tempfile
import tomllib
from pathlib import Path, PurePosixPath
from typing import Any

SDK = "src/aw/sdk/rust"
ROOTS = (SDK, "src/aw/crates/aw-provider", "src/aw/crates/aw-exec")
PUBLIC = {
    *ROOTS,
    "src/aw/crates/aw-config",
    *(f"src/tokenless/crates/tokenless-{name}" for name in
      ("ccr", "compressors", "protocol", "runtime", "schema", "stats")),
    *(f"src/agent-sec-core/v2/crates/asc-{name}" for name in
      ("daemon-client", "daemon-protocol", "foundation-types", "policy-types",
       "security-events", "observability")),
}
ASSETS = (
    "LICENSE",
    "NOTICE",
    "src/tokenless/LICENSE",
    "src/tokenless/adapters/tokenless/common/hooks/tool_categories.json",
    "src/agent-sec-core/v2/fixtures",
)


def joined(parent: str, child: str) -> str:
    """Reject local dependencies that would read outside the source archive."""
    if PurePosixPath(child).is_absolute():
        raise ValueError(f"absolute source dependency: {child}")
    path = posixpath.normpath(posixpath.join(parent, child))
    if path == ".." or path.startswith("../"):
        raise ValueError(f"source dependency escapes repository: {child}")
    return path


class Snapshot:
    """Read one immutable Git commit, never local uncommitted build inputs."""

    def __init__(self, repo: Path, revision: str) -> None:
        self.repo = repo
        self.commit = self.git("rev-parse", "--verify", "--end-of-options", f"{revision}^{{commit}}").decode().strip()
        if not re.fullmatch(r"[a-f0-9]{40}", self.commit):
            raise ValueError("expected a full Git commit SHA")
        self.files = {}
        for record in self.git("ls-tree", "-rz", self.commit).split(b"\0"):
            if record:
                metadata, name = record.split(b"\t", 1)
                mode, kind, oid = metadata.decode().split()
                self.files[name.decode()] = (mode, kind, oid)
        self.manifests = {}

    def git(self, *args: str) -> bytes:
        return subprocess.run(
            ["git", *args], cwd=self.repo, check=True, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, timeout=60,
        ).stdout

    def read(self, path: str) -> bytes:
        if path not in self.files:
            raise ValueError(f"source input missing from {self.commit}: {path}")
        return self.git("show", f"{self.commit}:{path}")

    def manifest(self, directory: str) -> dict:
        path = joined(directory, "Cargo.toml")
        if path not in self.manifests:
            self.manifests[path] = tomllib.loads(self.read(path).decode())
        return self.manifests[path]

    def workspace(self, directory: str) -> str:
        current = directory
        while True:
            if joined(current, "Cargo.toml") in self.files:
                if "workspace" in self.manifest(current):
                    return current
            parent = posixpath.dirname(current)
            if parent == current or not current:
                raise ValueError(f"no owning workspace for {directory}")
            current = parent


def closure(source: Snapshot) -> tuple[set[str], dict[str, set[str]]]:
    """Follow every local normal, build, target and test dependency."""
    pending = list(ROOTS)
    packages = set()
    workspaces = {}
    while pending:
        directory = pending.pop()
        if directory in packages:
            continue
        if directory not in PUBLIC:
            raise ValueError(f"dependency is not an admitted public library: {directory}")
        manifest = source.manifest(directory)
        if "package" not in manifest:
            raise ValueError(f"source dependency is not a package: {directory}")
        workspace = source.workspace(directory)
        workspaces.setdefault(workspace, set()).add(directory)
        packages.add(directory)
        tables = [manifest, *manifest.get("target", {}).values()]
        for table in tables:
            for kind in ("dependencies", "build-dependencies", "dev-dependencies"):
                for name, dependency in table.get(kind, {}).items():
                    if not isinstance(dependency, dict):
                        continue
                    base = directory
                    if dependency.get("workspace"):
                        dependency = source.manifest(workspace)["workspace"]["dependencies"][name]
                        base = workspace
                    if isinstance(dependency, dict) and "path" in dependency:
                        pending.append(joined(base, dependency["path"]))
    return packages, workspaces


def value(item: Any) -> str:
    """Encode Cargo's TOML values without a packaging-only Python dependency."""
    if isinstance(item, dict):
        return "{ " + ", ".join(f"{json.dumps(k)} = {value(v)}" for k, v in item.items()) + " }"
    if isinstance(item, list):
        return "[" + ", ".join(value(v) for v in item) + "]"
    if isinstance(item, (str, bool, int)):
        return json.dumps(item, ensure_ascii=False)
    raise ValueError(f"unsupported Cargo manifest value: {type(item).__name__}")


def manifest_bytes(document: dict) -> bytes:
    """Preserve semantic manifest data, including inherited dependency settings."""
    lines = [f"{json.dumps(key)} = {value(item)}" for key, item in document.items()
             if not isinstance(item, dict)]
    for key, item in document.items():
        if isinstance(item, dict):
            lines.append("")
            lines.append(f"[{json.dumps(key)}]")
            lines.extend(f"{json.dumps(k)} = {value(v)}" for k, v in item.items())
    lines.append("")
    encoded = "\n".join(lines).encode()
    if tomllib.loads(encoded.decode()) != document:
        raise ValueError("Cargo manifest serialization changed its meaning")
    return encoded


def inputs(source: Snapshot) -> tuple[dict[str, bytes], dict[str, int], list[dict]]:
    """Select tracked source, schemas, tests, licenses and explicit shared assets."""
    packages, workspaces = closure(source)
    selected = {
        path for path in source.files
        if any(path.startswith(directory + "/") for directory in packages)
        or any(path == asset or path.startswith(asset + "/") for asset in ASSETS)
    }
    for asset in ASSETS:
        if not any(path == asset or path.startswith(asset + "/") for path in selected):
            raise ValueError(f"required SDK asset missing: {asset}")
    for workspace in workspaces:
        selected.add(joined(workspace, "Cargo.toml"))
        for name in ("Cargo.lock", "rust-toolchain.toml"):
            path = joined(workspace, name)
            if path in source.files:
                selected.add(path)
    for path in selected:
        mode, kind, _ = source.files[path]
        if kind != "blob" or mode not in {"100644", "100755"}:
            raise ValueError(f"SDK source must be regular files: {path}")
    # Read only admitted files; never archive whole backend workspaces.
    archive = source.git("archive", "--format=tar", source.commit, "--", *sorted(selected))
    content = {}
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        for entry in tar:
            if entry.name in selected:
                if not entry.isfile():
                    raise ValueError(f"non-regular source archive member: {entry.name}")
                stream = tar.extractfile(entry)
                if stream is None:
                    raise ValueError(f"source archive member unreadable: {entry.name}")
                content[entry.name] = stream.read()
    if content.keys() != selected:
        raise ValueError(f"incomplete SDK snapshot: {sorted(selected - content.keys())}")
    versions = []
    for directory in sorted(packages):
        package = source.manifest(directory)["package"]
        version = package["version"]
        if isinstance(version, dict):
            version = source.manifest(source.workspace(directory))["workspace"]["package"]["version"]
        versions.append({"name": package["name"], "version": version, "path": directory})
    for workspace, members in workspaces.items():
        # Retain inheritance, lints and dependency definitions, but never Cargo
        # members that would require obtaining private implementation sources.
        document = tomllib.loads(content[joined(workspace, "Cargo.toml")].decode())
        document["workspace"]["members"] = sorted(posixpath.relpath(p, workspace) for p in members)
        document["workspace"].pop("default-members", None)
        content[joined(workspace, "Cargo.toml")] = manifest_bytes(document)
    # Keep package README links usable when their targets are outside the closure.
    for path in list(content):
        if PurePosixPath(path).name not in {"README.md", "README_zh.md"}:
            continue
        def link(match: re.Match[str]) -> str:
            target = match[1]
            if target.startswith(("https:", "http:", "#", "mailto:", "<")):
                return match[0]
            filename, _, anchor = target.partition("#")
            resolved = joined(posixpath.dirname(path), filename)
            if resolved in content:
                return match[0]
            suffix = "#" + anchor if anchor else ""
            return f"](https://github.com/agentic-os-org/ANOLISA/blob/{source.commit}/{resolved}{suffix})"
        content[path] = re.sub(r"\]\(([^)\s]+)\)", link, content[path].decode()).encode()
    modes = {path: int(source.files[path][0], 8) & 0o777 for path in content}
    return content, modes, versions


def export(source: Snapshot, output: Path) -> dict:
    """Write an immutable source archive with per-file digests and provenance."""
    content, modes, versions = inputs(source)
    sdk_version = next(p["version"] for p in versions if p["name"] == "aw-sdk")
    if not re.fullmatch(r"[0-9A-Za-z.+-]+", sdk_version):
        raise ValueError("invalid SDK version for archive root")
    root = f"aw-sdk-{sdk_version}"
    manifest = {
        "schema_version": 1, "sdk_version": sdk_version,
        "source_commit": source.commit, "packages": versions,
        "files": {path: {
            "sha256": hashlib.sha256(data).hexdigest(),
            "source_blob": source.files[path][2],
        } for path, data in sorted(content.items())},
    }
    content["source.json"] = (json.dumps(manifest, indent=2) + "\n").encode()
    content["README.md"] = (
        "# AW public Rust sources\n\n"
        "This archive contains the application SDK, public Provider libraries and\n"
        "their local source dependencies. Registry dependencies are locked by Cargo.\n\n"
        "```sh\ncd src/aw/sdk/rust\ncargo test --locked\ncargo doc --locked --no-deps\n```\n\n"
        "See `source.json` for component versions, source origin and file digests.\n"
        "The SDK does not install the sec-core daemon or RTK.\n"
    ).encode()
    output.parent.mkdir(parents=True, exist_ok=True)
    if output.exists():
        raise FileExistsError(output)
    with tempfile.NamedTemporaryFile(dir=output.parent, prefix=".sdk-source-") as temporary:
        with gzip.GzipFile(filename="", fileobj=temporary, mode="wb", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as tar:
                for path, data in sorted(content.items()):
                    entry = tarfile.TarInfo(f"{root}/{path}")
                    entry.size = len(data)
                    entry.mode = modes.get(path, 0o644)
                    tar.addfile(entry, io.BytesIO(data))
        temporary.flush()
        os.fsync(temporary.fileno())
        os.link(temporary.name, output)
    return {
        "file": str(output), "sha256": hashlib.sha256(output.read_bytes()).hexdigest(),
        "source_commit": source.commit, "sdk_version": sdk_version,
        "packages": len(versions), "bytes": output.stat().st_size,
    }
