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
    def test_os_skills_targets(self) -> None:
        rows = plan_matrix.build_matrix("os-skills")["include"]
        self.assertEqual(
            [(row["target-os"], row["target-arch"], row["profile"]) for row in rows],
            [
                ("linux", "x86_64", "static-linux"),
                ("linux", "aarch64", "static-linux"),
            ],
        )

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
                ("macos", "x86_64", "darwin11-x86_64"),
                ("windows", "x86_64", "windows-msvc-x86_64"),
            ],
        )
        self.assertTrue(all(row["component"] == "tokenless" for row in matrix["include"]))

    def test_agentsight_targets(self) -> None:
        rows = plan_matrix.build_matrix("agentsight")["include"]
        self.assertEqual(
            [(row["target-os"], row["target-arch"]) for row in rows],
            [("linux", "x86_64"), ("linux", "aarch64"), ("macos", "aarch64")],
        )

    def test_tokenless_preview_uses_source_targets(self) -> None:
        current = plan_matrix.build_matrix("tokenless")["include"]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = root / "src/tokenless/Cargo.toml"
            manifest.parent.mkdir(parents=True)
            manifest.touch()
            self.assertEqual(plan_matrix.build_matrix("tokenless", root)["include"], current[:3])
            source_matrix = root / ".github/actions/prebuilt-rust-common/targets.py"
            source_matrix.parent.mkdir(parents=True)
            for rows in (current[:3], current[:4], current):
                targets = [
                    {key: value for key, value in row.items() if key != "component"} for row in rows
                ]
                source_matrix.write_text(f"TARGETS = {{'tokenless': {targets!r}}}\n")
                self.assertEqual(plan_matrix.build_matrix("tokenless", root)["include"], rows)

    def test_tokenless_preview_requires_source_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary, self.assertRaises(ValueError):
            plan_matrix.build_matrix("tokenless", Path(temporary))

    def test_historical_tokenless_artifact_set(self) -> None:
        spec = importlib.util.spec_from_file_location(
            "verify_artifacts", Path(__file__).with_name("verify-artifacts.py")
        )
        verifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(verifier)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            manifest = source / "src/tokenless/Cargo.toml"
            manifest.parent.mkdir(parents=True)
            manifest.touch()
            artifacts = root / "artifacts"
            artifacts.mkdir()
            for row in plan_matrix.build_matrix("tokenless", source)["include"]:
                (
                    artifacts / f"tokenless-prebuilt-0.7.7-{row['target-os']}-{row['target-arch']}"
                ).mkdir()
            args = [
                "verify-artifacts.py",
                "--component",
                "tokenless",
                "--version",
                "0.7.7",
                "--directory",
                str(artifacts),
                "--layout",
                "actions",
                "--tokenless-source-root",
                str(source),
            ]
            with (
                mock.patch("sys.argv", args),
                mock.patch.object(verifier, "validate_directory") as validate,
            ):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(verifier.main(), 0)
                self.assertEqual(validate.call_count, 3)
                (artifacts / "tokenless-prebuilt-0.7.7-macos-x86_64").mkdir()
                with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                    verifier.main()

    def test_component_without_prebuilt_targets(self) -> None:
        self.assertEqual(plan_matrix.build_matrix("blaze"), {"include": []})

    def test_sec_core_has_only_linux_x86_64(self) -> None:
        self.assertEqual(
            plan_matrix.build_matrix("agent-sec-core"),
            {"include": [{
                "component": "agent-sec-core",
                "target-os": "linux",
                "target-arch": "x86_64",
                "profile": "gnu2.28-x86_64",
            }]},
        )

    def test_ws_ckpt_requires_both_linux_targets(self) -> None:
        matrix = plan_matrix.build_matrix("ws-ckpt")
        self.assertEqual(
            [(row["target-os"], row["target-arch"], row["profile"]) for row in matrix["include"]],
            [
                ("linux", "x86_64", "gnu2.28-x86_64"),
                ("linux", "aarch64", "gnu2.28-aarch64"),
            ],
        )
        self.assertTrue(all(row["component"] == "ws-ckpt" for row in matrix["include"]))

    def test_ws_ckpt_download_requires_arm64_artifact(self) -> None:
        spec = importlib.util.spec_from_file_location(
            "verify_artifacts", Path(__file__).with_name("verify-artifacts.py")
        )
        verifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(verifier)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for arch in ("x86_64", "aarch64"):
                (root / f"ws-ckpt-prebuilt-0.5.0-linux-{arch}").mkdir()
            args = [
                "verify-artifacts.py", "--component", "ws-ckpt", "--version", "0.5.0",
                "--directory", temporary, "--layout", "actions",
            ]
            with mock.patch("sys.argv", args), mock.patch.object(verifier, "validate_directory"):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(verifier.main(), 0)
                (root / "ws-ckpt-prebuilt-0.5.0-linux-aarch64").rmdir()
                with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                    verifier.main()

    def test_skillfs_requires_both_linux_targets(self) -> None:
        matrix = plan_matrix.build_matrix("skillfs")
        self.assertEqual(
            [(row["target-os"], row["target-arch"], row["profile"]) for row in matrix["include"]],
            [
                ("linux", "x86_64", "gnu2.28-x86_64"),
                ("linux", "aarch64", "gnu2.28-aarch64"),
            ],
        )
        self.assertTrue(all(row["component"] == "skillfs" for row in matrix["include"]))

    def test_skillfs_download_requires_arm64_artifact(self) -> None:
        spec = importlib.util.spec_from_file_location(
            "verify_artifacts", Path(__file__).with_name("verify-artifacts.py")
        )
        verifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(verifier)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for arch in ("x86_64", "aarch64"):
                (root / f"skillfs-prebuilt-0.5.0-linux-{arch}").mkdir()
            args = [
                "verify-artifacts.py", "--component", "skillfs", "--version", "0.5.0",
                "--directory", temporary, "--layout", "actions",
            ]
            with mock.patch("sys.argv", args), mock.patch.object(verifier, "validate_directory"):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(verifier.main(), 0)
                (root / "skillfs-prebuilt-0.5.0-linux-aarch64").rmdir()
                with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                    verifier.main()

    def test_agent_memory_requires_both_linux_targets(self) -> None:
        matrix = plan_matrix.build_matrix("agent-memory")
        self.assertEqual(
            [(row["target-os"], row["target-arch"], row["profile"]) for row in matrix["include"]],
            [
                ("linux", "x86_64", "gnu2.28-x86_64"),
                ("linux", "aarch64", "gnu2.28-aarch64"),
            ],
        )
        self.assertTrue(all(row["component"] == "agent-memory" for row in matrix["include"]))

    def test_agent_memory_download_requires_arm64_artifact(self) -> None:
        spec = importlib.util.spec_from_file_location(
            "verify_artifacts", Path(__file__).with_name("verify-artifacts.py")
        )
        verifier = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(verifier)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for arch in ("x86_64", "aarch64"):
                (root / f"agent-memory-prebuilt-0.5.0-linux-{arch}").mkdir()
            args = [
                "verify-artifacts.py", "--component", "agent-memory", "--version", "0.5.0",
                "--directory", temporary, "--layout", "actions",
            ]
            with mock.patch("sys.argv", args), mock.patch.object(verifier, "validate_directory"):
                with contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(verifier.main(), 0)
                (root / "agent-memory-prebuilt-0.5.0-linux-aarch64").rmdir()
                with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                    verifier.main()

    def test_cosh_ng_artifact_set_requires_intel_mac(self) -> None:
        self.assert_intel_artifact_set("cosh-ng", "0.25.0")

    def test_anolisa_artifact_set_requires_intel_mac(self) -> None:
        self.assert_intel_artifact_set("anolisa", "0.3.15")

    def test_tokenless_artifact_set_requires_intel_mac(self) -> None:
        self.assert_intel_artifact_set("tokenless", "0.8.8")

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
                count = 5 if component == "tokenless" else 4
                self.assertEqual(validate.call_count, count)
                self.assertIn(f"Verified {4 * count} {component} prebuilt release assets", output.getvalue())
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
