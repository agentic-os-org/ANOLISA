"""Verify bounded build selection without building Linux programs on the test host."""

from __future__ import annotations

import os
import subprocess
import sys
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build
import common
from test_packages import Fixture


class BuildTests(Fixture):
    def test_selected_packages_build_only_their_framework_or_bridge(self) -> None:
        expected = {
            "core": {"aw-cli"},
            "sec-core": {"aw-provider-sec-core"},
            "tokenless": {"aw-provider-tokenless"},
            "core sec-core tokenless": {"aw-cli", "aw-provider-sec-core", "aw-provider-tokenless"},
        }
        for selection, names in expected.items():
            with self.subTest(selection=selection):
                targets = build.build_targets(self.repo, selection)
                self.assertEqual({name for _, packages in targets for name in packages}, names)
                self.assertFalse(
                    any(
                        "src/tokenless" in str(path) or "agent-sec-core/v2" in str(path)
                        for path, _ in targets
                    )
                )

    def test_clean_checkout_is_rechecked_before_assembly(self) -> None:
        commands = []

        def command(argv: list[str], cwd: Path, *args: object) -> bytes:
            commands.append((argv, cwd))
            return self.commit.encode() if argv[:2] == ["git", "rev-parse"] else b""

        with patch.object(build.platform, "system", return_value="Linux"), patch.object(
            build.platform, "machine", return_value="x86_64"
        ), patch.object(build, "run", side_effect=command), patch.object(
            build, "assemble", return_value=[]
        ) as assemble:
            self.assertEqual(
                build.build(self.repo, self.version, "core", self.output, self.cancel), []
            )
        self.assertEqual(sum(argv[:2] == ["git", "status"] for argv, _ in commands), 2)
        self.assertEqual(sum(argv[:2] == ["git", "rev-parse"] for argv, _ in commands), 2)
        (cargo,) = [argv for argv, _ in commands if "+1.97.1" in argv]
        self.assertIn("--locked", cargo)
        self.assertEqual(cargo[-2:], ["-p", "aw-cli"])
        assemble.assert_called_once()

    def test_source_changes_abort_before_packaging(self) -> None:
        statuses = iter((b"", b" M changed"))

        def command(argv: list[str], *args: object) -> bytes:
            if argv[:2] == ["git", "status"]:
                return next(statuses)
            return self.commit.encode() if argv[:2] == ["git", "rev-parse"] else b""

        with patch.object(build.platform, "system", return_value="Linux"), patch.object(
            build.platform, "machine", return_value="x86_64"
        ), patch.object(build, "run", side_effect=command), patch.object(
            build, "assemble"
        ) as assemble:
            with self.assertRaisesRegex(ValueError, "changed"):
                build.build(self.repo, self.version, "core", self.output, self.cancel)
        assemble.assert_not_called()

    @unittest.skipUnless(os.name == "posix", "process-group cleanup requires POSIX")
    def test_cancellation_terminates_the_owned_command_group(self) -> None:
        pidfile = self.repo / "pid"
        script = "import os,time,pathlib; pathlib.Path('pid').write_text(str(os.getpid())); time.sleep(30)"

        def interrupt() -> None:
            deadline = time.monotonic() + 5
            while not pidfile.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.cancel.request()

        watcher = threading.Thread(target=interrupt)
        watcher.start()
        try:
            with self.assertRaises(common.Cancelled):
                common.run([sys.executable, "-c", script], self.repo, self.cancel, 10)
        finally:
            watcher.join(timeout=6)
        self.assertTrue(pidfile.exists())
        with self.assertRaises(ProcessLookupError):
            os.kill(int(pidfile.read_text()), 0)

    def test_non_utf8_process_arguments_fail_without_traceback(self) -> None:
        if os.name != "posix":
            self.skipTest("byte arguments require POSIX")
        result = subprocess.run(
            [
                os.fsencode(sys.executable),
                os.fsencode(build.__file__),
                b"--output",
                b"/tmp/output-\xff",
            ],
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn(b"UTF-8", result.stderr)
        self.assertNotIn(b"Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main()
