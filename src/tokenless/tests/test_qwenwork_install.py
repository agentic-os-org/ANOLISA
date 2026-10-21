#!/usr/bin/env python3
"""Sandbox-only lifecycle and registered-command tests for QwenWorkCN."""

from __future__ import annotations

import sys

sys.dont_write_bytecode = True

import ast
import contextlib
import importlib.util
import io
import json
import os
import plistlib
import re
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from test_compress_response_hook import _create_mock_tokenless

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "adapters/tokenless/qwenwork"
COMMON = ROOT / "adapters/tokenless/common/hooks"
CONTRACT_MOCK = ROOT / "tests/contract/mock_tokenless.py"
SPEC = importlib.util.spec_from_file_location("qwenwork_lifecycle", SOURCE / "scripts/lifecycle.py")
lifecycle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(lifecycle)


def snapshot(root: Path) -> dict:
    """Include modes and mtimes so a no-op rewrite also fails read-only checks."""
    result = {}
    if not root.exists():
        return result
    for path in [root, *sorted(root.rglob("*"))]:
        metadata = path.lstat()
        if path.is_symlink():
            value = ("link", os.readlink(path))
        elif path.is_file():
            value = ("file", path.read_bytes())
        else:
            value = ("directory",)
        result[str(path.relative_to(root))] = (metadata.st_mode, metadata.st_mtime_ns, value)
    return result


