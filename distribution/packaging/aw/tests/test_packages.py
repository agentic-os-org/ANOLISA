"""Verify ordinary core delivery and backend-independent Provider packages."""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import package
from common import Cancellation, Cancelled, digest


class Fixture(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.repo = Path(temporary.name)
        self.output = self.repo / "output"
        self.version = "0.1.0"
        self.commit = "a" * 40
        self.cancel = Cancellation()

    def file(self, path: str, data: bytes = b"fixture", mode: int = 0o644) -> Path:
        destination = self.repo / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        destination.chmod(mode)
        return destination

    def binary(self, source: str, name: str, arch: str = "x86_64") -> Path:
        header = bytearray(20)
        header[:6] = b"\x7fELF\x02\x01"
        header[18:20] = package.MACHINES[arch].to_bytes(2, "little")
        return self.file(f"{source}/target/release/{name}", bytes(header), 0o755)

    def prepare(self, selection: str) -> None:
        selected = package.components(selection)
        if "core" in selected:
            self.binary("src/aw", "aw")
            self.file(
                "src/aw/packaging/install.mk",
                (Path(__file__).resolve().parents[4] / "src/aw/packaging/install.mk").read_bytes(),
            )
        for name in ("LICENSE", "NOTICE"):
            self.file(name)
        for language in ("en", "zh"):
            self.file(
                f"docs/user-guide/{language}/aw/installation.md",
                b"[guide](configuration.md) [build](../../../../contribute/en/aw/building.md)",
            )
        for role in ("sec-core", "tokenless"):
            if role in selected:
                source = "providers/" + ("agent-sec-core" if role == "sec-core" else role)
                self.binary(source, f"aw-provider-{role}")
                self.file(f"{source}/examples/aw.yaml")
                self.file(f"{source}/schemas/provider.json", b"{}")

    def assemble(self, selection: str) -> list[Path]:
        return package.assemble(
            self.repo,
            self.version,
            self.commit,
            self.output,
            selection,
            "x86_64",
            self.cancel,
        )

    def check_provider(
        self,
        archive: tarfile.TarFile,
        files: dict[str, tarfile.TarInfo],
        prefix: str,
        identity: str,
    ) -> None:
        description = json.load(archive.extractfile(files[prefix + "package.json"]))
        self.assertEqual(description["schema_version"], 1)
        self.assertEqual(description["package_id"], identity)
        self.assertEqual(description["source_commit"], self.commit)
        self.assertEqual(description["version"], self.version)
        self.assertEqual(description["dependencies"], [])
        self.assertEqual(description["provider"], {"protocol": "aw-provider/v1alpha1"})
        executables = []
        for name, metadata in description["files"].items():
            member = files[prefix + "payload/" + name]
            self.assertEqual(member.mode, metadata["mode"])
            self.assertEqual(
                hashlib.sha256(archive.extractfile(member).read()).hexdigest(), metadata["sha256"]
            )
            if metadata["mode"] == 0o755:
                executables.append(Path(name).name)
        self.assertEqual(executables, [identity])


class ArchiveTests(Fixture):
    def test_explicit_selection_emits_only_independent_packages(self) -> None:
        for selection in (*package.COMPONENTS, "core sec-core tokenless"):
            with self.subTest(selection=selection):
                self.output = self.repo / selection.replace(" ", "-") / "output"
                self.prepare(selection)
                archives = self.assemble(selection)
                self.assertEqual(len(archives), len(package.components(selection)))
                checksums = (self.output / "SHA256SUMS").read_text()
                for path in archives:
                    self.assertIn(f"{digest(path, self.cancel)}  {path.name}\n", checksums)
                    identity = path.name.split("-0.1.0", 1)[0]
                    self.assertIn(identity, package.COMPONENTS.values())
                    with tarfile.open(path) as archive:
                        root = path.name.removesuffix(".tar.gz")
                        files = {m.name.removeprefix(root + "/"): m for m in archive if m.isfile()}
                        self.assertNotIn("aw", files)
                        self.assertFalse(any(name.endswith("manifest.json") for name in files))
                        forbidden = {
                            "tokenless",
                            "rtk",
                            "agent-sec-cli",
                            "agent-sec-daemon",
                            "agent-sec-daemon-health",
                        }
                        self.assertFalse(forbidden & {Path(name).name for name in files})
                        templates = {name for name in files if name.startswith("templates/")}
                        if identity == "aw-core":
                            self.assertIn("bin/aw", files)
                            self.assertIn("Makefile", files)
                            self.assertIn("packaging/install.mk", files)
                            self.assertNotIn("package.json", files)
                            self.assertFalse(any(name.startswith("share/aw/") for name in files))
                            receipt = json.load(
                                archive.extractfile(files["share/doc/aw/source.json"])
                            )
                            self.assertEqual(receipt["source_commit"], self.commit)
                            self.assertEqual(templates, set())
                        else:
                            self.assertNotIn("bin/aw", files)
                            self.check_provider(archive, files, "", identity)
                            role = identity.removeprefix("aw-provider-")
                            self.assertEqual(
                                templates,
                                {f"templates/{role}.json", "templates/sec-core-tokenless.json"},
                            )
                self.assertFalse((self.repo / "src/tokenless").exists())
                self.assertFalse((self.repo / "src/agent-sec-core").exists())

    def test_empty_duplicate_or_unknown_selection_is_rejected_before_publication(self) -> None:
        for selection in ("", "all", "core core", "unknown"):
            with self.subTest(selection=selection), self.assertRaisesRegex(
                ValueError, "components"
            ):
                self.assemble(selection)
        self.assertFalse(self.output.exists())

    @unittest.skipUnless(shutil.which("make"), "ordinary installation needs make")
    def test_core_install_honors_prefix_destdir_without_any_aw_database(self) -> None:
        self.prepare("core")
        (archive_path,) = self.assemble("core")
        unpacked = self.repo / "unpacked"
        with tarfile.open(archive_path) as archive:
            archive.extractall(unpacked)
        (root,) = unpacked.iterdir()
        staged = self.repo / "staged"
        subprocess.run(
            [
                "make",
                "-C",
                str(root),
                "install",
                "PREFIX=/opt/aw",
                "AWPRJ=example",
                f"DESTDIR={staged}",
            ],
            check=True,
            capture_output=True,
            timeout=10,
        )
        self.assertEqual((staged / "opt/aw/bin/aw").read_bytes(), (root / "bin/aw").read_bytes())
        self.assertEqual((staged / "opt/aw/bin/aw").stat().st_mode & 0o777, 0o755)
        for name in ("LICENSE", "NOTICE", "README.md", "README_zh.md", "source.json"):
            self.assertTrue((staged / "opt/aw/share/example/doc/aw" / name).is_file())
        self.assertFalse(
            any(path.name in {".local-packages", "installed.json"} for path in self.repo.rglob("*"))
        )

    def test_templates_require_external_backend_paths_and_rtk_digest(self) -> None:
        tokenless = (package.TEMPLATES / "tokenless.json").read_text()
        security = (package.TEMPLATES / "sec-core.json").read_text()
        self.assertIn("${TOKENLESS_CLI}", tokenless)
        self.assertIn("${RTK_BINARY}", tokenless)
        self.assertIn("${RTK_SHA256}", tokenless)
        self.assertIn("${SEC_CORE_CLI}", security)
        self.assertIn("${SOCKET}", security)
        self.assertNotIn("file_hashes", json.loads(tokenless))
        expected = {
            "sec-core": {"PROVIDERS_DIR", "SEC_CORE_CLI", "SOCKET"},
            "tokenless": {"PROVIDERS_DIR", "TOKENLESS_CLI", "RTK_BINARY", "RTK_SHA256", "STATE"},
            "sec-core-tokenless": {
                "PROVIDERS_DIR",
                "SEC_CORE_CLI",
                "SOCKET",
                "TOKENLESS_CLI",
                "RTK_BINARY",
                "RTK_SHA256",
                "STATE",
            },
        }
        for path in package.TEMPLATES.glob("*.json"):
            text = path.read_text()
            self.assertEqual(json.loads(text)["packages"], [])
            self.assertEqual(set(re.findall(r"\$\{([A-Z0-9_]+)\}", text)), expected[path.stem])
            self.assertNotIn("${PREFIX}", text)
            self.assertNotIn("/libexec/aw/providers", text)

    def test_security_templates_scope_fail_closed_scans_to_mapped_native_tools(self) -> None:
        for name in ("sec-core", "sec-core-tokenless"):
            with self.subTest(template=name):
                template = json.loads((package.TEMPLATES / f"{name}.json").read_text())
                config = template["providers"]["security"]["config"]
                self.assertEqual(config["unmapped_tool"], "error")
                before = template["events"]["tool.before"]
                self.assertTrue(before["required"])
                scan = next(step for step in before["steps"] if step["id"] == "scan-code")
                self.assertEqual(
                    scan["match"]["tools"], ["native:qoder:Bash", "native:openclaw:exec"]
                )
                self.assertEqual(set(config["tools"]), {"Bash", "exec"})
                self.assertEqual(scan["on_error"], "block")
                after = template["events"]["tool.after"]
                observe = next(
                    step for step in after["steps"] if step["id"] == "observe-completion"
                )
                self.assertEqual(observe["match"], scan["match"])

    def test_invalid_architecture_or_missing_inputs_publish_nothing(self) -> None:
        self.prepare("core")
        self.binary("src/aw", "aw", "aarch64")
        with self.assertRaisesRegex(ValueError, "ELF"):
            self.assemble("core")
        self.assertEqual(list(self.output.iterdir()), [])
        self.binary("src/aw", "aw")
        (self.repo / "NOTICE").unlink()
        with self.assertRaises(FileNotFoundError):
            self.assemble("core")
        self.assertEqual(list(self.output.iterdir()), [])

    def test_cancellation_during_compression_cleans_stage_and_allows_retry(self) -> None:
        self.prepare("core")
        compress = package._archive

        def interrupted(root: Path, destination: Path, cancel: Cancellation) -> None:
            cancel.request()
            compress(root, destination, cancel)

        with patch.object(package, "_archive", interrupted), self.assertRaises(Cancelled):
            self.assemble("core")
        self.assertEqual(list(self.output.iterdir()), [])
        self.cancel = Cancellation()
        self.assertEqual(len(self.assemble("core")), 1)

    def test_publication_collision_preserves_unknown_file_and_rolls_back_own_links(self) -> None:
        self.output.mkdir()
        first, second = self.file("first", b"first"), self.file("second", b"second")
        unknown = self.file("output/second", b"unknown")
        with self.assertRaises(FileExistsError):
            package.publish([first, second], self.output, self.cancel)
        self.assertEqual(unknown.read_bytes(), b"unknown")
        self.assertFalse((self.output / "first").exists())
        unknown.unlink()
        self.assertEqual(len(package.publish([first, second], self.output, self.cancel)), 2)

    def test_path_injection_and_nonempty_output_are_rejected(self) -> None:
        self.prepare("core")
        for version in ("0.1.0/../../x", "00.1.0", "0.1.0-01"):
            with self.assertRaisesRegex(ValueError, "version"):
                package.assemble(
                    self.repo, version, self.commit, self.output, "core", "x86_64", self.cancel
                )
        self.file("output/unknown")
        with self.assertRaisesRegex(ValueError, "empty"):
            self.assemble("core")


if __name__ == "__main__":
    unittest.main()
