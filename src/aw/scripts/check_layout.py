"""AW workspace dependencies and source layout policy."""

import os
from pathlib import Path


def structure(metadata: dict, root: Path) -> None:
    """Keep the reviewed crate boundaries and source sizes explicit."""
    allowed = {
        "aw-cli": {
            "aw-adapter", "aw-config", "aw-contracts", "aw-exec", "aw-host", "aw-package",
            "aw-provider", "aw-service", "clap", "clap_complete", "libc", "serde", "serde_json",
            "serde_yaml_ng", "sha2", "tempfile", "reqwest",
        },
        "aw-adapter": {
            "aw-config", "aw-contracts", "aw-exec", "aw-provider",
            "serde", "serde_json", "serde_yaml_ng", "sha2", "thiserror",
            "libc", "semver", "tempfile", "reqwest",
        },
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
            "aw-adapter",
            "aw-contracts",
            "aw-config",
            "aw-core",
            "aw-exec",
            "aw-host",
            "aw-provider",
            "libc",
            "reqwest",
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
    providers = {"aw-provider-sec-core": "agent-sec-core", "aw-provider-tokenless": "tokenless"}
    if members.keys() != allowed.keys() - providers.keys():
        raise ValueError("AW workspace members changed; review the crate boundaries")
    independent = metadata.get("provider_workspaces", [])
    if len(independent) != len(providers):
        raise ValueError("each concrete Provider needs an independent workspace")
    for workspace in independent:
        packages = [p for p in workspace["packages"] if p["id"] in workspace["workspace_members"]]
        if len(packages) != 1 or packages[0]["name"] not in providers:
            raise ValueError("Provider workspace must own exactly one concrete implementation")
        package = packages[0]
        name = package["name"]
        directory = root.parents[1] / "providers" / providers[name]
        if name in members or Path(workspace["workspace_root"]).resolve() != directory.resolve():
            raise ValueError("Provider workspace is duplicated or outside the collaboration directory")
        members[name] = package
    locations = {name: root / "crates" / name for name in allowed}
    locations.update({name: root.parents[1] / "providers" / directory for name, directory in providers.items()})
    binaries = {
        "aw-cli": {"aw"},
        "aw-package": {"aw-build"},
        **{name: {name} for name in providers},
    }
    for name, package in members.items():
        directory = locations[name]
        if Path(package["manifest_path"]).resolve() != (directory / "Cargo.toml").resolve():
            raise ValueError(f"{name}: workspace crate moved outside its reviewed location")
        source_roots = [directory / kind for kind in ("src", "tests", "examples")]
        actual_binaries = {
            target["name"] for target in package["targets"] if "bin" in target.get("kind", [])
        }
        if actual_binaries != binaries.get(name, set()):
            raise ValueError(f"{name}: executable targets changed; aw is the software management command")
        for target in package["targets"]:
            source = Path(target["src_path"]).resolve()
            if not any(source.is_relative_to(path.resolve()) for path in source_roots):
                raise ValueError(f"{name}: Cargo target is outside the audited source layout")
        for dependency in package["dependencies"]:
            if dependency["name"] not in allowed[name]:
                raise ValueError(f"{name}: unreviewed dependency {dependency['name']}")
            if dependency["name"] in locations:
                path = dependency.get("path")
                if (
                    not path
                    or dependency.get("source") is not None
                    or Path(path).resolve() != locations[dependency["name"]].resolve()
                ):
                    raise ValueError(f"{name}: local dependency must use this workspace")
            elif dependency.get("path") is not None or dependency.get("source", "").startswith(
                "git+"
            ):
                raise ValueError(f"{name}: dependency must come from the locked registry")
    source_sizes(root)
    for name in providers:
        source_sizes(locations[name])


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
            limit = 711 if relative == "crates/aw-contracts/src/validation.rs" else 699
            lines = len(file.read_text(encoding="utf-8").splitlines())
            if lines > limit:
                raise ValueError(f"{relative}: {lines} lines exceed the reviewed limit {limit}")
            if lines >= 600:
                print(f"AW layout: {relative}: {lines} lines (ceiling {limit})", flush=True)
