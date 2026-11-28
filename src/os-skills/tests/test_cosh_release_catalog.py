"""Verify the checked-in COSH source extension manifests."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
PACKAGING = ROOT / "packaging"
with patch.object(sys, "path", [str(PACKAGING), *sys.path]):
    from cosh_manifests import sync_manifests


class CoshReleaseCatalogTest(unittest.TestCase):
    def test_source_extension_manifests_match_release_catalog(self) -> None:
        result = subprocess.run(
            [sys.executable, str(PACKAGING / "cosh_manifests.py")],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_catalog_excludes_unreleased_skill_from_both_extension_entries(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            packaging = source / "packaging"
            packaging.mkdir()
            (packaging / "released-skills.txt").write_text("devops/released\n", encoding="utf-8")
            for name in ("released", "unreleased"):
                skill = source / "devops" / name / "SKILL.md"
                skill.parent.mkdir(parents=True)
                skill.write_text(f"---\nname: {name}\n---\n", encoding="utf-8")

            root_manifest = source / "cosh-extension.json"
            category_manifest = source / "devops/cosh-extension.json"
            for path in (root_manifest, category_manifest):
                path.write_text(
                    json.dumps({"name": "fixture", "version": "1.0.0", "skills": "."}),
                    encoding="utf-8",
                )

            self.assertEqual(sync_manifests(source, write=False), [root_manifest, category_manifest])
            self.assertEqual(json.loads(root_manifest.read_text(encoding="utf-8"))["skills"], ".")
            self.assertEqual(sync_manifests(source, write=True), [root_manifest, category_manifest])
            self.assertEqual(sync_manifests(source, write=False), [])
            self.assertEqual(
                json.loads(root_manifest.read_text(encoding="utf-8")),
                {"name": "fixture", "version": "1.0.0", "skills": ["devops/released"]},
            )
            self.assertEqual(
                json.loads(category_manifest.read_text(encoding="utf-8")),
                {"name": "fixture", "version": "1.0.0", "skills": ["released"]},
            )

    def test_sync_validates_all_manifests_before_writing(self) -> None:
        for category_data in (None, b'{"skills":', b"[]"):
            with self.subTest(category_data=category_data), tempfile.TemporaryDirectory() as directory:
                source = Path(directory)
                (source / "packaging").mkdir()
                (source / "packaging" / "released-skills.txt").write_text(
                    "devops/released\n", encoding="utf-8"
                )
                skill = source / "devops" / "released" / "SKILL.md"
                skill.parent.mkdir(parents=True)
                skill.write_text("# Released\n", encoding="utf-8")
                root_manifest = source / "cosh-extension.json"
                original = b'{"name":"fixture","skills":[]}\n'
                root_manifest.write_bytes(original)
                category = source / "devops" / "cosh-extension.json"
                if category_data is not None:
                    category.write_bytes(category_data)

                with self.assertRaises((OSError, ValueError)) as failure:
                    sync_manifests(source, write=True)
                self.assertEqual(root_manifest.read_bytes(), original)
                self.assertIn(str(category), str(failure.exception))
                if category_data is not None:
                    self.assertEqual(category.read_bytes(), category_data)
                else:
                    self.assertFalse(category.exists())


if __name__ == "__main__":
    unittest.main()
