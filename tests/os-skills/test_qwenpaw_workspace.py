"""Reinstalling must not discard the user's personalized workspace notes."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/ai/install-qwenpaw/scripts/setup.sh"
FILES = ("AGENTS.md", "SOUL.md", "PROFILE.md", "MEMORY.md", "BOOTSTRAP.md", "HEARTBEAT.md")


@unittest.skipUnless(os.name == "posix", "uses private POSIX shell fixtures")
class QwenPawWorkspaceTests(unittest.TestCase):
    def run_copy(self, prepare):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            skill = root / "skill"
            reference = skill / "reference"
            reference.mkdir(parents=True)
            workspace = root / "workspace"
            workspace.mkdir()
            for name in FILES:
                (reference / name).write_text(f"template {name}\n", encoding="utf-8")
            prepare(workspace)
            before = {p.name: p.read_bytes() for p in workspace.iterdir() if p.is_file()}
            source = SCRIPT.read_text(encoding="utf-8")
            fragment = source[source.index("# 3d:") : source.index("# ── 步骤 5")]
            result = subprocess.run(
                ["bash", "-eu", "-c", fragment],
                env={**os.environ, "SKILL_DIR": str(skill), "QWENPAW_DIR": str(workspace)},
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            for name in FILES:
                if (workspace / name).is_symlink():
                    self.assertEqual(os.readlink(workspace / name), "missing-personal-note")
                else:
                    expected = before.get(name, f"template {name}\n".encode())
                    self.assertEqual((workspace / name).read_bytes(), expected, name)
            return result.stdout

    def test_existing_personalized_files_remain_unchanged(self):
        def prepare(workspace):
            for name in FILES:
                (workspace / name).write_text(f"personal {name}\n", encoding="utf-8")

        self.run_copy(prepare)

    def test_partial_workspace_seeds_only_missing_files(self):
        self.run_copy(
            lambda workspace: (workspace / "MEMORY.md").write_text("remembered history\n")
        )

    def test_first_install_seeds_all_templates(self):
        self.run_copy(lambda workspace: None)

    def test_existing_empty_file_is_a_personal_choice(self):
        self.run_copy(lambda workspace: (workspace / "HEARTBEAT.md").touch())

    def test_dangling_symlink_is_preserved_without_creating_its_target(self):
        self.run_copy(
            lambda workspace: (workspace / "PROFILE.md").symlink_to("missing-personal-note")
        )


if __name__ == "__main__":
    unittest.main()
