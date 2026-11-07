"""Exercise the real Hermes update function against local Git repositories."""

import os
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/ai/install-hermes/scripts/install.sh"


@unittest.skipUnless(
    os.name == "posix" and shutil.which("bash") and shutil.which("git"), "POSIX tools"
)
class HermesAutostashTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="hermes-autostash-")
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.remote = self.base / "remote.git"
        self.checkout = self.base / "checkout"
        self.remote.mkdir()
        self.git("init", "--bare", "--initial-branch=main", directory=self.remote)
        self.git("clone", str(self.remote), str(self.checkout), directory=self.base)
        self.git("config", "user.name", "Hermes Test")
        self.git("config", "user.email", "hermes-test@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "core.autocrlf", "false")
        (self.checkout / "local.txt").write_text("committed\n", encoding="utf-8")
        (self.checkout / "other.txt").write_text("other committed\n", encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-m", "seed")
        self.git("push", "origin", "main")

    def git(self, *arguments: str, directory: Path | None = None) -> str:
        result = subprocess.run(
            ["git", *arguments],
            cwd=directory or self.checkout,
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=20,
            check=True,
        )
        return result.stdout.strip()

    def dirty_checkout(self) -> None:
        (self.checkout / "local.txt").write_text("local customization\n", encoding="utf-8")
        (self.checkout / "untracked.txt").write_text("untracked customization\n", encoding="utf-8")

    def existing_stash(self) -> str:
        (self.checkout / "other.txt").write_text("unrelated customization\n", encoding="utf-8")
        self.git("stash", "push", "-m", "user-owned stash")
        return self.git("rev-parse", "refs/stash")

    def run_update(self, wrapper: str = "") -> subprocess.CompletedProcess[str]:
        source = SCRIPT.read_text(encoding="utf-8")
        function = re.search(r"^clone_repo\(\) \{.*?^\}", source, re.M | re.S)
        self.assertIsNotNone(function)
        loggers = "\n".join(
            f"{name}() {{ printf '%s\\n' \"$*\"; }}"
            for name in ("log_info", "log_success", "log_warn", "log_error")
        )
        driver = (
            loggers
            + "\n"
            + function.group(0)
            + "\n"
            + wrapper
            + f"\nINSTALL_DIR={shlex.quote(str(self.checkout))}\nBRANCH=main\nclone_repo\n"
        )
        return subprocess.run(
            ["bash", "-e"],
            input=driver,
            cwd=self.base,
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=30,
        )

    def assert_customizations_restored(self) -> None:
        self.assertEqual(
            (self.checkout / "local.txt").read_text(encoding="utf-8"), "local customization\n"
        )
        self.assertEqual(
            (self.checkout / "untracked.txt").read_text(encoding="utf-8"),
            "untracked customization\n",
        )

    def test_successful_dirty_update_restores_and_removes_only_autostash(self) -> None:
        self.dirty_checkout()
        result = self.run_update()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_customizations_restored()
        self.assertEqual(self.git("stash", "list"), "")
        self.assertIn("Repository ready", result.stdout)

    def test_successful_update_preserves_preexisting_stash(self) -> None:
        previous = self.existing_stash()
        self.dirty_checkout()
        result = self.run_update()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_customizations_restored()
        self.assertEqual(self.git("stash", "list", "--format=%H"), previous)

    def test_newer_unrelated_stash_is_not_dropped(self) -> None:
        previous = self.existing_stash()
        self.dirty_checkout()
        wrapper = f"""
git() {{
    command git "$@"
    if [ "$1" = stash ] && [ "${{2:-}}" = apply ]; then
        command git stash store -m inserted-after-apply {shlex.quote(previous)}
    fi
}}
"""
        result = self.run_update(wrapper)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_customizations_restored()
        self.assertEqual(
            self.git("stash", "list", "--format=%H").splitlines(), [previous, previous]
        )

    def test_missing_restored_entry_does_not_drop_an_unrelated_stash(self) -> None:
        previous = self.existing_stash()
        self.dirty_checkout()
        wrapper = """
git() {
    command git "$@"
    if [ "$1" = stash ] && [ "${2:-}" = apply ]; then
        command git stash drop 'stash@{0}' >/dev/null
    fi
}
"""
        result = self.run_update(wrapper)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_customizations_restored()
        self.assertEqual(self.git("stash", "list", "--format=%H"), previous)

    def test_failed_restore_keeps_autostash_and_returns_failure(self) -> None:
        self.dirty_checkout()
        wrapper = """
git() {
    if [ "$1" = stash ] && [ "${2:-}" = apply ]; then
        return 1
    fi
    command git "$@"
}
"""
        result = self.run_update(wrapper)
        self.assertEqual(result.returncode, 1)
        self.assertIn("restoring local changes failed", result.stdout)
        self.assertIn("hermes-install-autostash", self.git("stash", "list"))
        self.assertEqual((self.checkout / "local.txt").read_text(encoding="utf-8"), "committed\n")

    def test_clean_update_does_not_change_an_existing_stash(self) -> None:
        previous = self.existing_stash()
        result = self.run_update()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git("stash", "list", "--format=%H"), previous)


if __name__ == "__main__":
    unittest.main()
