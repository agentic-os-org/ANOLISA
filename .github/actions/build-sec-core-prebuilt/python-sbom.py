#!/usr/bin/env python3
"""Inventory the Python runtime shipped inside the sec-core raw archive."""

import importlib.metadata
import json
import sys
import tomllib
from pathlib import Path


runtime = Path(sys.argv[1])
output = Path(sys.argv[2])
provenance = tomllib.loads((runtime / "PROVENANCE.toml").read_text())
python_version = provenance["python_version"]
python_ref = f"pkg:generic/cpython@{python_version}"
python_component = {
    "type": "framework",
    "name": "CPython",
    "version": python_version,
    "bom-ref": python_ref,
    "purl": python_ref,
    "licenses": [{"license": {"id": "Python-2.0"}}],
}

site_packages = runtime / "lib/python3.11/site-packages"
packages = []
for distribution in importlib.metadata.distributions(path=[str(site_packages)]):
    name = distribution.metadata["Name"]
    reference = f"pkg:pypi/{name.lower().replace('_', '-')}@{distribution.version}"
    component = {
        "type": "library",
        "name": name,
        "version": distribution.version,
        "bom-ref": reference,
        "purl": reference,
    }
    license_name = distribution.metadata.get("License")
    if not license_name and "License :: OSI Approved :: MIT License" in (
        distribution.metadata.get_all("Classifier", [])
    ):
        license_name = "MIT"
    if license_name:
        component["licenses"] = [{"license": {"id": license_name}}]
    packages.append(component)
if not packages:
    raise ValueError(f"bundled Python has no package metadata: {site_packages}")
packages.sort(key=lambda component: component["bom-ref"])

document = {
    "bomFormat": "CycloneDX",
    "specVersion": "1.6",
    "version": 1,
    "metadata": {"component": {"components": [python_component]}},
    "components": packages,
    "dependencies": [
        {"ref": python_ref, "dependsOn": [component["bom-ref"] for component in packages]},
        *[{"ref": component["bom-ref"], "dependsOn": []} for component in packages],
    ],
}
output.write_text(json.dumps(document))