class QwenWorkInstallTests(unittest.TestCase):
    def setUp(self) -> None:
        sandbox = tempfile.TemporaryDirectory(prefix="tokenless-qwenwork-特殊 '")
        self.addCleanup(sandbox.cleanup)
        self.root = Path(sandbox.name).resolve()
        self.home = self.root / "home space '$;`"
        self.home.mkdir()
        self.adapters = self.root / "adapter source '$;`"
        self.source = self.adapters / "qwenwork"
        ignored = shutil.ignore_patterns("__pycache__", "*.pyc")
        shutil.copytree(SOURCE, self.source, symlinks=True, ignore=ignored)
        shutil.copytree(COMMON, self.adapters / "common/hooks", ignore=ignored)
        self.manifest = self.source / ".qoder-plugin/plugin.json"
        template = (self.source / ".qoder-plugin/plugin.json.in").read_text(encoding="utf-8")
        self.manifest.write_text(template.replace("@VERSION@", "1.0.0"), encoding="utf-8")
        self.app = self.root / "千问 '$;` office.app"
        (self.app / "Contents").mkdir(parents=True)
        self.write_app()
        self.data = self.home / "office data '$;`"
        self.target = self.data / "plugins-custom/tokenless"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "python3").symlink_to(sys.executable)
        (self.bin / "bash").symlink_to("/bin/bash")
        self.executable("uname", "#!/bin/sh\nprintf '%s\\n' Darwin\n")
        self.executable("rtk", "#!/bin/sh\nexit 0\n")
        _create_mock_tokenless(str(self.bin))
        self.forbidden_log = self.root / "forbidden-command.log"
        for name in ("qoderclicn", "killall", "pkill", "osascript"):
            self.executable(name, '#!/bin/sh\nprintf invoked >> "$QWEN_FORBIDDEN_LOG"\nexit 99\n')
        self.env = {
            "HOME": str(self.home), "PATH": str(self.bin),
            "TOKENLESS_QWENWORK_APP": str(self.app),
            "TOKENLESS_QWENWORK_HOME": str(self.data),
            "QWEN_FORBIDDEN_LOG": str(self.forbidden_log),
            "PYTHONDONTWRITEBYTECODE": "1",
        }
        for name in (
            "settings.json", "disabled.json", "blocked.json", "permissions.json",
            "desktop.sqlite", "plugins-custom/other/plugin.json",
        ):
            path = self.data / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'{"tokenless":"disabled","unchanged":true}\n')
        for name in (".qoder", ".qwen", ".qwenpaw", ".qwenworkcn"):
            path = self.home / name / "settings.json"
            path.parent.mkdir(parents=True)
            path.write_bytes(b"untouched\n")
        self.protected = {
            path: (path.read_bytes(), path.stat().st_mtime_ns)
            for path in self.home.rglob("*") if path.is_file()
        }
        for patcher in (
            mock.patch.dict(os.environ, self.env, clear=True),
            mock.patch.object(lifecycle, "PLUGIN_ROOT", self.source),
            mock.patch.object(lifecycle.platform, "system", return_value="Darwin"),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)
        # Do not let a system installation satisfy a deliberately missing
        # sandbox dependency through the shared resolver's FHS fallbacks.
        real_isfile = os.path.isfile
        patcher = mock.patch.object(
            os.path, "isfile",
            side_effect=lambda path: Path(path).is_relative_to(self.root) and real_isfile(path),
        )
        patcher.start()
        self.addCleanup(patcher.stop)

    def tearDown(self) -> None:
        for path, original in self.protected.items():
            self.assertEqual((path.read_bytes(), path.stat().st_mtime_ns), original, str(path))
        self.assertFalse(self.forbidden_log.exists(), "must not call desktop CLI or stop the app")

    def executable(self, name: str, content: str) -> Path:
        path = self.bin / name
        path.write_text(content, encoding="utf-8")
        path.chmod(0o755)
        return path

    def write_app(
        self, version: str = "1.2.5", identifier: str = "cn.qwenwork.desktop.mac"
    ) -> None:
        with (self.app / "Contents/Info.plist").open("wb") as handle:
            plistlib.dump(
                {"CFBundleIdentifier": identifier, "CFBundleShortVersionString": version}, handle
            )

    def run_action(self, *args: str, success: bool = True) -> tuple[str, str]:
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = lifecycle.main(list(args))
        self.assertEqual(status, 0 if success else 1, stdout.getvalue() + stderr.getvalue())
        return stdout.getvalue(), stderr.getvalue()

    def upgrade_source(self) -> None:
        manifest = json.loads(self.manifest.read_text())
        manifest["version"] = "2.0.0"
        self.manifest.write_text(json.dumps(manifest), encoding="utf-8")

    def assert_modified_protected(self) -> None:
        before = snapshot(self.target)
        for action in (("install",), ("install", "--dry-run"), ("uninstall",)):
            with self.subTest(action=action):
                _, error = self.run_action(*action, success=False)
                self.assertRegex(error, "modified|symlink|marker|hard link")
                self.assertEqual(snapshot(self.target), before)

    def test_detect_and_dry_run_write_nothing(self) -> None:
        # Even absent parent directories and an unstaged source remain untouched.
        missing = self.home / "not-created/data"
        with mock.patch.dict(os.environ, {"TOKENLESS_QWENWORK_HOME": str(missing)}):
            before = snapshot(self.root)
            out, _ = self.run_action("detect")
            self.assertIn("1.2.5", out)
            self.assertIn("not sideloaded", out)
            self.assertNotIn("activated", out)
            out, _ = self.run_action("install", "--dry-run")
            self.assertIn("DRY-RUN", out)
            self.assertEqual(snapshot(self.root), before)
            self.assertFalse(missing.parent.exists())

    def test_missing_application_and_wrong_identity(self) -> None:
        with mock.patch.dict(os.environ, {"TOKENLESS_QWENWORK_APP": str(self.root / "absent.app")}):
            before = snapshot(self.root)
            _, error = self.run_action("detect", success=False)
            self.assertIn("not found", error)
            self.run_action("install", "--dry-run", success=False)
            self.assertEqual(snapshot(self.root), before)
        self.write_app(identifier="com.other.app")
        _, error = self.run_action("install", success=False)
        self.assertIn("bundle identifier", error)
        self.assertFalse(self.target.exists())

    def test_invalid_plist_and_missing_version_are_actionable(self) -> None:
        info = self.app / "Contents/Info.plist"
        missing_version = plistlib.dumps({"CFBundleIdentifier": "cn.qwenwork.desktop.mac"})
        for content, message in (
            (b"not a plist", "metadata"),
            (missing_version, "CFBundleShortVersionString"),
        ):
            with self.subTest(message=message):
                info.write_bytes(content)
                _, error = self.run_action("detect", success=False)
                self.assertIn(message, error)

    def test_unverified_version_is_reported_without_compatibility_claim(self) -> None:
        self.write_app(version="1.0.0")
        out, error = self.run_action("detect")
        self.assertIn("version 1.0.0", out)
        self.assertIn("1.2.5 native Bash and 1.3.2 workspace Shell", out)
        self.assertIn("compatibility is unverified", error)

    def test_default_application_and_home_paths(self) -> None:
        default_app = self.home / "Applications/QwenWorkCN.app"
        default_app.parent.mkdir()
        shutil.copytree(self.app, default_app)
        original_is_dir = Path.is_dir

        def sandbox_app(path: Path) -> bool:
            return path != Path("/Applications/QwenWorkCN.app") and original_is_dir(path)

        with mock.patch.dict(os.environ):
            os.environ.pop("TOKENLESS_QWENWORK_APP")
            os.environ.pop("TOKENLESS_QWENWORK_HOME")
            with mock.patch.object(Path, "is_dir", autospec=True, side_effect=sandbox_app):
                out, _ = self.run_action("detect")
        self.assertIn(str(default_app), out)
        self.assertIn(str(self.home / ".qwenworkcn/plugins-custom/tokenless"), out)

    def test_missing_tokenless_fails_without_writes(self) -> None:
        (self.bin / "tokenless").unlink()
        before = snapshot(self.root)
        for action in (("detect",), ("install", "--dry-run"), ("install",)):
            _, error = self.run_action(*action, success=False)
            self.assertIn("tokenless binary not found", error)
        self.assertEqual(snapshot(self.root), before)

    def test_shared_resolver_finds_user_install_layout(self) -> None:
        user_bin = self.home / ".local/bin/tokenless"
        user_bin.parent.mkdir(parents=True)
        shutil.move(str(self.bin / "tokenless"), user_bin)
        out, _ = self.run_action("detect")
        self.assertIn(str(user_bin), out)

    def test_missing_rtk_warns_but_allows_install(self) -> None:
        (self.bin / "rtk").unlink()
        _, error = self.run_action("install")
        self.assertIn("command rewriting is unavailable", error)
        self.assertTrue(self.target.is_dir())
        self.assertFalse((self.bin / "request.json").exists(), "detect must not invoke binaries")

    def test_non_darwin_rejects_install(self) -> None:
        before = snapshot(self.root)
        with mock.patch.object(lifecycle.platform, "system", return_value="Linux"):
            for action in (("detect",), ("install",), ("install", "--dry-run")):
                _, error = self.run_action(*action, success=False)
                self.assertIn("Darwin", error)
        self.assertEqual(snapshot(self.root), before)

    def test_runtime_manifest_and_shared_resources_required(self) -> None:
        self.manifest.unlink()
        before = snapshot(self.root)
        _, error = self.run_action("install", "--dry-run", success=False)
        self.assertIn("stamp adapter templates", error)
        self.assertEqual(snapshot(self.root), before)
        self.manifest.write_text('{"name":"tokenless","version":"@VERSION@"}')
        _, error = self.run_action("detect", success=False)
        self.assertIn("unstamped", error)
        self.manifest.write_text('{"name":"tokenless","version":"1.0.0"}')
        (self.adapters / "common/hooks/hook_utils.py").unlink()
        self.run_action("install", success=False)
        self.assertFalse(self.target.exists())

    def test_unreadable_and_invalid_runtime_resources_report_the_path(self) -> None:
        hooks = self.source / "hooks/hooks.json"
        access = os.access
        with mock.patch.object(
            lifecycle.os, "access",
            side_effect=lambda path, mode: path != hooks and access(path, mode),
        ):
            _, error = self.run_action("install", success=False)
        self.assertIn(str(hooks), error)
        hooks.write_text("invalid JSON")
        _, error = self.run_action("install", success=False)
        self.assertIn(str(hooks), error)
        self.assertFalse(self.target.exists())

    def test_staged_runtime_validation_precedes_replacement(self) -> None:
        self.run_action("install")
        self.upgrade_source()
        original = snapshot(self.target)
        copytree = shutil.copytree

        def corrupt_copy(source: Path, destination: Path, *args, **kwargs) -> Path:
            result = copytree(source, destination, *args, **kwargs)
            if source == self.source:
                (Path(destination) / "hooks/shared/hook_utils.py").unlink()
            return result

        with mock.patch.object(lifecycle.shutil, "copytree", side_effect=corrupt_copy):
            _, error = self.run_action("install", success=False)
        self.assertIn("hook_utils.py", error)
        self.assertIn("recovery files retained", error)
        self.assertEqual(snapshot(self.target), original)

    def test_install_reinstall_upgrade_uninstall_are_idempotent(self) -> None:
        cache = self.adapters / "common/hooks/__pycache__"
        cache.mkdir()
        (cache / "hook_utils.pyc").write_bytes(b"ignore")
        (self.source / "hooks/ignored.pyc").write_bytes(b"ignore")
        out, _ = self.run_action("install")
        self.assertIn("Sideloaded:", out)
        self.assertIn("new session", out)
        self.assertIn("UI", out)
        shared = self.target / "hooks/shared"
        self.assertTrue(shared.is_dir())
        self.assertFalse(shared.is_symlink())
        self.assertFalse(any(path.is_symlink() for path in self.target.rglob("*")))
        self.assertFalse(list(self.target.rglob("*.in")))
        self.assertFalse(list(self.target.rglob("*.pyc")))
        self.assertFalse(list(self.target.rglob("__pycache__")))
        before = snapshot(self.target)
        self.run_action("install")
        self.assertEqual(snapshot(self.target), before)
        self.upgrade_source()
        self.run_action("install")
        manifest = json.loads((self.target / ".qoder-plugin/plugin.json").read_text())
        self.assertEqual(manifest["version"], "2.0.0")
        self.assertIsNotNone(lifecycle._owned(self.target))
        self.assertFalse(list(self.data.glob(".tokenless-stage-*")))
        self.run_action("uninstall", "--dry-run")
        self.assertTrue(self.target.exists())
        self.run_action("uninstall")
        self.assertFalse(self.target.exists())
        before = snapshot(self.root)
        self.run_action("uninstall")
        self.assertEqual(snapshot(self.root), before)

    def test_anolisa_dry_run_env_var_previews_like_the_flag(self) -> None:
        # The adapter family contract is ANOLISA_DRY_RUN=1; `--dry-run` is this
        # adapter's own spelling of the same request. The variable must reach
        # the same preview and leave the tree alone.
        self.run_action("install")
        self.assertTrue(self.target.exists())
        before = snapshot(self.root)
        with mock.patch.dict(os.environ, {"ANOLISA_DRY_RUN": "1"}):
            out, _ = self.run_action("uninstall")
            self.assertIn("DRY-RUN: would remove owned plugin", out)
            self.assertTrue(self.target.exists())
            self.assertEqual(snapshot(self.root), before)
            env_out, _ = self.run_action("install")
            self.assertIn("DRY-RUN: would sideload plugin", env_out)
            self.assertEqual(snapshot(self.root), before)
        # The real path still runs once the variable is gone.
        self.run_action("uninstall")
        self.assertFalse(self.target.exists())

    def test_uninstall_does_not_need_app_dependencies_or_source(self) -> None:
        self.run_action("install")
        shutil.rmtree(self.app)
        shutil.rmtree(self.adapters)
        (self.bin / "tokenless").unlink()
        self.run_action("uninstall")
        self.run_action("uninstall")

    def test_foreign_plugin_conflict_never_overwritten(self) -> None:
        self.target.mkdir()
        (self.target / "user.txt").write_text("keep")
        before = snapshot(self.root)
        for action in (("install",), ("install", "--dry-run"), ("uninstall",)):
            _, error = self.run_action(*action, success=False)
            self.assertIn("Foreign plugin conflict", error)
        self.assertEqual(snapshot(self.root), before)

    def test_managed_path_symlinks_rejected(self) -> None:
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "keep").write_text("untouched")
        original = snapshot(outside)
        for component in ("data", "plugins-custom", "tokenless"):
            with self.subTest(component=component):
                data = self.home / ("unsafe-" + component)
                link = data
                if component != "data":
                    data.mkdir()
                    link = data / "plugins-custom"
                if component == "tokenless":
                    link.mkdir()
                    link = link / "tokenless"
                link.symlink_to(outside, target_is_directory=True)
                with mock.patch.dict(os.environ, {"TOKENLESS_QWENWORK_HOME": str(data)}):
                    for action in ("install", "uninstall"):
                        _, error = self.run_action(action, success=False)
                        self.assertIn("symlink", error)
                self.assertEqual(snapshot(outside), original)

    def test_symlink_within_home_ancestor_is_rejected(self) -> None:
        link = self.home / "linked-parent"
        link.symlink_to(self.root, target_is_directory=True)
        with mock.patch.dict(os.environ, {"TOKENLESS_QWENWORK_HOME": str(link / "new-data")}):
            _, error = self.run_action("install", success=False)
            self.assertIn("symlink", error)
        self.assertFalse((self.root / "new-data").exists())

    def test_trusted_home_alias_is_resolved_before_boundary_checks(self) -> None:
        alias = self.root / "home-alias"
        alias.symlink_to(self.home, target_is_directory=True)
        environment = {"HOME": str(alias), "TOKENLESS_QWENWORK_HOME": str(alias / self.data.name)}
        with mock.patch.dict(os.environ, environment):
            self.run_action("install")
            self.assertTrue(self.target.is_dir())
            self.run_action("uninstall")

    def test_external_data_root_with_trusted_parent_alias(self) -> None:
        alias = self.root / "var-alias"
        alias.symlink_to(self.root, target_is_directory=True)
        external = alias / "external-office"
        with mock.patch.dict(os.environ, {"TOKENLESS_QWENWORK_HOME": str(external)}):
            self.run_action("install")
            self.assertTrue((external / "plugins-custom/tokenless").is_dir())
            self.run_action("uninstall")

    def test_user_content_edits_are_preserved(self) -> None:
        self.run_action("install")
        (self.target / "hooks/hooks.json").write_text("user edits")
        self.assert_modified_protected()

    def test_user_added_empty_directory_is_preserved(self) -> None:
        self.run_action("install")
        (self.target / "user-directory").mkdir()
        self.assert_modified_protected()

    def test_user_deleted_file_is_not_replaced(self) -> None:
        self.run_action("install")
        (self.target / "hooks/run-hook.sh").unlink()
        self.assert_modified_protected()

    def test_user_permission_change_is_preserved(self) -> None:
        self.run_action("install")
        path = self.target / "hooks/run-hook.sh"
        path.chmod(stat.S_IMODE(path.stat().st_mode) ^ stat.S_IXUSR)
        self.assert_modified_protected()

    def test_owned_internal_symlink_and_marker_tampering_rejected(self) -> None:
        self.run_action("install")
        path = self.target / "hooks/run-hook.sh"
        path.unlink()
        path.symlink_to(self.source / "hooks/run-hook.sh")
        self.assert_modified_protected()
        path.unlink()
        shutil.copy2(self.source / "hooks/run-hook.sh", path)
        (self.target / lifecycle.MARKER).write_text('{"owner":"another-plugin"}')
        self.assert_modified_protected()

    def test_hardlinked_owned_file_is_not_removed(self) -> None:
        self.run_action("install")
        outside = self.root / "hardlinked-hook"
        os.link(self.target / "hooks/run-hook.sh", outside)
        self.assert_modified_protected()
        self.assertTrue(outside.is_file())

    def test_dangling_target_symlink_and_invalid_marker_are_preserved(self) -> None:
        self.target.symlink_to(self.root / "absent-directory", target_is_directory=True)
        self.assert_modified_protected()
        self.target.unlink()
        self.run_action("install")
        (self.target / lifecycle.MARKER).write_text("not JSON")
        self.assert_modified_protected()

    def test_unexpected_source_symlink_is_rejected(self) -> None:
        (self.source / "hooks/external.py").symlink_to(self.manifest)
        before = snapshot(self.root)
        _, error = self.run_action("install", success=False)
        self.assertIn("source symlink", error)
        self.assertEqual(snapshot(self.root), before)

    def test_staging_inside_source_is_rejected(self) -> None:
        for data in (self.source / "data", self.adapters / "common/hooks/data"):
            environment = {"TOKENLESS_QWENWORK_HOME": str(data)}
            with self.subTest(data=data), mock.patch.dict(os.environ, environment):
                before = snapshot(self.root)
                for action in (("install",), ("install", "--dry-run")):
                    _, error = self.run_action(*action, success=False)
                    self.assertIn("outside plugin source", error)
                self.assertEqual(snapshot(self.root), before)

    def test_staging_requires_same_filesystem_as_scanner(self) -> None:
        original_stat = Path.stat

        def other_device(path, *args, **kwargs):
            value = original_stat(path, *args, **kwargs)
            if path == self.data:
                fields = list(value)
                fields[2] += 1
                return os.stat_result(fields)
            return value

        with mock.patch.object(Path, "stat", autospec=True, side_effect=other_device):
            _, error = self.run_action("install", success=False)
        self.assertIn("share a filesystem", error)
        self.assertFalse(list(self.data.glob(".tokenless-stage-*")))
        self.assertFalse(self.target.exists())

    def test_failed_first_publish_retains_staged_copy(self) -> None:
        with mock.patch.object(lifecycle.os, "replace", side_effect=OSError("publish denied")):
            _, error = self.run_action("install", success=False)
        self.assertFalse(self.target.exists())
        stages = list(self.data.glob(".tokenless-stage-*"))
        self.assertEqual(len(stages), 1)
        self.assertIn(str(stages[0]), error)
        self.assertIsNotNone(lifecycle._owned(stages[0] / "plugin"))

    def test_upgrade_rename_failures_preserve_or_restore_old_plugin(self) -> None:
        self.run_action("install")
        self.upgrade_source()
        before = snapshot(self.target)
        replace = os.replace
        for failed_call in (1, 2):
            calls = []

            def fail_one(source, destination):
                source, destination = Path(source), Path(destination)
                calls.append((source, destination))
                staging_path = destination if source == self.target else source
                self.assertNotIn(self.target.parent, staging_path.parents)
                self.assertEqual(
                    staging_path.parent.stat().st_dev, self.target.parent.stat().st_dev
                )
                if len(calls) == failed_call:
                    raise OSError("injected rename failure")
                return replace(source, destination)

            patcher = mock.patch.object(lifecycle.os, "replace", side_effect=fail_one)
            with self.subTest(failed_call=failed_call), patcher:
                _, error = self.run_action("install", success=False)
            self.assertEqual(snapshot(self.target), before)
            self.assertIn("retained at", error)
            if failed_call == 2:
                self.assertEqual(len(calls), 3)
                self.assertIn("restored", error)

    def test_failed_rollback_reports_preserved_backup(self) -> None:
        self.run_action("install")
        old_entries = lifecycle._owned(self.target)
        self.upgrade_source()
        replace = os.replace
        calls = []

        def fail_publish_and_restore(source, destination):
            calls.append((Path(source), Path(destination)))
            if len(calls) > 1:
                raise OSError("injected publish/rollback failure")
            return replace(source, destination)

        with mock.patch.object(lifecycle.os, "replace", side_effect=fail_publish_and_restore):
            _, error = self.run_action("install", success=False)
        self.assertEqual(len(calls), 3)
        backup = calls[0][1]
        self.assertEqual(lifecycle._owned(backup), old_entries)
        self.assertIn("rollback failed", error)
        self.assertIn(str(backup), error)
        self.assertFalse(self.target.exists())
        self.assertTrue((backup.parent / "plugin").is_dir())

    def test_publish_failure_does_not_overwrite_concurrent_foreign_target(self) -> None:
        self.run_action("install")
        self.upgrade_source()
        replace = os.replace
        calls = []

        def occupy_target(source, destination):
            calls.append((Path(source), Path(destination)))
            if len(calls) == 2:
                self.target.mkdir()
                (self.target / "foreign").write_text("keep")
                raise OSError("injected concurrent target")
            return replace(source, destination)

        with mock.patch.object(lifecycle.os, "replace", side_effect=occupy_target):
            _, error = self.run_action("install", success=False)
        self.assertIn("Rollback destination is occupied", error)
        self.assertEqual((self.target / "foreign").read_text(), "keep")
        self.assertIsNotNone(lifecycle._owned(calls[0][1]))

    def test_wrappers_forward_action_arguments_and_disable_bytecode(self) -> None:
        (self.source / "scripts/lifecycle.py").write_text(
            "import json, sys\nprint(json.dumps([sys.argv[1:], sys.dont_write_bytecode]))\n"
        )
        for action in ("detect", "install", "uninstall"):
            result = subprocess.run(
                ["/bin/bash", str(self.source / f"scripts/{action}.sh"), "--dry-run"],
                env=self.env, capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout), [[action, "--dry-run"], True])
        (self.bin / "python3").unlink()
        result = subprocess.run(
            ["/bin/bash", str(self.source / "scripts/install.sh")],
            env=self.env, capture_output=True, text=True, timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("python3 is required", result.stderr)

    def test_helper_uses_python39_compatible_annotations(self) -> None:
        source = (self.source / "scripts/lifecycle.py").read_text()
        tree = ast.parse(source, feature_version=(3, 9))
        self.assertTrue(any(
            isinstance(node, ast.ImportFrom)
            and node.module == "__future__"
            and any(item.name == "annotations" for item in node.names)
            for node in tree.body
        ))

    def prepare_detached_plugin(self) -> None:
        self.run_action("install")
        self.hooks = json.loads((self.target / "hooks/hooks.json").read_text())["hooks"]
        shutil.rmtree(self.adapters)
        self.assertFalse(self.source.exists())
        self.assertTrue((self.target / "hooks/shared/hook_utils.py").is_file())

    def replay(self, event: str, payload: dict | str) -> dict:
        command = self.hooks[event][0]["hooks"][0]["command"]
        env = {
            **self.env,
            "QODER_PLUGIN_ROOT": str(self.target),
            "TOKENLESS_AGENT_ID": "wrong-agent",
        }
        result = subprocess.run(
            ["/bin/bash", "-c", command],
            input=payload if isinstance(payload, str) else json.dumps(payload),
            env=env, capture_output=True, text=True, timeout=15, cwd=self.root,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def payload(self, response: object, tool: str = "mcp__office__list_rows") -> dict:
        return {
            "tool_name": tool, "tool_response": response, "tool_input": {},
            "session_id": "sandbox", "tool_use_id": "tool-sandbox",
        }

    def test_registered_non_bash_mcp_replaces_output_without_source_or_fhs(self) -> None:
        self.prepare_detached_plugin()
        self.assertEqual(set(self.hooks), {"PreToolUse", "PostToolUse"})
        matcher = self.hooks["PreToolUse"][0]["matcher"]
        for tool in ("Bash", "mcp__workspace__bash", "mcp__qw-builtin__mcp_workspace_bash"):
            self.assertIsNotNone(re.fullmatch(matcher, tool))
        self.assertIsNone(re.fullmatch(matcher, "mcp__office__list_rows"))
        self.assertNotIn("matcher", self.hooks["PostToolUse"][0])
        for event in self.hooks.values():
            self.assertIn("--agent-id qwenwork", event[0]["hooks"][0]["command"])
        response = {"body": "x" * 500, "count": 2}
        for tool in ("mcp__office__list_rows", "OfficeAPI"):
            for value in (response, json.dumps(response)):
                with self.subTest(tool=tool, value_type=type(value).__name__):
                    result = self.replay("PostToolUse", self.payload(value, tool))
                    output = result["hookSpecificOutput"]
                    self.assertEqual(output["hookEventName"], "PostToolUse")
                    self.assertNotIn("additionalContext", output)
                    replacement = output["updatedToolOutput"]
                    self.assertIsInstance(replacement, type(value))
                    if isinstance(replacement, str):
                        replacement = json.loads(replacement)
                    self.assertEqual(replacement, {"body": "x" * 20, "count": 2})
                    request = json.loads((self.bin / "request.json").read_text())
                    self.assertTrue(request["capabilities"]["replace_output"])
                    self.assertEqual(request["content_origin"], "api_response")
        self.assertFalse(list(self.target.rglob("__pycache__")))
        self.assertIsNotNone(lifecycle._owned(self.target))

    def test_registered_post_preserves_file_content(self) -> None:
        self.prepare_detached_plugin()
        for tool in ("Read", "read_file"):
            with self.subTest(tool=tool):
                result = self.replay("PostToolUse", self.payload({"body": "x" * 500}, tool))
                self.assertEqual(result, {})
                request = json.loads((self.bin / "request.json").read_text())
                self.assertEqual(request["content_origin"], "file_content")
        self.assertFalse(list(self.target.rglob("*.pyc")))

    def test_registered_shell_output_replacement(self) -> None:
        self.prepare_detached_plugin()
        _create_mock_tokenless(str(self.bin), behavior="compress-text")
        response = {"stdout": "plain shell log " * 200, "stderr": "", "exit_code": 0}
        result = self.replay("PostToolUse", self.payload(response, "Bash"))
        self.assertEqual(
            result["hookSpecificOutput"]["updatedToolOutput"],
            {**response, "stdout": response["stdout"][:40]},
        )

    def use_contract_mock(self) -> None:
        shutil.copy2(CONTRACT_MOCK, self.bin / "tokenless")
        (self.bin / "tokenless").chmod(0o755)
        self.env["TOKENLESS_MOCK_REQUEST_LOG"] = str(self.root / "requests.jsonl")

    def test_workspace_shell_aliases_rewrite_preserve_arguments_and_deduplicate(self) -> None:
        self.prepare_detached_plugin()
        self.use_contract_mock()
        for tool in ("mcp__workspace__bash", "mcp__qw-builtin__mcp_workspace_bash"):
            with self.subTest(tool=tool):
                payload = self.payload("", tool)
                payload["cwd"] = str(self.root)
                payload["tool_input"] = {
                    "command": "git status",
                    "dir_path": str(self.root),
                    "timeout": 30,
                    "sandbox_permissions": "use_default",
                }
                output = self.replay("PreToolUse", payload)["hookSpecificOutput"]
                self.assertEqual(
                    output["updatedInput"],
                    {
                        **payload["tool_input"],
                        "command": "/mock/rtk git status",
                    },
                )
                self.assertTrue(list((self.root / ".tokenless/hook-state").glob("*")))
                self.assertFalse((self.home / ".tokenless").exists())
                payload["tool_response"] = [
                    {"type": "text", "text": '{"body":"' + "x" * 500 + '"}'}
                ]
                self.assertEqual(self.replay("PostToolUse", payload), {})
                request = json.loads((self.root / "requests.jsonl").read_text().splitlines()[-1])
                self.assertEqual(request["attribution"]["agent_id"], "qwenwork")
                self.assertEqual(request["input"]["output_optimization"], "rtk")
                self.assertEqual(request["input"]["content_origin"], "command_output")

    def test_workspace_text_blocks_use_text_slot_and_preserve_failure(self) -> None:
        self.prepare_detached_plugin()
        _create_mock_tokenless(str(self.bin), behavior="compress-text")
        for tool in ("mcp__workspace__bash", "mcp__qw-builtin__mcp_workspace_bash"):
            with self.subTest(tool=tool):
                text = "plain shell log " * 200
                payload = self.payload([{"type": "text", "text": text}], tool)
                payload["cwd"] = str(self.root)
                payload["tool_input"] = {"command": "python3 report.py"}
                result = self.replay("PostToolUse", payload)
                self.assertEqual(result["hookSpecificOutput"]["updatedToolOutput"], text[:40])
                request = json.loads((self.bin / "request.json").read_text())
                self.assertEqual(request["content"], text)
                self.assertEqual(request["tool_name"], "Bash")
                self.assertEqual(request["capabilities"]["recovery"], {"kind": "none"})
                self.assertTrue(request["capabilities"]["replace_with_text"])
                payload["is_error"] = True
                self.assertEqual(self.replay("PostToolUse", payload), {})

    def test_workspace_unknown_blocks_and_invalid_cwd_pass_through(self) -> None:
        self.prepare_detached_plugin()
        payload = self.payload([], "mcp__qw-builtin__mcp_workspace_bash")
        payload["cwd"] = str(self.root)
        text = {"type": "text", "text": "log " * 500}
        for blocks in (
            [],
            [text, text],
            [{**text, "annotations": {}}],
            [{"type": "image", "data": "image"}],
            [{"type": "text", "text": {}}],
        ):
            payload["tool_response"] = blocks
            self.assertEqual(self.replay("PostToolUse", payload), {})
        payload["tool_response"] = [text]
        for cwd in (None, "relative", str(self.root / "missing")):
            payload["cwd"] = cwd
            self.assertEqual(self.replay("PostToolUse", payload), {})
        self.assertFalse((self.bin / "spawn_log").exists())

    def test_private_binaries_are_used_without_changing_native_bash_output_shape(self) -> None:
        self.prepare_detached_plugin()
        private_bin = self.target / "bin"
        private_bin.mkdir()
        _create_mock_tokenless(str(private_bin), behavior="compress-text")
        response = {"stdout": "plain shell log " * 200, "stderr": "", "exit_code": 0}
        result = self.replay("PostToolUse", self.payload(response, "Bash"))
        self.assertEqual(
            result["hookSpecificOutput"]["updatedToolOutput"],
            {
                **response,
                "stdout": response["stdout"][:40],
            },
        )
        self.assertFalse((self.bin / "spawn_log").exists())
        request = json.loads((private_bin / "request.json").read_text())
        self.assertEqual(request["capabilities"]["recovery"], {"kind": "none"})

    def test_registered_array_and_pretool_attribution(self) -> None:
        self.prepare_detached_plugin()
        self.use_contract_mock()
        result = self.replay("PostToolUse", self.payload([{"body": "x" * 500}]))
        self.assertEqual(result["hookSpecificOutput"]["updatedToolOutput"], [{"body": "x" * 20}])
        payload = {
            "tool_name": "Bash",
            "tool_input": {"command": "git status", "description": "keep"},
            "session_id": "sandbox", "tool_use_id": "pre-sandbox",
        }
        result = self.replay("PreToolUse", payload)
        self.assertEqual(
            result["hookSpecificOutput"]["updatedInput"],
            {"command": "/mock/rtk git status", "description": "keep"},
        )
        requests = [
            json.loads(line) for line in (self.root / "requests.jsonl").read_text().splitlines()
        ]
        self.assertEqual([request["operation"] for request in requests], ["post_tool", "pre_tool"])
        for request in requests:
            self.assertEqual(request["attribution"]["agent_id"], "qwenwork")
        # The persisted RTK state must prevent a second transformation.
        post = self.payload({"body": "x" * 500}, "Bash")
        post["tool_use_id"] = "pre-sandbox"
        self.assertEqual(self.replay("PostToolUse", post), {})

    def test_dispatcher_fails_open_on_linux_invalid_input_and_missing_resources(self) -> None:
        self.prepare_detached_plugin()
        payload = self.payload({"body": "x" * 500})
        self.executable("uname", "#!/bin/sh\nprintf '%s\\n' Linux\n")
        for event in ("PreToolUse", "PostToolUse"):
            self.assertEqual(self.replay(event, payload), {})
        self.assertFalse((self.bin / "spawn_log").exists())
        self.executable("uname", "#!/bin/sh\nprintf '%s\\n' Darwin\n")
        self.assertEqual(self.replay("PostToolUse", "not json"), {})
        for argument in ("../shared/hook_utils.py", "compress_schema_hook.py", ""):
            result = subprocess.run(
                ["/bin/bash", str(self.target / "hooks/run-hook.sh"), argument],
                input="{}", env=self.env, capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout), {})
        (self.bin / "python3").unlink()
        self.assertEqual(self.replay("PostToolUse", payload), {})
        (self.bin / "python3").symlink_to(sys.executable)
        shutil.rmtree(self.target / "hooks/shared")
        self.assertEqual(self.replay("PostToolUse", payload), {})
        self.assertFalse(
            (self.bin / "spawn_log").exists(),
            "missing local hooks must not fall back to FHS/source",
        )


if __name__ == "__main__":
    unittest.main()
