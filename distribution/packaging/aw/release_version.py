"""Require one version across the AW release binaries and SDK packages."""

import json
from pathlib import Path
import tomllib

from sdk_candidate import SDK_PACKAGES


def release_version(repo: Path) -> str:
    versions = {}
    for manifest in [
        *sorted((repo / "src/aw/crates").glob("*/Cargo.toml")),
        repo / "providers/agent-sec-core/Cargo.toml",
        repo / "providers/tokenless/Cargo.toml",
    ]:
        package = tomllib.loads(manifest.read_text())["package"]
        versions[str(manifest.relative_to(repo))] = package["version"]
    for kind, _, directory in SDK_PACKAGES:
        if kind == "npm":
            version = json.loads((repo / directory / "package.json").read_text())["version"]
        else:
            project = tomllib.loads((repo / directory / "pyproject.toml").read_text())["project"]
            version = project["version"]
        versions[directory] = version
    version = versions["src/aw/crates/aw-cli/Cargo.toml"]
    if any(value != version for value in versions.values()):
        raise ValueError(f"inconsistent AW release versions: {versions}")
    return version


if __name__ == "__main__":
    print(release_version(Path(__file__).resolve().parents[3]))
