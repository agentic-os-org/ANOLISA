"""Exercise raw assessment staging without native builds or Docker."""

import io
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
from pathlib import Path

COMPONENT = Path(__file__).resolve().parents[2]
STAGER = COMPONENT / "packaging/raw/stage_skill_score.py"


class ScoreRawPackageTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.stage = self.root / "stage"
        (self.stage / ".anolisa").mkdir(parents=True)
        shutil.copyfile(
            COMPONENT / ".anolisa/component.toml",
            self.stage / ".anolisa/component.toml",
        )
        self.base = tomllib.loads((self.stage / ".anolisa/component.toml").read_text())[
            "component"
        ]["version"]
        self.version = self.base + "-skillscore.1234567"
        (self.stage / "bin").mkdir()
        (self.stage / "bin/agent-sec-cli").write_bytes(b"V2 binary fixture")
        self.payload = self.root / "score.tar.gz"

    def archive(self, extra: tarfile.TarInfo | None = None) -> None:
        with tarfile.open(self.payload, "w:gz") as archive:
            for name, contents in (
                ("cli/bin/python3.11", '#!/bin/sh\nprintf "%s\\n" "$@"\n'),
                (
                    "cli/bin/agent-sec-cli",
                    (COMPONENT / "packaging/skill-score/agent-sec-cli").read_text(),
                ),
                ("runtime/bin/python3.12", "#!/bin/sh\nexit 0\n"),
                ("cli/lib/data.txt", "data"),
            ):
                kind, rest = name.split("/", 1)
                entry = tarfile.TarInfo(f"opt/agent-sec/skill-score-{kind}/{rest}")
                data = contents.encode()
                entry.size = len(data)
                entry.mode = 0o755 if "/bin/" in name else 0o644
                archive.addfile(entry, io.BytesIO(data))
            link = tarfile.TarInfo("opt/agent-sec/skill-score-cli/bin/python3")
            link.type, link.linkname = tarfile.SYMTYPE, "python3.11"
            archive.addfile(link)
            if extra is not None:
                archive.addfile(extra, io.BytesIO())

    def stage_score(
        self, version: str | None = None
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(STAGER),
                str(self.stage),
                str(self.payload),
                version or self.version,
            ],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_installer_contract_preserves_v2_and_relocates_private_cli(self) -> None:
        self.archive()
        result = self.stage_score()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (self.stage / "bin/agent-sec-cli").read_bytes(), b"V2 binary fixture"
        )
        contract = tomllib.loads((self.stage / ".anolisa/component.toml").read_text())
        self.assertEqual(contract["component"]["name"], "sec-core")
        self.assertEqual(contract["component"]["version"], self.version)
        targets = {
            entry["target"] for entry in contract["component"]["layout"]["files"]
        }
        self.assertIn("{bindir}/agent-sec-cli", targets)
        self.assertNotIn("{bindir}/agent-sec-skill-score", targets)
        self.assertFalse((self.stage / "bin/agent-sec-skill-score").exists())
        self.assertIn("{libdir}/sec-core/skill-score/", targets)
        tree = next(
            entry
            for entry in contract["component"]["layout"]["files"]
            if entry["target"] == "{libdir}/sec-core/skill-score/"
        )
        self.assertNotIn("mode", tree)
        self.assertFalse(list(self.stage.rglob("*.tar*")))
        self.assertFalse(any(path.is_symlink() for path in self.stage.rglob("*")))
        command = subprocess.run(
            [
                str(
                    self.stage
                    / "lib/anolisa/sec-core/skill-score/cli/bin/agent-sec-cli"
                ),
                "skill-score",
                "plan",
                "path with spaces",
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            command.stdout.splitlines(),
            [
                "-I",
                "-m",
                "agent_sec_cli.skill_score.entrypoint",
                "skill-score",
                "plan",
                "path with spaces",
            ],
        )
        self.assertEqual(
            (self.stage / "lib/anolisa/sec-core/skill-score/cli/lib/data.txt")
            .stat()
            .st_mode
            & 0o777,
            0o644,
        )

    def test_preview_requires_base_version_and_safe_commit_suffix(self) -> None:
        self.archive()
        original = (self.stage / ".anolisa/component.toml").read_bytes()
        for version in (
            self.base,
            "9.9.9-skillscore.1234567",
            self.base + "-skillscore../bad",
        ):
            with self.subTest(version=version):
                result = self.stage_score(version)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("preview version", result.stderr)
        self.assertEqual(
            (self.stage / ".anolisa/component.toml").read_bytes(), original
        )

    def test_custom_base_variant_preserves_component_version(self) -> None:
        self.archive()
        version = self.version + ".base.abcdef012345"
        result = self.stage_score(version)
        self.assertEqual(result.returncode, 0, result.stderr)
        contract = tomllib.loads((self.stage / ".anolisa/component.toml").read_text())
        self.assertEqual(contract["component"]["version"], version)

    def test_payload_cannot_overwrite_default_cli(self) -> None:
        self.archive(tarfile.TarInfo("bin/agent-sec-cli"))
        result = self.stage_score()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("escapes assessment prefixes", result.stderr)
        self.assertEqual(
            (self.stage / "bin/agent-sec-cli").read_bytes(), b"V2 binary fixture"
        )

    def test_private_daemon_is_rejected(self) -> None:
        self.archive(
            tarfile.TarInfo("opt/agent-sec/skill-score-cli/bin/agent-sec-daemon")
        )
        result = self.stage_score()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("without a daemon", result.stderr)

    def test_runtime_download_cache_cannot_enter_package(self) -> None:
        self.archive(tarfile.TarInfo("opt/agent-sec/python-3.12.11-20250902.tar.gz"))
        result = self.stage_score()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("escapes assessment prefixes", result.stderr)
        self.assertFalse((self.stage / "lib/anolisa/sec-core/skill-score").exists())


if __name__ == "__main__":
    unittest.main()
