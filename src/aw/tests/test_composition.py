"""Independent AgentSight composition and optional user-service contracts."""

import copy
import importlib.util
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

AW = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("aw_compose", AW / "scripts/compose.py")
composition = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(composition)


class CompositionTests(unittest.TestCase):
    def setUp(self):
        (AW / "target").mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="composition-", dir=AW / "target")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.components = [
            {
                "format": 1, "component": name, "version": "0.1.0-preview.1",
                "source_commit": "a" * 40, "os": "linux", "arch": "aarch64",
                "provider_protocol": "aw-provider/v1alpha1",
                "requires": {} if name == "aw-core" else {"aw-core": "0.1.0-preview.1"},
                "files": {},
            }
            for name in sorted(composition.AW_COMPONENTS)
        ]
        self.contract = (
            '[component]\nname="agentsight"\nversion="0.14.1"\n'
            '[component.platform]\nos=["linux"]\narch=["aarch64", "x86_64"]\n'
        ).encode()
        self.sight_files = {".anolisa/component.toml": self.contract}
        elf = bytearray(20)
        elf[:6] = b"\x7fELF\x02\x01"
        elf[18:20] = (183).to_bytes(2, "little")
        for name in ["bin/agentsight", "bin/agentsight-enforcer"]:
            self.sight_files[name] = bytes(elf)
        for name in ["bin/agentsight-start", "share/anolisa/agentsight/agentsight.service",
                     "share/anolisa/agentsight/agentsight-enforcer.service"]:
            self.sight_files[name] = b"official independent payload"
        self.aw = self.root / "aw.tar.gz"
        self.sight = self.root / "sight.tar.gz"
        self.output = self.root / "combined"
        self.write_inputs()

    def archive(self, path, files, extra=None):
        with tarfile.open(path, "w:gz") as archive:
            for name, body in files.items():
                member = tarfile.TarInfo(name)
                member.mode = 0o755
                member.size = len(body)
                archive.addfile(member, io.BytesIO(body))
            if extra is not None:
                archive.addfile(extra)

    def write_inputs(self):
        self.archive(self.aw, {"aw-all-in-one/manifest.json": json.dumps(
            {"format": 1, "components": self.components}).encode()})
        self.archive(self.sight, self.sight_files)

    def compose(self, **overrides):
        args = dict(aw=self.aw, aw_sha256=composition.digest(self.aw), sight=self.sight,
                    sight_sha256=composition.digest(self.sight), sight_version="0.14.1",
                    sight_release="https://github.com/agentic-os-org/ANOLISA/releases/tag/sight/v0.14.1",
                    arch="aarch64", output=self.output)
        args.update(overrides)
        composition.compose(**args)

    def test_independent_version_and_original_archive_bytes(self):
        before = self.sight.read_bytes()
        self.compose()
        record = json.loads((self.output / "composition.json").read_text())
        self.assertEqual(record["aw"]["components"], self.components)
        fixture = json.loads((AW / "packaging/agentsight/composition.example.json").read_text())
        normalized = copy.deepcopy(record)
        normalized["aw"]["sha256"] = fixture["aw"]["sha256"]
        normalized["agentsight"]["sha256"] = fixture["agentsight"]["sha256"]
        self.assertEqual(normalized, fixture)
        self.assertEqual(record["agentsight"]["version"], "0.14.1")
        self.assertEqual(record["agentsight"]["ownership"], "independent")
        self.assertEqual((self.output / "agentsight.tar.gz").read_bytes(), before)
        self.assertEqual((self.output / "aw.tar.gz").read_bytes(), self.aw.read_bytes())
        self.assertFalse(any("agentsight" == item["component"] for item in record["aw"]["components"]))

    def test_rejects_hash_mismatch_without_publishing(self):
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            self.compose(sight_sha256="0" * 64)
        self.assertFalse(self.output.exists())
        self.assertEqual(list(self.root.glob(".aw-compose-*")), [])

    def test_preserves_aw_matching_source_and_version_rule(self):
        for field, value in [("source_commit", "b" * 40), ("version", "0.2.0-preview.1")]:
            with self.subTest(field=field):
                components = copy.deepcopy(self.components)
                self.components[0][field] = value
                self.write_inputs()
                with self.assertRaisesRegex(ValueError, "matching version and source"):
                    self.compose()
                self.assertFalse(self.output.exists())
                self.components = components

    def test_rejects_incomplete_unknown_and_invalid_receipt_fields(self):
        original = copy.deepcopy(self.components)
        invalid_components = []
        for field in composition.AW_COMPONENT_FIELDS:
            candidate = copy.deepcopy(original)
            candidate[0].pop(field)
            invalid_components.append(candidate)
        for field, value in [
            ("extra", True), ("format", True), ("files", []),
            ("files", {"bin/aw": {"sha256": "b" * 64}}),
            ("files", {"bin/aw": {"sha256": "b" * 64, "mode": 493, "extra": True}}),
            ("files", {"bin/aw": {"sha256": "invalid", "mode": 493}}),
            ("files", {"bin/aw": {"sha256": "b" * 64, "mode": 511}}),
            ("files", {"../aw": {"sha256": "b" * 64, "mode": 493}}),
        ]:
            candidate = copy.deepcopy(original)
            candidate[0][field] = value
            invalid_components.append(candidate)
        for candidate in invalid_components:
            with self.subTest(candidate=candidate):
                self.components = candidate
                self.write_inputs()
                with self.assertRaises(ValueError):
                    self.compose()
                self.assertFalse(self.output.exists())
        self.components = original

    def test_boolean_format_is_rejected_at_manifest_and_component_levels(self):
        for location in ["manifest", "component"]:
            with self.subTest(location=location):
                manifest = {"format": 1, "components": copy.deepcopy(self.components)}
                if location == "manifest":
                    manifest["format"] = True
                else:
                    manifest["components"][0]["format"] = True
                self.archive(self.aw, {"aw-all-in-one/manifest.json": json.dumps(manifest).encode()})
                with self.assertRaisesRegex(ValueError, "unsupported AW manifest|incompatible AW component"):
                    self.compose()
                self.assertFalse(self.output.exists())

    def test_rejects_wrong_platform_and_version(self):
        for args in [dict(arch="x86_64"), dict(sight_version="0.14.2",
                     sight_release="https://github.com/alibaba/anolisa/releases/tag/sight/v0.14.2")]:
            with self.subTest(args=args), self.assertRaises(ValueError):
                self.compose(**args)
        self.assertFalse(self.output.exists())

    def test_checks_actual_elf_not_only_multiarch_contract(self):
        self.sight_files["bin/agentsight"] = b"not a native ELF binary"
        self.write_inputs()
        with self.assertRaisesRegex(ValueError, "incompatible AgentSight executable"):
            self.compose()

    def test_rejects_links_traversal_duplicates_and_missing_payload(self):
        for kind in ["link", "hardlink", "fifo", "absolute", "traversal", "duplicate", "missing"]:
            with self.subTest(kind=kind):
                extra = tarfile.TarInfo("bin/link")
                files = dict(self.sight_files)
                if kind == "link":
                    extra.type = tarfile.SYMTYPE
                    extra.linkname = "/etc/passwd"
                elif kind == "hardlink":
                    extra.type = tarfile.LNKTYPE
                    extra.linkname = "bin/agentsight"
                elif kind == "fifo":
                    extra.type = tarfile.FIFOTYPE
                elif kind == "absolute":
                    extra.name = "/absolute"
                elif kind == "traversal":
                    extra.name = "../escape"
                elif kind == "duplicate":
                    extra.name = "bin/agentsight"
                else:
                    files.pop("bin/agentsight-enforcer")
                    extra = None
                self.archive(self.sight, files, extra)
                with self.assertRaises(ValueError):
                    self.compose()
                self.assertFalse(self.output.exists())

    def test_rejects_nonrelease_source_and_keeps_existing_output(self):
        with self.assertRaisesRegex(ValueError, "pinned release tag"):
            self.compose(sight_release="https://example.com/latest")
        self.output.mkdir()
        (self.output / "user-file").write_text("keep")
        with self.assertRaisesRegex(ValueError, "already exists"):
            self.compose()
        self.assertEqual((self.output / "user-file").read_text(), "keep")

    def test_partial_publication_removes_only_owned_output(self):
        with patch.object(composition.os, "link", side_effect=OSError("disk full")):
            with self.assertRaisesRegex(OSError, "disk full"):
                self.compose()
        self.assertFalse(self.output.exists())
        self.assertTrue(self.aw.exists())
        self.assertTrue(self.sight.exists())

    def test_publication_conflict_retains_foreign_file(self):
        link = composition.os.link
        def inject_foreign(source, destination):
            if Path(destination).name == "agentsight.tar.gz":
                Path(destination).write_text("foreign")
            return link(source, destination)
        with patch.object(composition.os, "link", side_effect=inject_foreign):
            with self.assertRaises(FileExistsError):
                self.compose()
        self.assertEqual((self.output / "agentsight.tar.gz").read_text(), "foreign")
        self.assertFalse((self.output / "aw.tar.gz").exists())

    def test_shell_metacharacters_in_archive_paths_are_literal(self):
        unusual = self.root / 'input ; $(touch unexpected).tar.gz'
        self.sight.rename(unusual)
        self.sight = unusual
        self.compose()
        self.assertFalse((self.root / "unexpected").exists())
        self.assertEqual((self.output / "agentsight.tar.gz").read_bytes(), unusual.read_bytes())

    def test_rollback_keeps_a_modified_published_file(self):
        link = composition.os.link
        def modify_then_fail(source, destination):
            if Path(destination).name == "agentsight.tar.gz":
                installed = self.output / "aw.tar.gz"
                installed.unlink()
                installed.write_text("changed after publication")
                raise OSError("publication failed")
            return link(source, destination)
        with patch.object(composition.os, "link", side_effect=modify_then_fail):
            with self.assertRaisesRegex(OSError, "publication failed"):
                self.compose()
        self.assertEqual((self.output / "aw.tar.gz").read_text(), "changed after publication")

    def test_rollback_keeps_a_published_file_modified_in_place(self):
        link = composition.os.link
        def modify_then_fail(source, destination):
            if Path(destination).name == "agentsight.tar.gz":
                # The published archive and staging file still share the same inode.
                (self.output / "aw.tar.gz").write_text("modified through hardlink")
                raise OSError("publication failed")
            return link(source, destination)
        with patch.object(composition.os, "link", side_effect=modify_then_fail):
            with self.assertRaisesRegex(OSError, "publication failed"):
                self.compose()
        self.assertEqual((self.output / "aw.tar.gz").read_text(), "modified through hardlink")

    def test_cli_validates_without_executing_component_binaries(self):
        result = subprocess.run([
            sys.executable, "-B", str(AW / "scripts/compose.py"),
            "--aw-archive", str(self.aw), "--aw-sha256", composition.digest(self.aw),
            "--sight-archive", str(self.sight), "--sight-sha256", composition.digest(self.sight),
            "--sight-version", "0.14.1", "--sight-release",
            "https://github.com/agentic-os-org/ANOLISA/releases/tag/sight/v0.14.1",
            "--arch", "aarch64", "--output", str(self.output),
        ], cwd=self.root, capture_output=True, text=True, timeout=10,
           env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"})
        self.assertEqual(result.returncode, 0, result.stderr)
        # The synthetic ELF headers cannot execute; success proves metadata-only validation.
        self.assertTrue((self.output / "composition.json").is_file())

    def test_expanded_limit_rejects_a_small_compressed_archive(self):
        self.sight_files["share/padding"] = b"x" * (1024 * 1024)
        self.write_inputs()
        self.assertLess(self.sight.stat().st_size, 32768)
        with patch.object(composition, "MAX_BYTES", 32768):
            with self.assertRaisesRegex(ValueError, "expanded size limit"):
                self.compose()
        self.assertFalse(self.output.exists())

    def test_archive_expansion_is_bounded(self):
        for setting in ["MAX_MEMBERS", "MAX_BYTES", "MAX_METADATA"]:
            with self.subTest(setting=setting), patch.object(composition, setting, 1):
                with self.assertRaisesRegex(ValueError, "limit|too many"):
                    self.compose()
                self.assertFalse(self.output.exists())


class ServiceProfileTests(unittest.TestCase):
    def test_probe_free_services_are_independent_and_loopback_only(self):
        root = AW / "packaging/agentsight"
        collector = (root / "aw-agentsight-collector.service").read_text()
        query = (root / "aw-agentsight-query.service").read_text()
        self.assertIn("trace --no-ebpf", collector)
        self.assertIn("serve --host 127.0.0.1 --port 7396", query)
        for profile in [collector, query]:
            self.assertIn("--config %h/.config/aw/agentsight.json", profile)
            self.assertIn("ConditionPathExists=%h/.config/aw/agentsight.json", profile)
            self.assertIn("NoNewPrivileges=true", profile)
            self.assertIn("UMask=0077", profile)
            for forbidden in ["enforcer", "PartOf=", "BindsTo=", "Wants=", "ExecStopPost=", "0.0.0.0"]:
                self.assertNotIn(forbidden, profile)


if __name__ == "__main__":
    unittest.main()
