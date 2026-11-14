#!/usr/bin/env python3
"""Sandbox-only lifecycle and registered-command tests for QwenWorkCN."""

from __future__ import annotations

import sys

sys.dont_write_bytecode = True

import ast
import contextlib
import errno
import importlib.util
import io
import json
import os
import plistlib
import pwd
import re
import shlex
import shutil
import stat
import subprocess
import tempfile
import unittest
import uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path, PosixPath, PureWindowsPath
from types import SimpleNamespace
from typing import Any
from unittest import mock

from test_compress_response_hook import _create_mock_tokenless

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "adapters/tokenless/qwenwork"
COMMON = ROOT / "adapters/tokenless/common/hooks"
CONTRACT_MOCK = ROOT / "tests/contract/mock_tokenless.py"
SPEC = importlib.util.spec_from_file_location("qwenwork_lifecycle", SOURCE / "scripts/lifecycle.py")
lifecycle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(lifecycle)
sys.path.insert(0, str(COMMON))
sys.path.insert(0, str(COMMON.parent.parent))
import hook_utils as utils
import hook_dispatch

qwenwork_adapter = hook_dispatch.load_adapter("qwenwork")
claude_adapter = hook_dispatch.load_adapter("claude-code")
REAL_BIN = os.environ.get("TOKENLESS_TEST_BIN")
WORKSPACE_SHELL_TOOLS = (
    "mcp__workspace__bash",
    "mcp__qw-builtin__mcp_workspace_bash",
    "mcp__qw-builtin__mcp__workspace__bash",
)


