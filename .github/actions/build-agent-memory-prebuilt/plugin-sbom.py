#!/usr/bin/env python3
"""Inventory the JavaScript actually bundled by esbuild, excluding host peers."""

import hashlib
import json
import sys
from pathlib import Path
from urllib.parse import quote


def inventory(root: Path) -> dict:
    """Build a mergeable CycloneDX fragment from the build manifest and npm lock."""
    package = json.loads((root / "package.json").read_text())
    lock = json.loads((root / "package-lock.json").read_text())["packages"]
    meta = json.loads((root / "dist/build-meta.json").read_text())
    output = meta["outputs"]["dist/index.js"]
    bundle = (root / "dist/index.js").read_bytes()
    if output["bytes"] != len(bundle):
        raise ValueError("esbuild manifest does not describe the bundle")
    packages = {}
    for name, usage in output["inputs"].items():
        if usage["bytesInOutput"] == 0 or "node_modules/" not in name:
            continue
        # The longest lockfile prefix also handles nested/scoped dependencies.
        key = max((key for key in lock if key and name.startswith(key + "/")), key=len)
        info = lock[key]
        dependency = key.rsplit("node_modules/", 1)[1]
        reference = f"pkg:npm/{quote(dependency, safe='/')}@{info['version']}"
        packages[reference] = {
            "type": "library",
            "name": dependency,
            "version": info["version"],
            "bom-ref": reference,
            "purl": reference,
        }
    plugin_ref = f"pkg:npm/{package['name']}@{package['version']}"
    plugin = {
        "type": "library",
        "name": package["name"],
        "version": package["version"],
        "bom-ref": plugin_ref,
        "purl": plugin_ref,
        "hashes": [{"alg": "SHA-256", "content": hashlib.sha256(bundle).hexdigest()}],
    }
    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "version": 1,
        "metadata": {"component": {"components": [plugin]}},
        "components": list(packages.values()),
        "dependencies": [{"ref": plugin_ref, "dependsOn": sorted(packages)}]
        + [{"ref": ref, "dependsOn": []} for ref in sorted(packages)],
    }


if __name__ == "__main__":
    Path(sys.argv[2]).write_text(json.dumps(inventory(Path(sys.argv[1]))))
