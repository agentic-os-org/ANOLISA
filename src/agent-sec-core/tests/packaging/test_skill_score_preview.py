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

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "packaging/skill-score/prepare-preview.py"
SPEC = importlib.util.spec_from_file_location("prepare_preview", SCRIPT)
assert SPEC and SPEC.loader
PREVIEW = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREVIEW)
OPTIONAL_SPEC = importlib.util.spec_from_file_location(
    "package_optional", ROOT / "packaging/skill-score/package-optional.py"
)
assert OPTIONAL_SPEC and OPTIONAL_SPEC.loader
OPTIONAL = importlib.util.module_from_spec(OPTIONAL_SPEC)
OPTIONAL_SPEC.loader.exec_module(OPTIONAL)
SOURCE = "1234567890abcdef" * 2 + "12345678"
VERSION = "1.1.0-skillscore." + SOURCE[:12]


class PreviewTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.components = self.directory / "components-source.toml"
        self.components.write_text(
            'schema_version = 2\n[[components]]\nname = "sec-core"\n'
        )
        images = {
            "work_base_image": {
                "reference": "ghcr.io/protectskills/claude-skill-sandbox:lite@sha256:f6f993df6f9c3f8b608c50548da598f4be862e020f204afa3d653701e95e07bc",
                "image_id": "sha256:" + "c" * 64,
                "platform": "linux/amd64",
            },
            "images": [
                f"agent-sec-skill-score:{VERSION}",
                f"agent-sec-skill-score-egress:{VERSION}",
            ],
            "image_details": [
                {
                    "name": name,
                    "id": "sha256:" + "a" * 64,
                    "manifest_id": "sha256:" + "b" * 64,
                    "platform": "linux/amd64",
                }
                for name in (
                    f"agent-sec-skill-score:{VERSION}",
                    f"agent-sec-skill-score-egress:{VERSION}",
                )
            ],
        }
        (self.directory / "images.json").write_text(json.dumps(images))
        (self.directory / "tool-inventory.json").write_text(
            json.dumps({"work_base_image": images["work_base_image"]})
        )
        for name in (
            "skill-score-images.tar.gz",
            "source.tar.gz",
        ):
            (self.directory / name).write_text("fixture")
        (self.directory / PREVIEW.INSTALLER).write_text("installer fixture")
        OPTIONAL.package(
            self.directory, VERSION, SOURCE, "https://example.org/immutable", 1
        )
        self.package()

    def package(
        self,
        version: str = VERSION,
        embedded_images: bool = False,
        old_launcher: bool = False,
        members: tuple[tarfile.TarInfo, ...] = (),
        component: str = "sec-core",
        filename_version: str = VERSION,
    ) -> None:
        with tarfile.open(
            self.directory / f"{component}-{filename_version}-linux-x86_64.tar.gz",
            "w:gz",
        ) as archive:
            data = (
                f'[component]\nname = "{component}"\nversion = "{version}"\n'.encode()
            )
            member = tarfile.TarInfo("./.anolisa/component.toml")
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
            if embedded_images:
                archive.addfile(
                    tarfile.TarInfo("lib/skill-score-images.tar.gz"), io.BytesIO()
                )
            if old_launcher:
                archive.addfile(
                    tarfile.TarInfo("./bin/agent-sec-skill-score"), io.BytesIO()
                )
            for member in members:
                archive.addfile(member, io.BytesIO())

    def prepare(self) -> dict:
        return PREVIEW.prepare(self.directory, SOURCE, VERSION, self.components)

    def test_index_and_manifest_bind_raw_payload(self) -> None:
        manifest = self.prepare()
        index = tomllib.loads((self.directory / "index-v2.toml").read_text())
        raw = next(
            item
            for item in manifest["artifacts"]
            if item["file"] == manifest["raw_packages"]["sec-core"]
        )
        self.assertEqual(index["entries"][0]["sha256"], raw["sha256"])
        self.assertEqual(index["entries"][0]["version"], VERSION)
        self.assertEqual(len(index["entries"]), 2)
        self.assertEqual(manifest["schema_version"], "agent-sec-skill-score/release-v3")
        self.assertEqual(manifest["installer"]["version"], "0.3.20")
        self.assertNotIn("python_cli", manifest)
        self.assertEqual(manifest["source_commit"], SOURCE)
        self.assertEqual(manifest["raw_repository"], "v1")
        self.assertEqual(manifest["cli"], "agent-sec-cli")
        self.assertEqual(manifest["command_prefix"], ["skillsec", "score"])
        self.assertNotIn(
            "skill-score-payload.tar.gz",
            {item["file"] for item in manifest["artifacts"]},
        )

    def test_rejects_installed_version_drift(self) -> None:
        self.package("1.1.0")
        with self.assertRaisesRegex(ValueError, "identity"):
            self.prepare()

    def test_rejects_images_embedded_in_raw(self) -> None:
        for component in PREVIEW.COMPONENTS:
            for suffix in (".tar", ".tar.gz"):
                with self.subTest(component=component, suffix=suffix):
                    self.package(
                        component=component,
                        members=(tarfile.TarInfo("lib/skill-score-images" + suffix),),
                    )
                    with self.assertRaisesRegex(ValueError, "outside"):
                        PREVIEW.package_contract(
                            self.directory
                            / f"{component}-{VERSION}-linux-x86_64.tar.gz",
                            component,
                            VERSION,
                            b"",
                        )

    def test_allows_only_standard_base_ensurepip_wheels(self) -> None:
        prefix = (
            "lib/anolisa/sec-core/python3.11/runtime/lib/python3.11/ensurepip/_bundled/"
        )
        self.package(
            members=tuple(
                tarfile.TarInfo("./" + prefix + wheel)
                for wheel in (
                    "pip-23.2.1-py3-none-any.whl",
                    "setuptools-65.5.0-py3-none-any.whl",
                )
            )
        )
        self.assertEqual(self.prepare()["version"], VERSION)

    def test_rejects_nonstandard_or_optional_wheels(self) -> None:
        standard = (
            "lib/anolisa/sec-core/python3.11/runtime/lib/python3.11/ensurepip/_bundled/"
            "pip-23.2.1-py3-none-any.whl"
        )
        for component, name in (
            ("sec-core", "lib/harbor-0.22.0-py3-none-any.whl"),
            ("sec-core", standard.replace("pip-23.2.1", "harbor-0.22.0")),
            ("sec-core", standard.replace("pip-23.2.1", "pip-99.0")),
            ("sec-core", standard.replace("python3.11/runtime", "python3.12/runtime")),
            ("sec-core-skill-score", standard),
        ):
            with self.subTest(component=component, name=name):
                self.package(component=component, members=(tarfile.TarInfo(name),))
                with self.assertRaisesRegex(ValueError, "outside"):
                    PREVIEW.package_contract(
                        self.directory / f"{component}-{VERSION}-linux-x86_64.tar.gz",
                        component,
                        VERSION,
                        b"",
                    )

    def test_rejects_nonregular_standard_wheel(self) -> None:
        name = (
            "lib/anolisa/sec-core/python3.11/runtime/lib/python3.11/ensurepip/_bundled/"
            "pip-23.2.1-py3-none-any.whl"
        )
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.DIRTYPE):
            with self.subTest(kind=kind):
                member = tarfile.TarInfo(name)
                member.type = kind
                member.linkname = "other.whl"
                self.package(members=(member,))
                with self.assertRaisesRegex(ValueError, "outside"):
                    self.prepare()

    def test_rejects_optional_runtime_in_base(self) -> None:
        for name in (
            "lib/anolisa/sec-core-skill-score/runtime-manifest.json",
            "lib/anolisa/sec-core-skill-score",
            "lib/anolisa/sec-core/skill-score/runtime/worker.py",
        ):
            with self.subTest(name=name):
                self.package(members=(tarfile.TarInfo(name),))
                with self.assertRaisesRegex(ValueError, "optional scoring runtime"):
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

    def test_custom_base_recipe_survives_optional_and_release_packaging(self) -> None:
        version = VERSION + ".base." + "d" * 12
        images = json.loads((self.directory / "images.json").read_text())
        base = {
            "reference": "sha256:" + "d" * 64,
            "image_id": "sha256:" + "d" * 64,
            "platform": "linux/amd64",
            "recipe": {
                "source_repository": "https://example.org/business",
                "source_commit": "e" * 40,
                "dockerfile_path": "deploy/Dockerfile",
                "dockerfile_sha256": "f" * 64,
                "upstream_base_image": images["work_base_image"],
            },
        }
        images["work_base_image"] = base
        images["images"] = [name.replace(VERSION, version) for name in images["images"]]
        for item in images["image_details"]:
            item["name"] = item["name"].replace(VERSION, version)
        (self.directory / "images.json").write_text(json.dumps(images))
        (self.directory / "tool-inventory.json").write_text(
            json.dumps({"work_base_image": base})
        )
        archive = OPTIONAL.package(
            self.directory, version, SOURCE, "https://example.org/custom", 1
        )
        runtime = json.loads((self.directory / "runtime-manifest.json").read_text())
        self.assertEqual(runtime["work_base_image"], base)
        with tarfile.open(archive) as package:
            with package.extractfile(
                str(OPTIONAL.PAYLOAD / "runtime-manifest.json")
            ) as stream:
                self.assertEqual(json.load(stream)["work_base_image"], base)
        self.package(version, filename_version=version)
        release = PREVIEW.prepare(self.directory, SOURCE, version, self.components)
        self.assertEqual(release["images"]["work_base_image"], base)
        for invalid in (VERSION + ".base." + "e" * 12, VERSION):
            with self.assertRaises(ValueError):
                OPTIONAL.package(
                    self.directory, invalid, SOURCE, "https://example.org/custom", 1
                )

    def test_rejects_base_identity_disagreement_between_metadata_files(self) -> None:
        (self.directory / "tool-inventory.json").write_text("{}")
        with self.assertRaisesRegex(ValueError, "base image identity"):
            OPTIONAL.package(
                self.directory, VERSION, SOURCE, "https://example.org/immutable", 1
            )
        with self.assertRaisesRegex(ValueError, "base image identity"):
            self.prepare()


if __name__ == "__main__":
    unittest.main()
