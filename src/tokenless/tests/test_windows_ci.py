#!/usr/bin/env python3
"""Check that missing CI prerequisites cannot produce a successful Windows gate."""

import contextlib
import io
import sys
from types import ModuleType, SimpleNamespace
import unittest
from unittest import mock

import test_windows


class WindowsCIPolicyTests(unittest.TestCase):
    def run_gate(self, skipped=(), successful=True, strict=True):
        result = SimpleNamespace(skipped=skipped, wasSuccessful=lambda: successful)
        arguments = ["test_windows.py", "WindowsHookTests", "-v"]
        if strict:
            arguments.insert(1, "--ci")
        with mock.patch.object(
            test_windows.unittest, "main", return_value=SimpleNamespace(result=result)
        ) as runner, contextlib.redirect_stderr(io.StringIO()) as errors:
            status = test_windows.main(arguments)
        self.assertEqual(runner.call_args.kwargs["argv"], ["test_windows.py", "WindowsHookTests", "-v"])
        return status, errors.getvalue()

    def skipped(self, name):
        return [(test_windows.WindowsHookTests(name), "missing prerequisite")]

    def test_successful_required_tests_pass(self):
        self.assertEqual(self.run_gate(), (0, ""))

    def test_missing_bash_or_node_fails_ci(self):
        for name in (
            "test_legacy_bash_execution_preserves_stats_path",
            "test_npm_launchers_support_selected_node_and_preserve_exit_code",
        ):
            with self.subTest(name=name):
                status, error = self.run_gate(skipped=self.skipped(name))
                self.assertEqual(status, 1)
                self.assertIn(name, error)

    def test_desktop_host_skips_are_explicitly_allowed(self):
        for name in (
            "test_managed_bash_uses_installed_host_manifest",
            "test_installed_host_executes_registered_hooks",
        ):
            with self.subTest(name=name):
                self.assertEqual(self.run_gate(skipped=self.skipped(name)), (0, ""))

    def test_symlink_checkout_skip_fails_ci(self):
        skipped = [(test_windows.WindowsPackagingTests(
            "test_text_symlink_materialization_excludes_build_artifacts"
        ), "requires a core.symlinks=false checkout")]
        self.assertEqual(self.run_gate(skipped=skipped)[0], 1)

    def test_failures_remain_failures(self):
        self.assertEqual(self.run_gate(successful=False)[0], 1)

    def test_optional_local_skips_keep_existing_behavior(self):
        skipped = self.skipped("test_legacy_bash_execution_preserves_stats_path")
        self.assertEqual(self.run_gate(skipped=skipped, strict=False), (0, ""))

    def test_desktop_allowlist_is_exact_and_class_qualified(self):
        prefix = f"{test_windows.__name__}.WindowsHookTests."
        self.assertEqual(test_windows.desktop_host_test_ids(), {
            prefix + "test_managed_bash_uses_installed_host_manifest",
            prefix + "test_installed_host_executes_registered_hooks",
        })

    def test_workflow_selects_every_windows_test_class(self):
        self.assertEqual(set(test_windows.windows_test_cases()), {
            test_windows.WindowsHookTests, test_windows.WindowsPackagingTests,
        })

    def test_same_named_method_in_another_class_is_required(self):
        name = "test_installed_host_executes_registered_hooks"
        other_class = type("OtherTests", (unittest.TestCase,), {name: lambda self: None})
        skipped = [(other_class(name), "missing prerequisite")]
        self.assertEqual(self.run_gate(skipped=skipped)[0], 1)

    def test_real_unittest_skip_fails_ci(self):
        class RequiredSkip(unittest.TestCase):
            def test_missing_prerequisite(self):
                self.skipTest("missing prerequisite")

        module = ModuleType("__main__")
        module.RequiredSkip = RequiredSkip
        with (
            mock.patch.dict(sys.modules, {"__main__": module}),
            contextlib.redirect_stderr(io.StringIO()) as errors,
        ):
            status = test_windows.main(["test_windows.py", "--ci", "RequiredSkip"])
        self.assertEqual(status, 1)
        self.assertIn("Required Windows test skipped:", errors.getvalue())


if __name__ == "__main__":
    unittest.main()