def _quote_ps(value: str) -> str:
    """Mirror the adapter's PowerShell quoting for command-shape assertions."""
    return "'" + value.replace("'", "''") + "'"


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
        self.os_home = self.root / "system user home"
        self.os_home.mkdir()
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
        # Isolate passwd-backed state in child Hooks without equating it to HOME.
        (self.bin / "sitecustomize.py").write_text(
            "import pwd\nfrom types import SimpleNamespace\n"
            f"pwd.getpwuid = lambda uid: SimpleNamespace(pw_dir={str(self.os_home)!r})\n"
        )
        (self.bin / "python3").symlink_to(sys.executable)
        (self.bin / "bash").symlink_to("/bin/bash")
        self.executable("uname", "#!/bin/sh\nprintf '%s\\n' Darwin\n")
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
            "PYTHONPATH": str(self.bin),
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
            mock.patch.object(pwd, "getpwuid", return_value=SimpleNamespace(pw_dir=str(self.os_home))),
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

    def test_data_directory_notice_is_read_only_and_preserves_environment(self) -> None:
        custom = str(self.root / "custom data '$;`")
        for configured in (None, "", custom, "relative-data"):
            with self.subTest(configured=configured), mock.patch.dict(os.environ):
                if configured is None:
                    os.environ.pop("TOKENLESS_DATA_DIR", None)
                else:
                    os.environ["TOKENLESS_DATA_DIR"] = configured
                before = snapshot(self.root)
                for action in (("detect",), ("install", "--dry-run")):
                    out, error = self.run_action(*action)
                    directory = configured or str(self.os_home / ".tokenless")
                    source = "TOKENLESS_DATA_DIR" if configured else "default"
                    if configured == "relative-data":
                        self.assertIn("unavailable; configured value: 'relative-data'", out)
                    else:
                        self.assertIn(f"{directory} ({source})", out)
                    self.assertIn("does not test sandbox write access", out)
                    self.assertIn("desktop process environment", out)
                    self.assertIn("commands can still run", out)
                    self.assertIn("Hook-side compression statistics", out)
                    self.assertIn("RTK's native database", out)
                    self.assertEqual("must be an absolute path" in error, configured == "relative-data")
                    self.assertEqual(os.environ.get("TOKENLESS_DATA_DIR"), configured)
                self.assertEqual(snapshot(self.root), before)

    def test_install_shows_notice_without_creating_data_directory(self) -> None:
        directory = self.root / "configured data"
        with mock.patch.dict(os.environ, {"TOKENLESS_DATA_DIR": str(directory)}):
            out, _ = self.run_action("install")
        self.assertIn(f"{directory} (TOKENLESS_DATA_DIR)", out)
        self.assertIn("RTK statistics and execution receipts may be missing", out)
        self.assertFalse(directory.exists())

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

    def test_unsupported_platform_rejects_install(self) -> None:
        before = snapshot(self.root)
        with mock.patch.object(lifecycle.platform, "system", return_value="Linux"):
            for action in (("detect",), ("install",), ("install", "--dry-run")):
                _, error = self.run_action(*action, success=False)
                self.assertIn("supports macOS and Windows only", error)
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
        self.assertEqual(set(WORKSPACE_SHELL_TOOLS), set(qwenwork_adapter._QWENWORK_WORKSPACE_SHELLS))
        self.assertLessEqual({"Bash", *WORKSPACE_SHELL_TOOLS}, set(matcher.split("|")))
        for tool in ("Bash", *WORKSPACE_SHELL_TOOLS):
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
            response["stdout"][:40],
        )

    def use_contract_mock(self) -> None:
        shutil.copy2(CONTRACT_MOCK, self.bin / "tokenless")
        (self.bin / "tokenless").chmod(0o755)
        self.env["TOKENLESS_MOCK_REQUEST_LOG"] = str(self.root / "requests.jsonl")

    def test_workspace_shell_aliases_leave_other_agents_unchanged(self) -> None:
        before = snapshot(self.root)
        for tool in WORKSPACE_SHELL_TOOLS:
            with self.subTest(tool=tool):
                payload = self.payload([{"type": "text", "text": '{"count": 2}'}], tool)
                payload["cwd"] = str(self.root)
                self.assertIs(claude_adapter.prepare_input(payload), payload)
        self.assertEqual(snapshot(self.root), before)

    def test_workspace_shell_aliases_rewrite_preserve_arguments_and_deduplicate(self) -> None:
        self.prepare_detached_plugin()
        self.use_contract_mock()
        for tool in WORKSPACE_SHELL_TOOLS:
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
                self.assertTrue(list((self.os_home / ".tokenless/hook-state").glob("*")))
                self.assertFalse((self.root / ".tokenless").exists())
                payload["tool_response"] = [
                    {"type": "text", "text": '{"body":"' + "x" * 500 + '"}'}
                ]
                self.assertEqual(self.replay("PostToolUse", payload), {})
                request = json.loads((self.root / "requests.jsonl").read_text().splitlines()[-1])
                self.assertEqual(request["attribution"]["agent_id"], "qwenwork")
                self.assertEqual(request["input"]["output_optimization"], "rtk")
                self.assertEqual(request["input"]["content_origin"], "command_output")

    def test_workspace_data_directory_preserves_environment_across_cwds(self) -> None:
        self.prepare_detached_plugin()
        self.use_contract_mock()
        mock_path = self.bin / "tokenless"
        mock_path.rename(self.bin / "contract.py")
        self.executable("tokenless", """#!/usr/bin/env python3
import json
import os
import runpy
from pathlib import Path

with (Path(__file__).parent / "data-dir.json").open("w") as capture:
    json.dump(os.environ.get("TOKENLESS_DATA_DIR"), capture)
runpy.run_path(str(Path(__file__).parent / "contract.py"), run_name="__main__")
""")
        other_cwd = self.root / "other workspace"
        other_cwd.mkdir()
        custom = self.root / "custom data '$;`"
        for tool in WORKSPACE_SHELL_TOOLS:
            for override in (None, "", str(custom)):
                with self.subTest(tool=tool, override=override):
                    self.env.pop("TOKENLESS_DATA_DIR", None)
                    if override is not None:
                        self.env["TOKENLESS_DATA_DIR"] = override
                    state_dir = custom if override else self.os_home / ".tokenless"
                    payload = self.payload("", tool)
                    payload["cwd"] = str(self.root)
                    payload["tool_input"] = {"command": "git status"}
                    result = self.replay("PreToolUse", payload)
                    self.assertIn("hookSpecificOutput", result)
                    self.assertEqual(json.loads((self.bin / "data-dir.json").read_text()), override)
                    self.assertTrue(list((state_dir / "hook-state").glob("*")))
                    payload["cwd"] = str(other_cwd)
                    payload["tool_response"] = [{"type": "text", "text": "log " * 500}]
                    self.assertEqual(self.replay("PostToolUse", payload), {})
                    self.assertEqual(json.loads((self.bin / "data-dir.json").read_text()), override)
                    request = json.loads((self.root / "requests.jsonl").read_text().splitlines()[-1])
                    self.assertEqual(request["input"]["output_optimization"], "rtk")
                    self.assertFalse(list((state_dir / "hook-state").glob("*")))
                    self.assertFalse((self.root / ".tokenless").exists())
                    self.assertFalse((other_cwd / ".tokenless").exists())
                    self.assertFalse((self.home / ".tokenless").exists())

    def test_workspace_invalid_data_directories_pass_through_without_writes(self) -> None:
        self.prepare_detached_plugin()
        regular = self.root / "regular-file"
        regular.write_text("keep")
        broken = self.root / "broken-link"
        broken.symlink_to(self.root / "missing-target")
        root_link = self.root / "root-link"
        root_link.symlink_to("/")
        loop = self.root / "loop"
        loop.symlink_to(loop)
        for directory in (
            "/",
            str(root_link),
            str(regular),
            str(regular / "child"),
            str(self.root / "missing/../state"),
            str(broken / "state"),
            str(loop),
        ):
            with self.subTest(directory=directory):
                self.env["TOKENLESS_DATA_DIR"] = directory
                payload = self.payload(
                    [{"type": "text", "text": "log " * 500}], "mcp__workspace__bash"
                )
                payload["cwd"] = str(self.root)
                payload["tool_input"] = {"command": "git status"}
                before = snapshot(self.root)
                for event in ("PreToolUse", "PostToolUse"):
                    self.assertEqual(self.replay(event, payload), {})
                with mock.patch.dict(os.environ, {"TOKENLESS_DATA_DIR": directory}):
                    for action in (("detect",), ("install", "--dry-run")):
                        # Detached source is deliberately absent; inspect the installed copy.
                        with mock.patch.object(lifecycle, "PLUGIN_ROOT", self.target):
                            out, error = self.run_action(*action)
                        self.assertIn("unavailable", out)
                        self.assertIn("hooks pass through", error)
                self.assertEqual(snapshot(self.root), before)
                self.assertFalse((self.bin / "spawn_log").exists())

    def test_missing_trusted_home_never_falls_back_to_home_or_cwd(self) -> None:
        for home in ("", "/", "relative-home", None):
            with self.subTest(home=home):
                patch = (
                    mock.patch.object(pwd, "getpwuid", side_effect=KeyError("missing user"))
                    if home is None
                    else mock.patch.object(
                        pwd, "getpwuid", return_value=SimpleNamespace(pw_dir=home)
                    )
                )
                with patch:
                    before = snapshot(self.root)
                    out, error = self.run_action("detect")
                    self.assertIn("unavailable", out)
                    self.assertIn("no trusted home directory", error)
                    for tool in WORKSPACE_SHELL_TOOLS:
                        with contextlib.redirect_stdout(io.StringIO()) as output:
                            with contextlib.redirect_stderr(io.StringIO()):
                                with self.assertRaises(SystemExit) as stopped:
                                    qwenwork_adapter.prepare_input(
                                        {"tool_name": tool, "cwd": str(self.root)}
                                    )
                        self.assertEqual(stopped.exception.code, 0)
                        self.assertEqual(json.loads(output.getvalue()), {})
                    self.assertEqual(snapshot(self.root), before)
                    with mock.patch.dict(
                        os.environ, {"TOKENLESS_DATA_DIR": str(self.root / "explicit")}
                    ):
                        self.assertEqual(
                            utils.resolve_data_directory(), str(self.root / "explicit")
                        )

    def test_data_directory_resolves_symlink_prefix_without_writes(self) -> None:
        link = self.root / "directory-link"
        link.symlink_to(self.os_home)
        with mock.patch.dict(os.environ, {"TOKENLESS_DATA_DIR": str(link / "new/state")}):
            before = snapshot(self.root)
            self.assertEqual(utils.resolve_data_directory(), str(self.os_home / "new/state"))
            out, _ = self.run_action("detect")
            self.assertIn(str(self.os_home / "new/state"), out)
            self.assertEqual(snapshot(self.root), before)

    def test_workspace_relative_data_directory_does_not_write_to_cwd(self) -> None:
        self.prepare_detached_plugin()
        self.env["TOKENLESS_DATA_DIR"] = "relative-data"
        payload = self.payload([{"type": "text", "text": "log " * 500}], "mcp__workspace__bash")
        payload["cwd"] = str(self.root)
        payload["tool_input"] = {"command": "git status"}
        for event in ("PreToolUse", "PostToolUse"):
            self.assertEqual(self.replay(event, payload), {})
        self.assertFalse((self.root / "relative-data").exists())
        self.assertFalse((self.root / ".tokenless").exists())
        self.assertFalse((self.home / ".tokenless").exists())
        self.assertFalse((self.bin / "spawn_log").exists())

    def test_workspace_text_blocks_use_text_slot_and_preserve_failure(self) -> None:
        self.prepare_detached_plugin()
        _create_mock_tokenless(str(self.bin), behavior="compress-text")
        for tool in WORKSPACE_SHELL_TOOLS:
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

    def test_workspace_json_blocks_compress_body(self) -> None:
        self.prepare_detached_plugin()
        body = json.dumps({"body": "数据" * 250, "count": 2}, ensure_ascii=False)
        for tool in WORKSPACE_SHELL_TOOLS:
            with self.subTest(tool=tool):
                payload = self.payload([{"type": "text", "text": body}], tool)
                payload["cwd"] = str(self.root)
                payload["tool_input"] = {"command": "python3 report.py"}
                result = self.replay("PostToolUse", payload)
                replacement = result["hookSpecificOutput"]["updatedToolOutput"]
                self.assertIsInstance(replacement, str)
                self.assertEqual(json.loads(replacement), {"body": "数据" * 10, "count": 2})
                request = json.loads((self.bin / "request.json").read_text())
                self.assertEqual(request["content"], body)
                self.assertEqual(request["tool_name"], "Bash")
                self.assertEqual(request["content_origin"], "command_output")
                self.assertEqual(request["command"], "python3 report.py")
                self.assertTrue(request["capabilities"]["replace_with_text"])

    def test_workspace_unknown_blocks_and_invalid_cwd_pass_through(self) -> None:
        self.prepare_detached_plugin()
        text = {"type": "text", "text": "log " * 500}
        for tool in WORKSPACE_SHELL_TOOLS:
            with self.subTest(tool=tool):
                payload = self.payload([], tool)
                payload["cwd"] = str(self.root)
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
            response["stdout"][:40],
        )
        self.assertFalse((self.bin / "spawn_log").exists())
        request = json.loads((private_bin / "request.json").read_text())
        self.assertEqual(
            request["capabilities"]["recovery"],
            # Shell recovery is now advertised on every host with a complete
            # private bridge; Linux here proves the bridge alone is not
            # macOS-gated.
            {"kind": "shell"},
        )

    @unittest.skipUnless(sys.platform == "darwin", "macOS private recovery bridge")
    def test_private_recovery_without_global_path_and_concurrent_attribution(self) -> None:
        self.prepare_detached_plugin()
        private = self.target / "bin/tokenless"
        private.parent.mkdir()
        private.write_text(
            "#!/usr/bin/env python3\nimport json, os, sys\n"
            "if sys.argv[1] == 'retrieve':\n"
            " print(json.dumps({'hash':sys.argv[2], 'env':{k:v for k,v in os.environ.items() "
            "if k.startswith('TOKENLESS_') or k == 'DSH_TOKENLESS_STASH_DB'}}))\n"
            "else:\n"
            f" os.execv(sys.executable, [sys.executable, {str(CONTRACT_MOCK)!r}, *sys.argv[1:]])\n"
        )
        private.chmod(0o755)
        (self.bin / "tokenless").unlink()
        data = self.root / "custom state '$;`"
        self.env.update(TOKENLESS_DATA_DIR=str(data), TOKENLESS_STASH_DB=str(data / "selected.db"))

        def recover(index: int) -> tuple[dict, dict]:
            key = f"{index:024x}"
            payload = self.payload(
                "original " * 400, "Bash" if index == 0 else WORKSPACE_SHELL_TOOLS[index - 1]
            )
            payload.update(
                cwd=str(self.root), session_id=f"session-{index}", tool_use_id=f"call-{index}"
            )
            payload["tool_input"] = {"command": f"tokenless retrieve '{key}'", "timeout": 30}
            rewritten = self.replay("PreToolUse", payload)["hookSpecificOutput"]["updatedInput"]
            self.assertEqual(rewritten["timeout"], 30)
            result = subprocess.run(
                ["/bin/bash", "-c", rewritten["command"]],
                capture_output=True,
                text=True,
                env={**self.env, "TOKENLESS_DATA_DIR": str(self.root / "wrong")},
                check=True,
            )
            payload["tool_input"] = rewritten
            self.assertEqual(self.replay("PostToolUse", payload), {})
            return json.loads(result.stdout), payload

        with ThreadPoolExecutor(max_workers=4) as pool:
            results = list(pool.map(recover, range(4)))
        for index, (result, _) in enumerate(results):
            self.assertEqual(result["hash"], f"{index:024x}")
            self.assertEqual(result["env"]["TOKENLESS_DATA_DIR"], str(data))
            self.assertEqual(result["env"]["TOKENLESS_STASH_DB"], str(data / "selected.db"))
            self.assertEqual(result["env"]["DSH_TOKENLESS_STASH_DB"], "")
            self.assertEqual(result["env"]["TOKENLESS_SESSION_ID"], f"session-{index}")
            self.assertEqual(result["env"]["TOKENLESS_TOOL_USE_ID"], f"call-{index}")
            self.assertEqual(result["env"]["TOKENLESS_AGENT_ID"], "qwenwork")

    def test_private_recovery_requires_complete_bridge_and_exact_command(self) -> None:
        self.prepare_detached_plugin()
        private = self.target / "bin/tokenless"
        private.parent.mkdir()
        shutil.copy2(self.bin / "tokenless", private)
        key = "abcdef0123456789abcdef01"
        payload = self.payload("", "Bash")
        with mock.patch.dict(os.environ, {"QODER_PLUGIN_ROOT": str(self.target)}):
            self.addCleanup(setattr, qwenwork_adapter, "_QWENWORK_RECOVERY", None)
            qwenwork_adapter.prepare_input(payload)
            self.assertTrue(qwenwork_adapter.recovery_available())
            for command in (
                f"tokenless retrieve {key}",
                f"tokenless retrieve '{key.upper()}'",
                f"tokenless retrieve '<<tokenless:{key}>>'",
            ):
                updated = qwenwork_adapter.rewrite_qwenwork_retrieve("Bash", {"command": command})
                self.assertIsNotNone(updated)
                self.assertTrue(qwenwork_adapter.is_retrieve_result("Bash", updated))
            for command in (
                "tokenless retrieve bad",
                f"tokenless retrieve {key}; echo extra",
                f"tokenless retrieve {key} | cat",
                f"tokenless retrieve {key} > out",
                "tokenless retrieve $(id)",
                f"tokenless retrieve {key} --stash-db other.db",
                updated["command"] + " | cat",
            ):
                self.assertIsNone(qwenwork_adapter.rewrite_qwenwork_retrieve("Bash", {"command": command}))
                self.assertFalse(qwenwork_adapter.is_retrieve_result("Bash", {"command": command}))
            with mock.patch.object(qwenwork_adapter.os, "name", "nt"):
                # Without the managed-engine compatibility flag, Windows keeps
                # the POSIX rewrite: the legacy engine runs Git Bash, whose
                # /usr/bin/env prefix works. The binary name changes to .exe.
                with mock.patch.dict(os.environ, {"QWENWORK_PLUGIN_HOOK_COMPATIBILITY": ""}):
                    shutil.copy2(self.bin / "tokenless", self.target / "bin/tokenless.exe")
                    # A POSIX interpreter cannot query a process token; the
                    # configured data directory also mirrors the live
                    # installation, which always sets one.
                    with mock.patch.dict(
                        os.environ, {"TOKENLESS_DATA_DIR": str(self.root / "win databases")}
                    ), mock.patch.object(
                        qwenwork_adapter, "Path", PosixPath
                    ), mock.patch.object(qwenwork_adapter.hook_utils, "Path", PosixPath):
                        qwenwork_adapter.prepare_input(payload)
                    self.assertTrue(qwenwork_adapter.recovery_available())
                    self.assertTrue(
                        qwenwork_adapter._QWENWORK_RECOVERY["binary"].endswith("tokenless.exe"),
                        qwenwork_adapter._QWENWORK_RECOVERY["binary"],
                    )
                    legacy = qwenwork_adapter.rewrite_qwenwork_retrieve(
                        "Bash", {"command": f"tokenless retrieve {key}"}
                    )
                    self.assertIsNotNone(legacy)
                    self.assertTrue(legacy["command"].startswith("/usr/bin/env TOKENLESS_AGENT_ID="))
                    self.assertTrue(
                        legacy["command"].endswith(" retrieve " + key),
                        legacy["command"],
                    )
                    self.assertTrue(qwenwork_adapter.is_retrieve_result("Bash", legacy))
            with mock.patch.object(qwenwork_adapter.os, "name", "nt"):
                # The managed Windows engine calls its shell "Bash" but runs
                # PowerShell; the rewrite must use the PowerShell contract.
                with mock.patch.dict(
                    os.environ,
                    {"QWENWORK_PLUGIN_HOOK_COMPATIBILITY": "managed-tool-hooks-v1"},
                ):
                    shutil.copy2(self.bin / "tokenless", self.target / "bin/tokenless.exe")
                    # A POSIX interpreter cannot query a process token; the
                    # configured data directory also mirrors the live
                    # installation, which always sets one.
                    with mock.patch.dict(
                        os.environ, {"TOKENLESS_DATA_DIR": str(self.root / "win databases")}
                    ), mock.patch.object(qwenwork_adapter, "Path", PosixPath), mock.patch.object(
                        qwenwork_adapter.hook_utils, "Path", PosixPath
                    ):
                        qwenwork_adapter.prepare_input(payload)
                    self.assertTrue(qwenwork_adapter.recovery_available())
                    managed = qwenwork_adapter.rewrite_qwenwork_retrieve(
                        "Bash", {"command": f"tokenless retrieve {key}"}
                    )
                    self.assertIsNotNone(managed)
                    self.assertIn("$env:TOKENLESS_AGENT_ID = ", managed["command"])
                    self.assertIn("finally {", managed["command"])
                    self.assertIn(
                        f"& {_quote_ps(qwenwork_adapter._QWENWORK_RECOVERY['binary'])} retrieve {key}",
                        managed["command"],
                    )
                    self.assertNotIn("/usr/bin/env", managed["command"])
                    self.assertTrue(qwenwork_adapter.is_retrieve_result("Bash", managed))
                    # Pipelines and extra arguments stay unrecognized on Windows.
                    for command in (
                        f"tokenless retrieve {key} | cat",
                        managed["command"] + " | cat",
                        managed["command"] + "; echo extra",
                    ):
                        self.assertIsNone(qwenwork_adapter.rewrite_qwenwork_retrieve("Bash", {"command": command}))
                        self.assertFalse(qwenwork_adapter.is_retrieve_result("Bash", {"command": command}))
                    # Paths with spaces and shell-significant characters survive quoting.
                    spaced = self.target / "bin/tokenless state '$;`"
                    spaced.parent.mkdir(exist_ok=True)
                    shutil.copy2(self.bin / "tokenless", spaced)
                    # The POSIX interpreter cannot instantiate WindowsPath for the
                    # plugin-root arithmetic; a POSIX Path keeps the sandbox layout
                    # while the environment still carries the real values.
                    with mock.patch.dict(
                        os.environ,
                        {
                            "TOKENLESS_STASH_DB": str(self.target / "custom '$;` db"),
                            "QODER_PLUGIN_ROOT": str(self.target),
                            "TOKENLESS_DATA_DIR": str(self.root / "win databases"),
                        },
                    ), mock.patch.object(
                        qwenwork_adapter, "Path", PosixPath
                    ), mock.patch.object(qwenwork_adapter.hook_utils, "Path", PosixPath):
                        qwenwork_adapter.prepare_input(payload)
                        quoted = qwenwork_adapter.rewrite_qwenwork_retrieve(
                            "Bash", {"command": f"tokenless retrieve '{key.upper()}'"}
                        )
                        self.assertIsNotNone(quoted)
                        self.assertIn("$env:TOKENLESS_STASH_DB = ", quoted["command"])
                        self.assertIn("custom ''$;` db';", quoted["command"])
                        self.assertTrue(qwenwork_adapter.is_retrieve_result("Bash", quoted))
            hooks = self.target / "hooks/hooks.json"
            configuration = json.loads(hooks.read_text())
            configuration["hooks"]["PreToolUse"] = []
            hooks.write_text(json.dumps(configuration))
            qwenwork_adapter.prepare_input(payload)
            self.assertFalse(qwenwork_adapter.recovery_available())
            self.assertIsNone(
                qwenwork_adapter.rewrite_qwenwork_retrieve("Bash", {"command": f"tokenless retrieve {key}"})
            )

    def test_windows_installed_hook_shape_enables_the_bridge_and_rewrites_powershell(self) -> None:
        # The Windows installer rewrites every registered hook command into
        # the PowerShell form `& '<python>' -X utf8 -B
        # "${env:QODER_PLUGIN_ROOT}/hooks/shared/rewrite_hook.py"` and adds
        # PowerShell to the PreToolUse matcher (lifecycle.py's Windows
        # branch). Both shapes must enable the bridge, and a PowerShell
        # event must receive the PowerShell retrieve form with or without
        # the managed flag — the same signal the RTK rewrite keys on.
        self.prepare_detached_plugin()
        # `_QWENWORK_RECOVERY` is a module global shared across tests; reset
        # it so a later test cannot inherit state pointing into this sandbox.
        self.addCleanup(setattr, qwenwork_adapter, "_QWENWORK_RECOVERY", None)
        private_bin = self.target / "bin"
        private_bin.mkdir(exist_ok=True)
        _create_mock_tokenless(str(private_bin))
        (self.target / "bin/tokenless.exe").write_text(
            "#!/usr/bin/env python3\nprint('win')\n"
        )
        os.chmod(self.target / "bin/tokenless.exe", 0o755)
        key = "abcdef0123456789abcdef01"
        payload = {
            "tool_name": "Bash",
            "tool_response": "", "tool_input": {"command": "echo hi"},
            "session_id": "win-session", "cwd": str(self.root),
        }
        windows_hooks = json.loads((self.target / "hooks/hooks.json").read_text())
        for event, groups in windows_hooks["hooks"].items():
            for group in groups:
                if event == "PreToolUse":
                    group["matcher"] = f"{group['matcher']}|PowerShell"
                script = (
                    "rewrite_hook.py" if event == "PreToolUse" else "compress_response_hook.py"
                )
                python = sys.executable.replace("'", "''")
                for hook in group["hooks"]:
                    hook["shell"] = "powershell"
                    hook["command"] = (
                        f"& '{python}' -X utf8 -B "
                        f'"${{env:QODER_PLUGIN_ROOT}}/hooks/shared/{script}" --agent-id qwenwork'
                    )
        (self.target / "hooks/hooks.json").write_text(
            json.dumps(windows_hooks, indent=2) + "\n", encoding="utf-8"
        )
        with mock.patch.object(
            qwenwork_adapter, "Path", PosixPath
        ), mock.patch.object(qwenwork_adapter.hook_utils, "Path", PosixPath), mock.patch.object(
            qwenwork_adapter.os, "name", "nt"
        ), mock.patch.dict(
            os.environ,
            {
                "TOKENLESS_DATA_DIR": str(self.root / "win databases"),
                "QODER_PLUGIN_ROOT": str(self.target),
            },
        ):
            qwenwork_adapter.prepare_input(
                {**payload, "tool_use_id": "call-win", "tool_input": {}}
            )
            self.assertTrue(
                qwenwork_adapter.recovery_available(),
                "the installed Windows hooks.json shape must enable the bridge",
            )
            # A native PowerShell event (managed flag absent) still gets the
            # PowerShell command form, matching the RTK rewrite contract.
            for tool, expected_shell in (("PowerShell", "$env:"), ("Bash", "/usr/bin/env")):
                with self.subTest(tool=tool):
                    rewritten = qwenwork_adapter.rewrite_qwenwork_retrieve(
                        tool, {"command": f"tokenless retrieve {key}"}
                    )
                    self.assertIsNotNone(rewritten)
                    self.assertIn(expected_shell, rewritten["command"])
                    self.assertNotIn(
                        "/usr/bin/env" if tool == "PowerShell" else "$env:",
                        rewritten["command"],
                    )
                    self.assertTrue(qwenwork_adapter.is_retrieve_result(tool, rewritten))
            # With the managed flag, a Bash event renames to PowerShell and
            # the rewrite follows.
            with mock.patch.dict(
                os.environ, {"QWENWORK_PLUGIN_HOOK_COMPATIBILITY": "managed-tool-hooks-v1"}
            ):
                name, rewritten = qwenwork_adapter.pre_tool_input(
                    {**payload, "tool_input": {"command": f"tokenless retrieve {key}"}}
                )
                self.assertEqual(name, "PowerShell")
                self.assertIsNotNone(rewritten)
                self.assertIn("$env:TOKENLESS_AGENT_ID = ", rewritten["command"])
                self.assertIn("finally {", rewritten["command"])
                self.assertTrue(
                    qwenwork_adapter.is_retrieve_result("PowerShell", rewritten)
                )
        # The Windows bridge does not require run-hook.sh: the installer
        # rewrites hook commands to call Python directly under PowerShell,
        # so the POSIX launcher is never executed there.
        (self.target / "hooks/run-hook.sh").unlink()
        with mock.patch.object(
            qwenwork_adapter, "Path", PosixPath
        ), mock.patch.object(qwenwork_adapter.hook_utils, "Path", PosixPath), mock.patch.object(
            qwenwork_adapter.os, "name", "nt"
        ), mock.patch.dict(
            os.environ,
            {
                "TOKENLESS_DATA_DIR": str(self.root / "win databases"),
                "QODER_PLUGIN_ROOT": str(self.target),
            },
        ):
            qwenwork_adapter.prepare_input(
                {**payload, "tool_use_id": "call-win", "tool_input": {}}
            )
            self.assertTrue(
                qwenwork_adapter.recovery_available(),
                "the bridge must enable without run-hook.sh on Windows",
            )

    def test_pre_tool_input_fails_open_on_non_string_tool_name(self) -> None:
        # A malformed payload must never crash the host's PreToolUse hook
        # (fail-open contract, `test_malformed_hook_stdin_passes_through`).
        self.prepare_detached_plugin()
        for bad in (None, 123, {"a": 1}):
            with self.subTest(tool_name=bad):
                name, rewritten = qwenwork_adapter.pre_tool_input(
                    {"tool_name": bad, "tool_input": {"command": "echo hi"}}
                )
                self.assertEqual(name, "")
                self.assertIsNone(rewritten)

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


