#!/usr/bin/env python3
"""Exercise fork release identity without a Linux build or Docker daemon."""

import importlib.util
import io
import json
import tarfile
import tempfile
import tomllib
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "packaging/skill-score/prepare-preview.py"
SPEC = importlib.util.spec_from_file_location("prepare_preview", SCRIPT)
assert SPEC and SPEC.loader
PREVIEW = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREVIEW)
SOURCE = "1234567890abcdef" * 2 + "12345678"
VERSION = "1.1.0-skillscore." + SOURCE[:12]


class PreviewTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.components = self.directory / "components-source.toml"
        self.components.write_text('schema_version = 2\n[[components]]\nname = "sec-core"\n')
        images = {
            "images": ["agent-sec-skill-score:1", "agent-sec-skill-score-egress:1"],
            "image_details": [
                {"name": name, "id": "sha256:" + "a" * 64}
                for name in ("agent-sec-skill-score:1", "agent-sec-skill-score-egress:1")
            ],
        }
        (self.directory / "images.json").write_text(json.dumps(images))
        for name in ("skill-score-images.tar.gz", "tool-inventory.json", "source.tar.gz"):
            (self.directory / name).write_text("fixture")
        self.package()

    def package(
        self, version: str = VERSION, embedded_images: bool = False, old_launcher: bool = False
    ) -> None:
        with tarfile.open(
            self.directory / f"sec-core-{VERSION}-linux-x86_64.tar.gz", "w:gz"
        ) as archive:
            data = f'[component]\nname = "sec-core"\nversion = "{version}"\n'.encode()
            member = tarfile.TarInfo("./.anolisa/component.toml")
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
            if embedded_images:
                archive.addfile(tarfile.TarInfo("lib/skill-score-images.tar.gz"), io.BytesIO())
            if old_launcher:
                archive.addfile(tarfile.TarInfo("./bin/agent-sec-skill-score"), io.BytesIO())

    def prepare(self) -> dict:
        return PREVIEW.prepare(self.directory, SOURCE, VERSION, self.components)

    def test_index_and_manifest_bind_raw_payload(self) -> None:
        manifest = self.prepare()
        index = tomllib.loads((self.directory / "index-v2.toml").read_text())
        raw = next(
            item for item in manifest["artifacts"] if item["file"] == manifest["raw_package"]
        )
        self.assertEqual(index["entries"][0]["sha256"], raw["sha256"])
        self.assertEqual(index["entries"][0]["version"], VERSION)
        self.assertEqual(manifest["source_commit"], SOURCE)
        self.assertEqual(manifest["raw_repository"], "v1")
        self.assertEqual(manifest["cli"], "agent-sec-cli")
        self.assertEqual(manifest["command_prefix"], ["skillsec", "score"])
        self.assertNotIn(
            "skill-score-payload.tar.gz", {item["file"] for item in manifest["artifacts"]}
        )

    def test_rejects_installed_version_drift(self) -> None:
        self.package("1.1.0")
        with self.assertRaisesRegex(ValueError, "identity"):
            self.prepare()

    def test_rejects_images_embedded_in_raw(self) -> None:
        self.package(embedded_images=True)
        with self.assertRaisesRegex(ValueError, "outside"):
            self.prepare()

    def test_rejects_source_version_mismatch(self) -> None:
        with self.assertRaisesRegex(ValueError, "bind"):
            PREVIEW.prepare(self.directory, "a" * 40, VERSION, self.components)

    def test_rejects_standalone_launcher(self) -> None:
        self.package(old_launcher=True)
        with self.assertRaisesRegex(ValueError, "unified"):
            self.prepare()

    def test_rejects_missing_image(self) -> None:
        (self.directory / "skill-score-images.tar.gz").unlink()
        with self.assertRaisesRegex(ValueError, "release asset"):
            self.prepare()


if __name__ == "__main__":
    unittest.main()
