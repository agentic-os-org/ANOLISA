#!/usr/bin/env python3
"""Validate sec-core's source/artifact names and its exact four release files."""

import gzip
import hashlib
import io
import json
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

COMMON = Path(__file__).resolve().parent.parent / "prebuilt-rust-common"
VERIFIER = COMMON / "verify-artifacts.py"
VERSION = "1.0.1"
ARCHIVE = f"sec-core-{VERSION}-linux-x86_64.tar.gz"


def write_fixture(root: Path) -> Path:
    """Create an Actions artifact with a valid archive-bound CycloneDX document."""
    directory = root / f"agent-sec-core-prebuilt-{VERSION}-linux-x86_64"
    directory.mkdir()
    archive = directory / ARCHIVE
    with tarfile.open(archive, "w:gz") as package:
        data = b"fixture\n"
        entry = tarfile.TarInfo("bin/agent-sec-cli")
        entry.size = len(data)
        package.addfile(entry, io.BytesIO(data))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    Path(f"{archive}.sha256").write_text(f"{digest}  {archive.name}\n")
    artifact_id = f"sec-core-{VERSION}-linux-x86_64-tar"
    cli_ref = f"pkg:cargo/asc-cli@{VERSION}"
    document = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "metadata": {
            "tools": [{"name": "cargo-sbom", "version": "0.10.0"}],
            "component": {
                "name": "sec-core",
                "version": VERSION,
                "bom-ref": artifact_id,
                "hashes": [{"alg": "SHA-256", "content": digest}],
                "components": [
                    {
                        "type": "application",
                        "name": "asc-cli",
                        "version": VERSION,
                        "bom-ref": cli_ref,
                    }
                ],
                "properties": [
                    {"name": "anolisa:artifact:name", "value": archive.name},
                    {"name": "anolisa:target:os", "value": "linux"},
                    {"name": "anolisa:target:arch", "value": "x86_64"},
                ],
            },
        },
        "components": [],
        "dependencies": [
            {"ref": artifact_id, "dependsOn": [cli_ref]},
            {"ref": cli_ref, "dependsOn": []},
        ],
    }
    sbom = Path(f"{archive}.cdx.json")
    sbom.write_text(json.dumps(document))
    sbom_sha = hashlib.sha256(sbom.read_bytes()).hexdigest()
    Path(f"{sbom}.sha256").write_text(f"{sbom_sha}  {sbom.name}\n")
    return directory


def rebind_archive(archive: Path) -> None:
    """Keep checksums and SBOM valid so only archive integrity can reject a fixture."""
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    Path(f"{archive}.sha256").write_text(f"{digest}  {archive.name}\n")
    sbom = Path(f"{archive}.cdx.json")
    document = json.loads(sbom.read_text())
    document["metadata"]["component"]["hashes"][0]["content"] = digest
    sbom.write_text(json.dumps(document))
    digest = hashlib.sha256(sbom.read_bytes()).hexdigest()
    Path(f"{sbom}.sha256").write_text(f"{digest}  {sbom.name}\n")


def verify(root: Path) -> subprocess.CompletedProcess[str]:
    """Run the publisher's verifier against an Actions Artifact download."""
    return subprocess.run(
        [
            sys.executable,
            str(VERIFIER),
            "--directory",
            str(root),
            "--component",
            "agent-sec-core",
            "--version",
            VERSION,
            "--layout",
            "actions",
        ],
        check=False,
        capture_output=True,
        text=True,
    )


class SecCoreArtifactsTest(unittest.TestCase):
    def test_valid_four_files(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_fixture(root)
            self.assertEqual(verify(root).returncode, 0)

    def test_gzip_integrity_is_checked_after_tar_end(self) -> None:
        for corruption in ("crc", "size", "truncated", "trailing", "deflate"):
            with self.subTest(corruption=corruption), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                directory = write_fixture(root)
                archive = directory / ARCHIVE
                data = bytearray(archive.read_bytes())
                if corruption == "crc":
                    data[-8] ^= 1
                elif corruption == "size":
                    data[-4] ^= 1
                elif corruption == "truncated":
                    del data[-8:]
                elif corruption == "deflate":
                    # A second gzip member with the reserved DEFLATE block type.
                    data.extend(gzip.compress(b"")[:10] + b"\x07" + b"\0" * 8)
                else:
                    data.extend(b"invalid gzip trailer")
                archive.write_bytes(data)
                rebind_archive(archive)
                result = verify(root)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("cannot read archive", result.stderr)
                self.assertNotIn("Traceback", result.stderr)

    def test_valid_gzip_padding_and_concatenated_members(self) -> None:
        for suffix in (b"\0" * 32, gzip.compress(b"\0" * 1024)):
            with self.subTest(suffix=suffix), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                directory = write_fixture(root)
                archive = directory / ARCHIVE
                with archive.open("ab") as stream:
                    stream.write(suffix)
                rebind_archive(archive)
                result = verify(root)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_file_is_rejected(self) -> None:
        for filename in (
            ARCHIVE,
            f"{ARCHIVE}.sha256",
            f"{ARCHIVE}.cdx.json",
            f"{ARCHIVE}.cdx.json.sha256",
        ):
            with self.subTest(filename=filename), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                directory = write_fixture(root)
                (directory / filename).unlink()
                self.assertIn("missing=", verify(root).stderr)

    def test_corrupt_archive_and_sbom_are_rejected(self) -> None:
        for filename, expected in (
            (ARCHIVE, "archive checksum mismatch"),
            (f"{ARCHIVE}.cdx.json", "SBOM checksum mismatch"),
        ):
            with self.subTest(filename=filename), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                directory = write_fixture(root)
                with (directory / filename).open("ab") as stream:
                    stream.write(b"tampered")
                self.assertIn(expected, verify(root).stderr)

    def test_workflow_name_cannot_replace_raw_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = write_fixture(root)
            sbom = directory / f"{ARCHIVE}.cdx.json"
            document = json.loads(sbom.read_text())
            document["metadata"]["component"]["name"] = "agent-sec-core"
            sbom.write_text(json.dumps(document))
            digest = hashlib.sha256(sbom.read_bytes()).hexdigest()
            Path(f"{sbom}.sha256").write_text(f"{digest}  {sbom.name}\n")
            self.assertIn("SBOM component identity mismatch", verify(root).stderr)

    def test_archive_root_must_reach_component(self) -> None:
        for missing_root in (True, False):
            with (
                self.subTest(missing_root=missing_root),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = Path(temporary)
                directory = write_fixture(root)
                sbom = directory / f"{ARCHIVE}.cdx.json"
                document = json.loads(sbom.read_text())
                if missing_root:
                    document["dependencies"] = document["dependencies"][1:]
                else:
                    document["dependencies"][0]["dependsOn"] = []
                sbom.write_text(json.dumps(document))
                digest = hashlib.sha256(sbom.read_bytes()).hexdigest()
                Path(f"{sbom}.sha256").write_text(f"{digest}  {sbom.name}\n")
                self.assertIn("archive root is not connected", verify(root).stderr)


if __name__ == "__main__":
    unittest.main()
