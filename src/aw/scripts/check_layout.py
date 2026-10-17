"""AW workspace dependencies and source layout policy."""

import os
from pathlib import Path


def structure(metadata: dict, root: Path) -> None:
    """Keep the reviewed crate boundaries and source sizes explicit."""
    allowed = {
        "aw-package": {
            "serde",
            "serde_json",
            "serde_yaml_ng",
            "sha2",
            "thiserror",
            "libc",
            "flate2",
            "tar",
            "tempfile",
            "aw-config",
            "aw-exec",
            "aw-service",
        },
        "aw-provider-tokenless": {
            "aw-exec",
            "aw-provider",
            "serde",
            "serde_json",
            "thiserror",
            "tempfile",
            "libc",
            "shlex",
            "sha2",
            "aw-provider-sec-core",
        },
        "aw-provider-sec-core": {
            "aw-exec",
            "aw-provider",
            "serde",
            "serde_json",
            "thiserror",
            "libc",
            "jsonschema",
        },
        "aw-contracts": {"jsonschema", "serde", "serde_json", "sha2", "thiserror"},
        "aw-core": {"aw-contracts", "serde_json", "thiserror"},
        "aw-config": {"jsonschema", "serde", "serde_json", "serde_yaml_ng", "thiserror"},
        "aw-provider": {"aw-config", "jsonschema", "serde", "serde_json", "sha2", "thiserror"},
        "aw-exec": {"libc", "thiserror"},
        "aw-host": {"aw-config", "aw-exec", "aw-provider", "serde_json", "sha2", "thiserror"},
        "aw-service": {
            "aw-contracts",
            "aw-config",
            "aw-core",
            "aw-exec",
            "aw-host",
            "aw-provider",
            "libc",
            "reqwest",
            "semver",
            "serde",
            "serde_json",
            "serde_yaml_ng",
            "sha2",
            "thiserror",
            "tempfile",
        },
    }
    members = {
        p["name"]: p for p in metadata["packages"] if p["id"] in metadata["workspace_members"]
    }
    if members.keys() != allowed.keys():
        raise ValueError("AW workspace members changed; review the crate boundaries")
    for name, package in members.items():
        directory = root if name == "aw-contracts" else root / "crates" / name
        if Path(package["manifest_path"]).resolve() != (directory / "Cargo.toml").resolve():
            raise ValueError(f"{name}: workspace crate moved outside its reviewed location")
        source_roots = [directory / kind for kind in ("src", "tests", "examples")]
        for target in package["targets"]:
            source = Path(target["src_path"]).resolve()
            if not any(source.is_relative_to(path.resolve()) for path in source_roots):
                raise ValueError(f"{name}: Cargo target is outside the audited source layout")
        for dependency in package["dependencies"]:
            if dependency["name"] not in allowed[name]:
                raise ValueError(f"{name}: unreviewed dependency {dependency['name']}")
            local = {
                name: root if name == "aw-contracts" else root / "crates" / name for name in allowed
            }
            if dependency["name"] in local:
                path = dependency.get("path")
                if (
                    not path
                    or dependency.get("source") is not None
                    or Path(path).resolve() != local[dependency["name"]].resolve()
                ):
                    raise ValueError(f"{name}: local dependency must use this workspace")
            elif dependency.get("path") is not None or dependency.get("source", "").startswith(
                "git+"
            ):
                raise ValueError(f"{name}: dependency must come from the locked registry")
    source_sizes(root)


def source_sizes(root: Path) -> None:
    """Audit production code and test fixtures equally, including bridge scripts."""
    # Only the designated build/output directory is excluded. New script locations,
    # examples and fixtures are audited without maintaining another path allowlist.
    for directory, children, files in os.walk(root, followlinks=False):
        parent = Path(directory)
        if parent == root and "target" in children:
            children.remove("target")
        for name in [*children, *files]:
            file = parent / name
            if file.is_symlink():
                raise ValueError(f"source symlink is outside the layout audit: {file}")
            if not file.is_file() or file.suffix not in {".rs", ".py", ".js", ".mjs", ".cjs"}:
                continue
            relative = file.relative_to(root).as_posix()
            if file.name == "mod.rs" and not relative.endswith("tests/common/mod.rs"):
                raise ValueError(f"use named Rust modules instead of mod.rs: {relative}")
            # Fixed pre-existing exception; new modules and fixtures share the same ceiling.
            limit = 711 if relative == "src/validation.rs" else 699
            lines = len(file.read_text(encoding="utf-8").splitlines())
            if lines > limit:
                raise ValueError(f"{relative}: {lines} lines exceed the reviewed limit {limit}")
            if lines >= 600:
                print(f"AW layout: {relative}: {lines} lines (ceiling {limit})", flush=True)
