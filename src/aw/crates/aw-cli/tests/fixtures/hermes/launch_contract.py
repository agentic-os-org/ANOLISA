"""Validate Hermes launch identity before registering native hooks or readiness."""

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch


class LaunchTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory(dir=os.environ["AW_TEST_OUTPUT"])
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.path = self.root / "launch.json"
        self.ready = self.root / "ready.json"
        self.data = {
            "schema": "aw-hermes/v1alpha1", "binary": "/bin/true",
            "binding": str(self.root / "binding"), "profile": str(self.root),
            "cwd": str(Path.cwd()), "ready_path": str(self.ready),
            "hooks": [{"event": "tool.before", "step": "test", "on_error": "block", "timeout": 1}],
            "token": "test-token", "revision": "a" * 40,
        }
        source = Path(__file__).resolve().parents[5] / "crates/aw-adapter/native/hermes/__init__.py"
        spec = importlib.util.spec_from_file_location("aw_hermes_launch", source)
        self.plugin = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = self.plugin
        self.addCleanup(sys.modules.pop, spec.name, None)
        spec.loader.exec_module(self.plugin)
        self.version = SimpleNamespace(commit=None)
        shell = ModuleType("agent.shell_hooks")
        shell.ShellHookSpec = lambda **kwargs: SimpleNamespace(**kwargs)
        shell._make_callback = lambda spec: spec
        version = ModuleType("hermes_cli.version_info")
        version.get_version_info = lambda: self.version
        constants = ModuleType("hermes_constants")
        constants.get_hermes_home = lambda: self.root
        version.__file__ = str(self.root / "checkout/hermes_cli/version_info.py")
        (self.root / "checkout/.git").mkdir(parents=True)
        cli = ModuleType("hermes_cli")
        cli.version_info = version
        modules = patch.dict(sys.modules, {"hermes_cli": cli,
            "agent.shell_hooks": shell, "hermes_cli.version_info": version,
            "hermes_constants": constants,
            "_aw_hermes_session": SimpleNamespace(launch_path=str(self.path)),
        })
        modules.start()
        self.addCleanup(modules.stop)
        self.hooks = []
        self.context = SimpleNamespace(register_hook=lambda *args: self.hooks.append(args))

    def register(self) -> None:
        self.path.write_text(json.dumps(self.data))
        self.path.chmod(0o600)
        self.plugin.register(self.context)

    def test_frozen_source_requires_checked_contents_and_permissions(self):
        from aw_hermes_launch import aw_identity

        # A private tmp directory under /tmp also works on shared checkout hosts.
        with tempfile.TemporaryDirectory(dir="/tmp") as temp:
            source = Path(temp)
            file = source / "entry.py"
            file.write_text("print(1)")
            file.chmod(0o644)
            stamp = source / ".packaged-revision"
            digest = aw_identity.source_digest(source)
            with patch.object(aw_identity, "SOURCE_DIGEST", digest):
                self.assertEqual(aw_identity.verified_revision(source), aw_identity.REVISION)
                stamp.write_text(aw_identity.REVISION + "\n")
                stamp.chmod(0o644)
                self.assertEqual(aw_identity.verified_revision(source), aw_identity.REVISION)
                duplicate = source / ".other-revision"
                duplicate.write_bytes(stamp.read_bytes())
                duplicate.chmod(0o644)
                with self.assertRaisesRegex(ValueError, "multiple Hermes packaged revision stamps"):
                    aw_identity.verified_revision(source)
                duplicate.unlink()
                file.write_text("print(2)")
                with self.assertRaises(ValueError):
                    aw_identity.verified_revision(source)
                file.write_text("print(1)")
                file.chmod(0o755)
                with self.assertRaises(ValueError):
                    aw_identity.verified_revision(source)
                file.chmod(0o666)
                with self.assertRaises(ValueError):
                    aw_identity.source_digest(source)
                file.chmod(0o644)
                stamp.write_text(aw_identity.REVISION)
                with self.assertRaisesRegex(ValueError, "invalid Hermes packaged revision stamp"):
                    aw_identity.verified_revision(source)
                stamp.write_text("a" * 40 + "\n")
                with self.assertRaisesRegex(ValueError, "Hermes packaged revision differs"):
                    aw_identity.source_digest(source)
                stamp.unlink()
                (source / "alias.py").symlink_to(file)
                with self.assertRaises(ValueError):
                    aw_identity.source_digest(source)

    def test_result_commit_keeps_withholding_across_later_steps(self):
        from aw_hermes_launch import aw_result

        payload = {"tool_name": "terminal", "extra": {"result": "original"}}
        steps = [{"event": "tool.after", "on_error": "withhold_result"},
                 {"event": "tool.after", "on_error": "report"}]
        for failed in ({}, b"", b"[]", b"invalid", b'{"aw_withheld": true}', TimeoutError()):
            def invoke(hook):
                if hook is steps[1]:
                    return b'{"aw_replacement":"later"}'
                if isinstance(failed, Exception):
                    raise failed
                return failed
            self.assertEqual(aw_result.select_result(steps, payload, invoke), aw_result.WITHHELD)
        self.assertEqual(aw_result.select_result(steps[1:], payload, lambda _: b"invalid"), "original")
        self.assertEqual(aw_result.select_result(steps, payload, lambda _: b'{"aw_replacement":"safe"}'), "safe")

    def test_missing_or_malformed_revision_never_publishes_readiness(self) -> None:
        del self.data["revision"]
        with self.assertRaisesRegex(ValueError, "full Git object ID"):
            self.register()
        for revision in (None, "", 123, [], "a" * 39, "a" * 41, "a" * 63,
                         "a" * 65, "g" * 40, "a" * 39 + "\n", "０" * 40):
            with self.subTest(revision=revision):
                self.data["revision"] = revision
                with self.assertRaisesRegex(ValueError, "full Git object ID"):
                    self.register()
        self.assertEqual(self.hooks, [])
        self.assertFalse(self.ready.exists())

    def test_changed_or_unknown_native_revision_never_publishes_readiness(self) -> None:
        for commit in (None, "b" * 40):
            with self.subTest(commit=commit):
                self.version.commit = commit
                with self.assertRaisesRegex(ValueError, "checkout changed"):
                    self.register()
        self.assertEqual(self.hooks, [])
        self.assertFalse(self.ready.exists())

    def test_matching_full_git_ids_allow_other_revisions(self) -> None:
        for revision in ("a" * 40, "b" * 64, "ABCDEF0123" * 4):
            with self.subTest(revision=revision):
                self.data["revision"] = revision
                self.version.commit = revision
                self.register()
                self.assertEqual(json.loads(self.ready.read_text())["token"], "test-token")
                self.assertEqual(len(self.hooks), 1)
                self.hooks.clear()
                self.ready.unlink()


if __name__ == "__main__":
    unittest.main()
