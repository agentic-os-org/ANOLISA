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
BUNDLE_SPEC = importlib.util.spec_from_file_location(
    "bundle_preview", ROOT / "packaging/skill-score/bundle-preview.py"
)
assert BUNDLE_SPEC and BUNDLE_SPEC.loader
BUNDLE = importlib.util.module_from_spec(BUNDLE_SPEC)
BUNDLE_SPEC.loader.exec_module(BUNDLE)
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
        (self.directory / "source.tar.gz").write_text("fixture")
        (self.directory / PREVIEW.INSTALLER).write_text("installer fixture")
        OPTIONAL.package(self.directory, VERSION, SOURCE, 1)
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

    def test_delivery_keeps_base_independent_and_has_only_four_assets(self) -> None:
        manifest = self.prepare()
        output = self.directory / "bundles"
        paths = BUNDLE.bundle(self.directory, output, 1)
        self.assertEqual(len(paths), 4)
        payloads = {}
        for component in ("sec-core", "skill-score"):
            path = output / f"{component}-bundle-{VERSION}-linux-x86_64.tar.gz"
            self.assertEqual(
                path.with_name(path.name + ".sha256").read_text(),
                BUNDLE.digest(path) + "  " + path.name + "\n",
            )
            with tarfile.open(path) as archive:
                self.assertTrue(all(item.isfile() for item in archive))
                payloads[component] = set(archive.getnames())
        self.assertEqual(
            payloads["sec-core"],
            {
                "v1/" + name
                for name in (
                    manifest["raw_packages"]["sec-core"],
                    manifest["installer"]["file"],
                    "index.toml",
                    "index-v2.toml",
                    "components-v2.toml",
                    "release-manifest.json",
                )
            },
        )
        self.assertIn("v1/runtime-manifest.json", payloads["skill-score"])
        self.assertNotIn("v1/skill-score-images.tar.gz", payloads["skill-score"])
        self.assertFalse(payloads["sec-core"] & payloads["skill-score"])
        self.assertEqual(
            payloads["sec-core"] | payloads["skill-score"],
            {"v1/" + item["file"] for item in manifest["artifacts"]}
            | {"v1/release-manifest.json"},
        )
        runtime = json.loads((self.directory / "runtime-manifest.json").read_text())
        self.assertNotIn("image_archive", runtime)
        self.assertIn("image_build", runtime)
        second = self.directory / "second"
        BUNDLE.bundle(self.directory, second, 1)
        for path in paths:
            self.assertEqual(path.read_bytes(), (second / path.name).read_bytes())

    def test_delivery_rejects_tampering_and_unsafe_members(self) -> None:
        manifest = self.prepare()
        target = self.directory / "bundles"
        (self.directory / "source.tar.gz").write_text("tampered")
        with self.assertRaisesRegex(ValueError, "checksum"):
            BUNDLE.bundle(self.directory, target, 1)
        self.assertFalse(target.exists())
        manifest["artifacts"][0]["file"] = "../outside"
        (self.directory / "release-manifest.json").write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "unsafe"):
            BUNDLE.bundle(self.directory, target, 1)

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
        self.assertEqual(manifest["schema_version"], "agent-sec-skill-score/release-v4")
        self.assertEqual(manifest["installer"]["version"], PREVIEW.INSTALLER_VERSION)
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

    def test_rejects_missing_optional_raw(self) -> None:
        (
            self.directory / f"sec-core-skill-score-{VERSION}-linux-x86_64.tar.gz"
        ).unlink()
        with self.assertRaisesRegex(ValueError, "release asset"):
            self.prepare()

    def test_optional_contains_bound_build_sources_without_image_payload(self) -> None:
        manifest = self.prepare()
        path = self.directory / manifest["raw_packages"]["sec-core-skill-score"]
        with tarfile.open(path) as archive:
            names = set(archive.getnames())
            prefix = "lib/anolisa/sec-core-skill-score/"
            self.assertIn(
                prefix + "image-source/skill-score-runtime/image/build.py", names
            )
            self.assertIn(
                prefix + "image-source/skill-score-runtime/egress/Dockerfile", names
            )
            self.assertIn(
                prefix
                + "image-source/agent-sec-cli/src/agent_sec_cli/code_scanner/__init__.py",
                names,
            )
            self.assertFalse(
                any(name.endswith("skill-score-images.tar.gz") for name in names)
            )
        self.assertLess(path.stat().st_size, 2 * 1024**2)

    def test_rejects_prebuilt_outputs_in_source_release(self) -> None:
        path = self.directory / "runtime-manifest.json"
        runtime = json.loads(path.read_text())
        runtime["images"] = []
        path.write_text(json.dumps(runtime))
        with self.assertRaisesRegex(ValueError, "build inputs"):
            self.prepare()


if __name__ == "__main__":
    unittest.main()
