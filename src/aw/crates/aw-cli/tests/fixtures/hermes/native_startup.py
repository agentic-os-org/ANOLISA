"""Exercise native-main interception without starting a user Hermes profile."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parents[4] / "aw-adapter/native/hermes/startup.py"


class NativeStartupTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory(prefix="aw-hermes-startup-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.launch = self.root / "hermes-launch.json"
        self.launch.write_text(json.dumps({"schema": "aw-hermes/v1alpha1"}))
        self.launch.chmod(0o600)
        self.guard = self.root / "hermes-entrypoint.py"
        self.guard.write_text(
            "import os, sys\nfrom pathlib import Path\n"
            "def main():\n"
            "    assert sys.argv[1:] == ['hermes', 'chat', '--cli']\n"
            "    Path(os.environ['AW_TEST_MARKER']).write_text(str(os.getpid()))\n"
        )
        self.guard.chmod(0o600)
        self.reply = {"binding": str(self.root / "binding.json"), "instance_id": "test-instance", "command": {"args": [str(self.guard), "hermes", "chat", "--cli"]}, "environment": {
            "AW_HERMES_LAUNCH": str(self.launch),
        }}
        self.binary = self.root / "aw"
        self.binary.write_text(
            "#!" + sys.executable + "\nimport json, os, sys\nfrom pathlib import Path\n"
            + "if sys.argv[1:3] == ['instance', 'release']:\n"
            + "    Path(" + repr(str(self.root / "release.json")) + ").write_text("
            "json.dumps({'args':sys.argv[1:], 'owner':os.getppid()}))\n"
            + "    raise SystemExit(0)\n"
            + "Path(" + repr(str(self.root / "prepare.json")) + ").write_text("
            "json.dumps({'args':sys.argv[1:], 'owner':os.getppid()}))\n"
            + "print(" + repr(json.dumps(self.reply)) + ")\n"
        )
        self.binary.chmod(0o700)
        self.descriptor = self.root / "bootstrap.json"
        self.descriptor.write_text(json.dumps({
            "binary": str(self.binary), "config": str(self.root / "aw.yaml"),
            "agent": "hermes", "profile": str(self.root / "profile"),
        }))
        self.descriptor.chmod(0o600)
        package = self.root / "hermes_cli"
        package.mkdir()
        (package / "__init__.py").write_text("")
        (package / "main.py").write_text(
            "from pathlib import Path\nimport os\n"
            "def main():\n"
            "    Path(os.environ['AW_TEST_MARKER'] + '.unguarded').touch()\n"
        )

    def run_native(self, args: list[str] | None = None) -> subprocess.CompletedProcess:
        script = (
            "import importlib.util, sys\n"
            "sys.argv = " + repr(args or ["hermes", "chat", "--cli"]) + "\n"
            "spec = importlib.util.spec_from_file_location('aw_startup', " + repr(str(SOURCE)) + ")\n"
            "module = importlib.util.module_from_spec(spec)\nspec.loader.exec_module(module)\n"
            "module.install()\nmodule.install()\nfrom hermes_cli import main\nmodule.install()\nmain.main()\n"
        )
        return subprocess.run([sys.executable, "-B", "-c", script], cwd=self.root,
            env={**os.environ, "AW_INSTANCE_BOOTSTRAP": str(self.descriptor),
                 "AW_TEST_MARKER": str(self.root / "native")},
            capture_output=True, timeout=10, check=False)

    def test_native_owner_prepares_and_runs_existing_guard_in_process(self) -> None:
        result = self.run_native()
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        prepared = json.loads((self.root / "prepare.json").read_text())
        self.assertEqual(int((self.root / "native").read_text()), prepared["owner"])
        self.assertEqual(prepared["args"][:2], ["instance", "prepare"])
        self.assertFalse((self.root / "native.unguarded").exists())
        released = json.loads((self.root / "release.json").read_text())
        self.assertEqual(released["owner"], prepared["owner"])
        self.assertEqual(released["args"], ["instance", "release", "--binding", self.reply["binding"]])

    def test_guard_exit_and_exception_preserve_native_failure_and_release(self) -> None:
        for failure, expected in [("raise SystemExit(0)", 0), ("raise SystemExit(7)", 7), ("raise RuntimeError('native failure')", 1)]:
            with self.subTest(failure=failure):
                self.guard.write_text("def main():\n    " + failure + "\n")
                result = self.run_native()
                self.assertEqual(result.returncode, expected, result.stderr.decode())
                self.assertTrue((self.root / "release.json").is_file())
                (self.root / "release.json").unlink()

    def test_import_failures_after_preparation_release(self) -> None:
        package = self.root / "hermes_cli/main.py"
        for path in [package, self.guard]:
            with self.subTest(path=path):
                original = path.read_text()
                path.write_text("raise RuntimeError('import failure')\n")
                result = self.run_native()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(b"import failure", result.stderr)
                self.assertTrue((self.root / "release.json").is_file())
                (self.root / "release.json").unlink()
                path.write_text(original)

    def test_cleanup_failure_is_visible_and_preserves_original_exit(self) -> None:
        self.binary.write_text(self.binary.read_text().replace("raise SystemExit(0)", "raise SystemExit(3)"))
        result = self.run_native()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"release failed", result.stderr)
        self.guard.write_text("def main():\n    raise SystemExit(0)\n")
        result = self.run_native()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"release failed", result.stderr)
        self.guard.write_text("def main():\n    raise SystemExit(7)\n")
        result = self.run_native()
        self.assertEqual(result.returncode, 7)
        self.assertIn(b"cleanup failed", result.stderr)

    def test_preparation_failure_cannot_dispatch_native_main(self) -> None:
        self.binary.write_text("#!" + sys.executable + "\nraise SystemExit(1)\n")
        result = self.run_native()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "native").exists())
        self.assertFalse((self.root / "native.unguarded").exists())

    def test_unsafe_descriptor_and_guard_are_rejected(self) -> None:
        self.descriptor.chmod(0o644)
        self.assertNotEqual(self.run_native().returncode, 0)
        self.descriptor.chmod(0o600)
        self.guard.chmod(0o644)
        self.assertNotEqual(self.run_native().returncode, 0)
        self.assertTrue((self.root / "release.json").is_file())
        self.assertFalse((self.root / "native").exists())
        self.assertFalse((self.root / "native.unguarded").exists())

    def test_native_arguments_must_match_the_prepared_target(self) -> None:
        result = self.run_native(["hermes", "chat", "--cli", "--query", "different"])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"arguments differ", result.stderr)
        self.assertTrue((self.root / "release.json").is_file())
        self.assertFalse((self.root / "native").exists())
        self.assertFalse((self.root / "native.unguarded").exists())

    def test_help_and_chat_values_do_not_prepare_an_instance(self) -> None:
        self.descriptor.unlink()
        for arguments in (["hermes", "chat", "--help"], ["hermes", "chat", "-h"],
                          ["hermes", "config", "get", "chat"],
                          ["hermes", "model", "--provider", "chat"]):
            with self.subTest(arguments=arguments):
                result = self.run_native(arguments)
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                self.assertFalse((self.root / "prepare.json").exists())
                self.assertTrue((self.root / "native.unguarded").exists())

    def test_management_commands_remain_native(self) -> None:
        self.assertEqual(self.run_native(["hermes", "--version"]).returncode, 0)
        self.assertFalse((self.root / "prepare.json").exists())
        self.assertTrue((self.root / "native.unguarded").exists())


if __name__ == "__main__":
    unittest.main()