class DataDirectoryPlatformTests(unittest.TestCase):
    def test_missing_windows_roots_fail_promptly_without_writes(self) -> None:
        probes = []

        class MissingWindowsPath(PureWindowsPath):
            def lstat(self) -> os.stat_result:
                probes.append(str(self))
                if len(probes) > 3:
                    raise AssertionError("ancestor search did not terminate at the missing root")
                raise FileNotFoundError(errno.ENOENT, "missing Windows root", str(self))

        for configured in (
            "Z:\\tokenless",
            "Z:\\",
            "\\\\server\\missing\\tokenless",
            "\\\\server\\missing\\",
        ):
            with self.subTest(configured=configured), contextlib.ExitStack() as stack:
                stack.enter_context(mock.patch.dict(os.environ, {"TOKENLESS_DATA_DIR": configured}))
                stack.enter_context(mock.patch.object(utils, "Path", MissingWindowsPath))
                writes = [
                    stack.enter_context(
                        mock.patch(target, side_effect=AssertionError("unexpected write"))
                    )
                    for target in ("os.mkdir", "builtins.open", "io.open")
                ]
                root = PureWindowsPath(configured).anchor
                expected = [configured] if configured == root else [configured, root]
                probes.clear()
                with self.assertRaises(FileNotFoundError) as error:
                    utils.resolve_data_directory()
                self.assertEqual(error.exception.filename, root)
                self.assertEqual(probes, expected)

                probes.clear()
                stdout, stderr = io.StringIO(), io.StringIO()
                with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    lifecycle._data_directory_notice(utils.resolve_data_directory)
                self.assertEqual(probes, expected)
                self.assertIn("unavailable", stdout.getvalue())
                self.assertIn("missing Windows root", stderr.getvalue())
                self.assertIn("hooks pass through", stderr.getvalue())
                self.assertEqual(os.environ["TOKENLESS_DATA_DIR"], configured)
                for operation in writes:
                    operation.assert_not_called()

    def test_windows_profile_uses_process_token_and_closes_handle(self) -> None:
        import ctypes

        for failure in (None, "token", "size", "profile"):
            with self.subTest(failure=failure):
                kernel, security, profiles = mock.Mock(), mock.Mock(), mock.Mock()
                kernel.GetCurrentProcess.return_value = 123

                def open_token(process: int, access: int, token: Any) -> bool:
                    self.assertEqual((process, access), (123, 0x0008))
                    token._obj.value = 456
                    return failure != "token"

                def get_profile(token: Any, buffer: Any, size: Any) -> bool:
                    self.assertEqual(token.value, 456)
                    if buffer is None:
                        size._obj.value = 0 if failure == "size" else 256
                        return False
                    buffer.value = r"C:\Users\system-user"
                    return failure != "profile"

                security.OpenProcessToken.side_effect = open_token
                profiles.GetUserProfileDirectoryW.side_effect = get_profile
                with mock.patch.object(
                    ctypes, "WinDLL", create=True, side_effect=[kernel, security, profiles]
                ):
                    if failure:
                        with self.assertRaises(OSError):
                            utils._windows_profile_directory()
                    else:
                        self.assertEqual(
                            utils._windows_profile_directory(), r"C:\Users\system-user"
                        )
                self.assertEqual(kernel.CloseHandle.call_count, int(failure != "token"))


