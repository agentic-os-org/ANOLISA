"""Exercise conventional file installs without compiling or registering software."""

import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest


REPOSITORY = Path(__file__).resolve().parents[3]
COMPONENTS = (
    ("src/aw", "aw", None),
    ("providers/agent-sec-core", "aw-provider-sec-core", "sec-core"),
    ("providers/tokenless", "aw-provider-tokenless", "tokenless"),
)


class FileInstallTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="aw-file-install-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name).resolve()
        self.binary = self.directory / "fixture-binary"
        self.binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.binary.chmod(0o700)

    def make(self, component, target="install", environment=None, **variables):
        settings = {"BINARY": str(self.binary), "CARGO": "/missing/cargo"}
        settings.update(variables)
        return subprocess.run(
            [
                "make",
                "--no-print-directory",
                "-C",
                str(REPOSITORY / component),
                target,
                *(f"{name}={value}" for name, value in settings.items()),
            ],
            env=environment,
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )

    def assert_installed(self, component, binary, provider, root, prefix, namespace=""):
        prefix = root / str(prefix).lstrip("/")
        share = prefix / "share" / namespace
        documentation = share / "doc" / binary
        expected = {
            documentation / "LICENSE": REPOSITORY / "LICENSE",
            documentation / "README.md": REPOSITORY / component / "README.md",
            documentation / "README_zh.md": REPOSITORY / component / "README_zh.md",
        }
        if provider:
            installed_binary = prefix / "libexec" / namespace / "aw/providers" / provider / binary
            resources = share / "aw/providers" / provider
            expected[resources / "examples/aw.yaml"] = (
                REPOSITORY / component / "examples/aw.yaml"
            )
            if provider == "sec-core":
                schema = "schemas/health-v1alpha1.schema.json"
                expected[resources / schema] = REPOSITORY / component / schema
        else:
            installed_binary = prefix / "bin/aw"
        expected[installed_binary] = self.binary
        inventory = prefix if root == Path("/") else root
        actual = {path for path in inventory.rglob("*") if path.is_file()}
        self.assertEqual(set(expected), actual)
        for installed, source in expected.items():
            self.assertEqual(source.read_bytes(), installed.read_bytes())
            mode = 0o755 if installed == installed_binary else 0o644
            self.assertEqual(mode, stat.S_IMODE(installed.stat().st_mode))
        self.assertFalse(any(path.name.startswith(".local-") for path in inventory.rglob("*")))

    def test_system_layout_is_staged_without_building_backends(self):
        for index, (component, binary, provider) in enumerate(COMPONENTS):
            with self.subTest(component=component):
                stage = self.directory / f"stage-{index}"
                result = self.make(component, PREFIX="/usr", AWPRJ="", DESTDIR=stage)
                self.assertEqual(0, result.returncode, result.stderr)
                self.assert_installed(component, binary, provider, stage, "/usr")

    def test_namespace_and_quoted_prefix_change_only_the_install_layout(self):
        for index, (component, binary, provider) in enumerate(COMPONENTS):
            with self.subTest(component=component):
                stage = self.directory / f"namespace-{index}"
                prefix = "/opt/team's tools/"
                result = self.make(
                    component, PREFIX=prefix, AWPRJ="anolisa", DESTDIR=stage
                )
                self.assertEqual(0, result.returncode, result.stderr)
                self.assert_installed(component, binary, provider, stage, prefix, "anolisa")

    def test_explicit_directories_override_the_derived_layout(self):
        for index, (component, binary, provider) in enumerate(COMPONENTS):
            with self.subTest(component=component):
                stage = self.directory / f"override-{index}"
                settings = {"DOCDIR": "/custom/docs"}
                if provider:
                    settings.update(LIBEXECDIR="/custom/programs", DATADIR="/custom/data")
                else:
                    settings.update(BINDIR="/custom/programs")
                result = self.make(component, PREFIX="/usr", DESTDIR=stage, **settings)
                self.assertEqual(0, result.returncode, result.stderr)
                self.assertTrue((stage / "custom/programs" / binary).is_file())
                self.assertTrue((stage / "custom/docs/LICENSE").is_file())
                if provider:
                    self.assertTrue((stage / "custom/data/examples/aw.yaml").is_file())
                self.assertFalse((stage / "usr").exists())

    def test_direct_install_has_no_database_or_service_side_effects(self):
        for index, (component, binary, provider) in enumerate(COMPONENTS):
            with self.subTest(component=component):
                prefix = self.directory / f"direct-{index}"
                result = self.make(component, PREFIX=prefix, DESTDIR="", AWPRJ="")
                self.assertEqual(0, result.returncode, result.stderr)
                self.assert_installed(component, binary, provider, Path("/"), prefix)

    def test_core_archive_can_extend_the_same_document_inventory(self):
        notice = self.directory / "NOTICE"
        notice.write_text("Fixture notice\n", encoding="utf-8")
        stage = self.directory / "archive"
        documents = [REPOSITORY / "LICENSE", notice]
        result = self.make(
            "src/aw",
            PREFIX="/usr",
            DESTDIR=stage,
            DOC_FILES=" ".join(str(path) for path in documents),
        )
        self.assertEqual(0, result.returncode, result.stderr)
        actual = {path.relative_to(stage).as_posix() for path in stage.rglob("*") if path.is_file()}
        self.assertEqual({"usr/bin/aw", "usr/share/doc/aw/LICENSE", "usr/share/doc/aw/NOTICE"}, actual)

    def test_missing_prebuilt_binary_fails_before_creating_destinations(self):
        for index, (component, _, _) in enumerate(COMPONENTS):
            with self.subTest(component=component):
                stage = self.directory / f"missing-{index}"
                result = self.make(
                    component,
                    PREFIX="/usr",
                    DESTDIR=stage,
                    BINARY=self.directory / "not-built",
                )
                self.assertNotEqual(0, result.returncode)
                self.assertIn("Missing prebuilt", result.stderr)
                self.assertFalse(stage.exists())

    def test_invalid_layout_fails_before_creating_destinations(self):
        for index, (component, _, _) in enumerate(COMPONENTS):
            for bad in ({"AWPRJ": "../other"}, {"PREFIX": "relative"}, {"DESTDIR": "relative"}):
                with self.subTest(component=component, bad=bad):
                    stage = self.directory / f"invalid-{index}"
                    settings = {"PREFIX": "/usr", "DESTDIR": stage}
                    settings.update(bad)
                    result = self.make(component, **settings)
                    self.assertNotEqual(0, result.returncode)
                    self.assertFalse(stage.exists())

    def test_build_targets_compile_only_their_own_binary(self):
        cargo = self.directory / "cargo-fixture"
        cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$BUILD_ARGS"\n', encoding="utf-8")
        cargo.chmod(0o700)
        for index, (component, binary, _) in enumerate(COMPONENTS):
            with self.subTest(component=component):
                arguments = self.directory / f"arguments-{index}"
                environment = dict(os.environ, BUILD_ARGS=str(arguments))
                target = self.directory / f"target-{index}"
                result = self.make(
                    component,
                    "build",
                    environment=environment,
                    CARGO=cargo,
                    CARGO_TARGET_DIR=target,
                )
                self.assertEqual(0, result.returncode, result.stderr)
                actual = arguments.read_text(encoding="utf-8").splitlines()
                expected = [
                    "build", "--release", "--locked", "--manifest-path",
                    str(REPOSITORY / component / "Cargo.toml"), "--target-dir", str(target),
                ]
                if binary == "aw":
                    expected += ["-p", "aw-cli"]
                self.assertEqual(expected + ["--bin", binary], actual)


if __name__ == "__main__":
    unittest.main()
