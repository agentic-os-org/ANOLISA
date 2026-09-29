#!/usr/bin/env python3
"""Regression tests for the prebuilt release matrix planner."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import plan_matrix


class PlanMatrixTests(unittest.TestCase):
    def test_anolisa_targets(self) -> None:
        matrix = plan_matrix.build_matrix("anolisa")

        self.assertEqual(
            [(row["target-os"], row["target-arch"], row["profile"]) for row in matrix["include"]],
            [
                ("linux", "x86_64", "gnu2.17-x86_64"),
                ("linux", "aarch64", "gnu2.17-aarch64"),
                ("macos", "aarch64", "darwin11-aarch64"),
                ("macos", "x86_64", "darwin11-x86_64"),
            ],
        )
        self.assertTrue(all(row["component"] == "anolisa" for row in matrix["include"]))

    def test_cosh_ng_targets(self) -> None:
        matrix = plan_matrix.build_matrix("cosh-ng")

        self.assertEqual(
            [(row["target-os"], row["target-arch"], row["profile"]) for row in matrix["include"]],
            [
                ("linux", "x86_64", "gnu2.28-x86_64"),
                ("linux", "aarch64", "gnu2.28-aarch64"),
                ("macos", "aarch64", "darwin11-aarch64"),
                ("macos", "x86_64", "darwin11-x86_64"),
            ],
        )
        self.assertTrue(all(row["component"] == "cosh-ng" for row in matrix["include"]))

    def test_tokenless_targets(self) -> None:
        matrix = plan_matrix.build_matrix("tokenless")

        self.assertEqual(
            [(row["target-os"], row["target-arch"], row["profile"]) for row in matrix["include"]],
            [
                ("linux", "x86_64", "gnu2.17-x86_64"),
                ("linux", "aarch64", "gnu2.17-aarch64"),
                ("macos", "aarch64", "darwin11-aarch64"),
            ],
        )
        self.assertTrue(all(row["component"] == "tokenless" for row in matrix["include"]))

    def test_component_without_prebuilt_targets(self) -> None:
        self.assertEqual(plan_matrix.build_matrix("agentsight"), {"include": []})

    def test_cosh_ng_artifact_set_requires_intel_mac(self) -> None:
        self.assert_intel_artifact_set("cosh-ng", "0.25.0")

    def test_anolisa_artifact_set_requires_intel_mac(self) -> None:
        self.assert_intel_artifact_set("anolisa", "0.3.15")

    def assert_intel_artifact_set(self, component: str, version: str) -> None:
        spec = importlib.util.spec_from_file_location(
            "verify_artifacts", Path(__file__).with_name("verify-artifacts.py")
        )
        verifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(verifier)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for row in plan_matrix.build_matrix(component)["include"]:
                (root / f"{component}-prebuilt-{version}-{row['target-os']}-{row['target-arch']}").mkdir()
            args = [
                "verify-artifacts.py",
                "--component", component,
                "--version", version,
                "--directory", temporary,
                "--layout", "actions",
            ]
            with (
                mock.patch("sys.argv", args),
                mock.patch.object(verifier, "validate_directory") as validate,
            ):
                output = io.StringIO()
                with contextlib.redirect_stdout(output):
                    self.assertEqual(verifier.main(), 0)
                self.assertEqual(validate.call_count, 4)
                self.assertIn(f"Verified 16 {component} prebuilt release assets", output.getvalue())
                duplicate = root / f"{component}-prebuilt-{version}-macos-x86_64-copy"
                duplicate.mkdir()
                with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                    verifier.main()
                duplicate.rmdir()
                (root / f"{component}-prebuilt-{version}-macos-x86_64").rmdir()
                errors = io.StringIO()
                with contextlib.redirect_stderr(errors), self.assertRaises(SystemExit):
                    verifier.main()
                self.assertIn(f"missing=['{component}-prebuilt-{version}-macos-x86_64']", errors.getvalue())


if __name__ == "__main__":
    unittest.main()
