"""Release containers need payload members, beyond directory entries."""

import hashlib
import io
import json
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

VERIFIER = Path(__file__).with_name("verify-artifacts.py")


def release_fixture(directory: Path, target_os: str, payload: bytes | None) -> None:
    extension = "zip" if target_os == "windows" else "tar.gz"
    archive = directory / f"tokenless-1.2.3-{target_os}-x86_64.{extension}"
    if target_os == "windows":
        with zipfile.ZipFile(archive, "w") as package:
            package.writestr("bin/", b"")
            if payload is not None:
                package.writestr("bin/fixture", payload)
    else:
        with tarfile.open(archive, "w:gz") as package:
            member = tarfile.TarInfo("bin/")
            member.type = tarfile.DIRTYPE
            package.addfile(member)
            if payload is not None:
                member = tarfile.TarInfo("bin/fixture")
                member.size = len(payload)
                package.addfile(member, io.BytesIO(payload))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    artifact_id = f"tokenless-1.2.3-{target_os}-x86_64-{'zip' if target_os == 'windows' else 'tar'}"
    document = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "metadata": {
            "tools": [{"name": "cargo-sbom", "version": "0.10.0"}],
            "component": {
                "name": "tokenless",
                "version": "1.2.3",
                "bom-ref": artifact_id,
                "hashes": [{"alg": "SHA-256", "content": digest}],
                "properties": [
                    {"name": "anolisa:artifact:name", "value": archive.name},
                    {"name": "anolisa:target:os", "value": target_os},
                    {"name": "anolisa:target:arch", "value": "x86_64"},
                ],
            },
        },
        "components": [{"type": "file", "name": "fixture", "bom-ref": "fixture"}],
        "dependencies": [
            {"ref": artifact_id, "dependsOn": ["fixture"]},
            {"ref": "fixture", "dependsOn": []},
        ],
    }
    sbom = Path(f"{archive}.cdx.json")
    sbom.write_text(json.dumps(document), encoding="utf-8")
    for path in (archive, sbom):
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        Path(f"{path}.sha256").write_text(f"{digest}  {path.name}\n", encoding="utf-8")


class ArchivePayloadTests(unittest.TestCase):
    def verify(self, target_os: str, payload: bytes | None):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            release_fixture(directory, target_os, payload)
            return subprocess.run(
                [
                    sys.executable,
                    "-X",
                    "utf8",
                    str(VERIFIER),
                    "--component",
                    "tokenless",
                    "--version",
                    "1.2.3",
                    "--layout",
                    "flat",
                    "--os",
                    target_os,
                    "--arch",
                    "x86_64",
                    "--directory",
                    str(directory),
                ],
                capture_output=True,
                text=True,
                encoding="utf-8",
                timeout=15,
            )

    def test_directory_only_archives_are_rejected(self):
        for target_os in ("linux", "windows"):
            with self.subTest(target_os=target_os):
                result = self.verify(target_os, None)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("ERROR:", result.stderr)
                self.assertNotIn("Verified", result.stdout)

    def test_payload_archives_are_accepted(self):
        for target_os in ("linux", "windows"):
            with self.subTest(target_os=target_os):
                result = self.verify(target_os, b"fixture\n")
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_empty_file_is_still_a_payload_member(self):
        for target_os in ("linux", "windows"):
            with self.subTest(target_os=target_os):
                result = self.verify(target_os, b"")
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