@unittest.skipUnless(REAL_BIN, "set TOKENLESS_TEST_BIN to existing Tokenless/RTK binaries")
class RealDataDirectoryTests(unittest.TestCase):
    def test_hook_state_matches_real_cli_with_changed_home(self) -> None:
        """Probe existing binaries without executing rewritten commands or recording stats."""
        binary_dir = Path(REAL_BIN).resolve()
        with tempfile.TemporaryDirectory(prefix="tokenless-real-path-") as temporary:
            root = Path(temporary).resolve()
            custom = root / "custom '$;` data"
            env = {
                **os.environ,
                "HOME": str(root),
                "PATH": str(binary_dir) + os.pathsep + os.environ.get("PATH", ""),
                "TOKENLESS_STATS_ENABLED": "0",
                "TOKENLESS_SLS_ENABLED": "0",
                "TOKENLESS_COMPRESSION_ENABLED": "1",
                "DSH_SHELL": "0",
            }
            env.pop("QODER_PLUGIN_ROOT", None)
            for override in (None, "", str(custom)):
                with self.subTest(override=override):
                    env.pop("TOKENLESS_DATA_DIR", None)
                    if override is not None:
                        env["TOKENLESS_DATA_DIR"] = override
                    with mock.patch.dict(os.environ, env, clear=True):
                        # Empty attribution prevents RTK receipt registration. The
                        # returned command exposes the real CLI's validated path.
                        request = utils.build_pre_tool_request(
                            {"command": "git status"}, "qwenwork", "Bash", "command"
                        )
                        result = subprocess.run(
                            [str(binary_dir / "tokenless"), "compress"],
                            input=json.dumps(request),
                            capture_output=True,
                            text=True,
                            env=env,
                            timeout=15,
                        )
                        self.assertEqual(result.returncode, 0, result.stderr)
                        response = json.loads(result.stdout)["result"]
                        self.assertEqual(response["action"], "replace_arguments")
                        command = response["arguments"]["command"]
                        cli_directory = next(
                            part.split("=", 1)[1]
                            for part in shlex.split(command)
                            if part.startswith("TOKENLESS_DATA_DIR=")
                        )
                        qwenwork_adapter.prepare_input(
                            {"tool_name": "mcp__workspace__bash", "cwd": str(root)}
                        )
                        identity = uuid.uuid4().hex
                        marker = Path(
                            utils._optimization_state_path("qwenwork", identity, identity)
                        )
                        created = [
                            path
                            for path in (marker.parent, marker.parent.parent)
                            if not path.exists()
                        ]
                        try:
                            # Do not prune unrelated user sessions during this opt-in probe.
                            with mock.patch.object(utils, "_prune_optimization_states"):
                                utils.mark_rtk_optimized("qwenwork", identity, identity)
                            self.assertTrue(marker.is_file())
                            self.assertEqual(marker.parent.parent, Path(cli_directory))
                            self.assertEqual(os.environ.get("TOKENLESS_DATA_DIR"), override)
                            self.assertFalse((root / ".tokenless").exists())
                        finally:
                            marker.unlink(missing_ok=True)
                            for path in created:
                                path.rmdir()


if __name__ == "__main__":
    unittest.main()
