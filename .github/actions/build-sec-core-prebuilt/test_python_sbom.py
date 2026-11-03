#!/usr/bin/env python3
"""Check the published SBOM against versions in a bundled Python payload."""

import email
import hashlib
import io
import json
import runpy
import subprocess
import tarfile
import tempfile
import tomllib
import unittest
from pathlib import Path


ACTION_DIR = Path(__file__).resolve().parent
COMMON = ACTION_DIR.parent / "prebuilt-rust-common"
VERSION = "1.0.1"
ARCHIVE = f"sec-core-{VERSION}-linux-x86_64.tar.gz"
RUNTIME_IN_ARCHIVE = "lib/anolisa/sec-core/python3.11/runtime"


class PythonSbomTest(unittest.TestCase):
    def test_sidecar_versions_match_archive(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            site = runtime / "lib/python3.11/site-packages"
            site.mkdir(parents=True)
            (runtime / "PROVENANCE.toml").write_text('python_version = "3.11.6"\n')
            for name, version in (("pip", "23.2.1"), ("setuptools", "68.2.2")):
                metadata = site / f"{name}-{version}.dist-info/METADATA"
                metadata.parent.mkdir()
                metadata.write_text(f"Name: {name}\nVersion: {version}\nLicense: MIT\n")

            artifacts = root / "artifacts"
            artifacts.mkdir()
            archive = artifacts / ARCHIVE
            with tarfile.open(archive, "w:gz") as package:
                package.add(runtime, arcname=RUNTIME_IN_ARCHIVE)
                entry = tarfile.TarInfo("bin/agent-sec-cli")
                entry.size = len(b"fixture\n")
                package.addfile(entry, io.BytesIO(b"fixture\n"))
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            Path(f"{archive}.sha256").write_text(f"{digest}  {ARCHIVE}\n")

            fragment_path = root / "python.json"
            subprocess.run(
                ["python3", str(ACTION_DIR / "python-sbom.py"), str(runtime), str(fragment_path)],
                check=True,
            )
            cargo_ref = f"pkg:cargo/asc-cli@{VERSION}"
            cargo_document = {
                "bomFormat": "CycloneDX",
                "specVersion": "1.6",
                "metadata": {
                    "tools": [{"name": "cargo-sbom", "version": "0.10.0"}],
                    "component": {"components": [{
                        "type": "application", "name": "asc-cli", "version": VERSION,
                        "bom-ref": cargo_ref,
                    }]},
                },
                "components": [],
                "dependencies": [{"ref": cargo_ref, "dependsOn": []}],
            }
            generator = runpy.run_path(str(COMMON / "generate-sbom.py"))
            merged = generator["merge_sboms"](
                [cargo_document, json.loads(fragment_path.read_text())]
            )
            sidecar = generator["normalize_sbom"](
                merged, component_name="sec-core", version=VERSION,
                target_os="linux", target_arch="x86_64", artifact_name=ARCHIVE,
                artifact_sha256=digest, epoch=0,
            )
            sbom = Path(f"{archive}.cdx.json")
            sbom.write_text(json.dumps(sidecar))
            sbom_sha = hashlib.sha256(sbom.read_bytes()).hexdigest()
            Path(f"{sbom}.sha256").write_text(f"{sbom_sha}  {sbom.name}\n")
            subprocess.run(
                ["python3", str(COMMON / "verify-artifacts.py"), "--directory", str(artifacts),
                 "--component", "sec-core", "--version", VERSION, "--layout", "flat",
                 "--os", "linux", "--arch", "x86_64"],
                check=True,
            )

            with tarfile.open(archive, "r:gz") as package:
                provenance = tomllib.loads(
                    package.extractfile(f"{RUNTIME_IN_ARCHIVE}/PROVENANCE.toml")
                    .read().decode()
                )
                expected = {("CPython", provenance["python_version"])}
                for member in package:
                    if member.name.endswith(".dist-info/METADATA"):
                        metadata = email.message_from_bytes(package.extractfile(member).read())
                        expected.add((metadata["Name"], metadata["Version"]))
            components = (
                sidecar["metadata"]["component"]["components"] + sidecar["components"]
            )
            actual = {
                (component["name"], component["version"])
                for component in components
                if component["name"] == "CPython"
                or component.get("purl", "").startswith("pkg:pypi/")
            }
            self.assertEqual(actual, expected)
            artifact_ref = sidecar["metadata"]["component"]["bom-ref"]
            root_edges = next(edge["dependsOn"] for edge in sidecar["dependencies"]
                              if edge["ref"] == artifact_ref)
            python_ref = f"pkg:generic/cpython@{provenance['python_version']}"
            self.assertIn(python_ref, root_edges)
            python_edges = next(edge["dependsOn"] for edge in sidecar["dependencies"]
                                if edge["ref"] == python_ref)
            self.assertEqual(set(python_edges), {
                component["bom-ref"] for component in sidecar["components"]
                if component.get("purl", "").startswith("pkg:pypi/")
            })


if __name__ == "__main__":
    unittest.main()
