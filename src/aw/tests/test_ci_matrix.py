"""Exercise omitted coverage, ignored tests and script size limits without Cargo."""

import copy
import json
import unittest
from pathlib import Path

from test_ci_checks import GateFixture, gate
from check_inventory import collect, inventory
from check_layout import source_sizes
from check_matrix import END, START, table, validate


class InventoryTests(GateFixture):
    def test_build_once_and_reject_missing_empty_ignored_or_failed_targets(self) -> None:
        metadata = {
            "workspace_members": ["service"],
            "packages": [
                {
                    "id": "service",
                    "name": "aw-service",
                    "targets": [{"name": "new_agent", "kind": ["test"]}],
                }
            ],
        }
        artifact = {
            "reason": "compiler-artifact",
            "package_id": "service",
            "target": metadata["packages"][0]["targets"][0],
            "profile": {"test": True},
            "executable": "/fixture/new_agent",
        }
        for mode in (
            "valid",
            "missing",
            "empty",
            "ignored",
            "unknown-ignored",
            "failed",
            "truncated",
            "opt-in",
            "opt-in-empty",
            "opt-in-live",
        ):
            commands = []

            def run(command: list[str], cwd: Path, **kwargs: object) -> str:
                commands.append(command)
                if command[0] == "cargo":
                    rows = [] if mode == "missing" else [artifact]
                    if mode != "truncated":
                        rows.append({"reason": "build-finished", "success": mode != "failed"})
                    return "\n".join(map(json.dumps, rows))
                if mode == "unknown-ignored" and "--ignored" in command:
                    return "unknown: test\n1 test, 0 benchmarks\n"
                if mode in {"empty", "opt-in-empty"} or (
                    mode not in {"ignored", "opt-in"} and "--ignored" in command
                ):
                    return "0 tests, 0 benchmarks\n"
                return "regression: test\n1 test, 0 benchmarks\n"

            with self.subTest(mode=mode):
                opt_in = (
                    frozenset({"aw-service/test/new_agent"})
                    if mode.startswith("opt-in")
                    else frozenset()
                )
                if mode in {"valid", "opt-in"}:
                    self.assertEqual(
                        collect(self.root, metadata, run, opt_in),
                        {
                            "aw-service/test/new_agent": (
                                {"regression"} if mode == "valid" else set()
                            ),
                        },
                    )
                else:
                    with self.assertRaises(ValueError):
                        collect(self.root, metadata, run, opt_in)
                self.assertEqual(sum(c[0] == "cargo" for c in commands), 1)

    def test_malformed_libtest_listing_is_not_empty_success(self) -> None:
        for malformed in (
            "",
            "noise",
            "0 tests, 0 benchmarks\nnoise",
            "test: test\n2 tests, 0 benchmarks",
            "0 tests, 0 benchmarks\n0 tests, 0 benchmarks",
        ):
            with self.subTest(listing=malformed), self.assertRaises(ValueError):
                inventory(malformed)


class MatrixTests(GateFixture):
    def setUp(self) -> None:
        super().setUp()
        self.matrix = json.loads((gate.AW / "tests/regression-matrix.json").read_text())
        self.runnable = {target: {"fixture"} for target in self.matrix["required_targets"]}
        for case in self.matrix["cases"].values():
            self.runnable.setdefault(case["target"], set()).add(case["test"])
        for name in ("README.md", "README_zh.md"):
            (self.root / name).write_text(f"{START}\n{table(self.matrix)}\n{END}\n")

    def test_complete_manifest_is_accepted(self) -> None:
        validate(self.matrix, self.runnable, self.root)

    def test_each_required_case_must_exist_and_be_runnable(self) -> None:
        for name, case in self.matrix["cases"].items():
            with self.subTest(case=name):
                runnable = copy.deepcopy(self.runnable)
                runnable[case["target"]].remove(case["test"])
                with self.assertRaisesRegex(ValueError, "missing or ignored"):
                    validate(self.matrix, runnable, self.root)

    def test_new_agent_hook_effect_and_composition_need_coverage(self) -> None:
        for mode in (
            "agent",
            "hook",
            "effect",
            "security",
            "tokenless",
            "lifecycle",
            "unknown-case",
            "catalog",
        ):
            matrix = copy.deepcopy(self.matrix)
            row = matrix["agents"]["qoder"]
            if mode == "agent":
                matrix["agents"]["new-agent"] = {"events": {}}
            elif mode == "hook":
                row["events"]["session.start"] = {}
            elif mode == "effect":
                row["events"]["tool.before"]["rewrite_input"] = []
            elif mode in {"security", "tokenless", "lifecycle"}:
                row[mode] = []
            elif mode == "unknown-case":
                row["events"]["tool.before"]["block"] = ["never-defined"]
            else:
                del matrix["cases"]["catalog"]
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                validate(matrix, self.runnable, self.root)

    def test_empty_target_and_stale_readme_fail(self) -> None:
        runnable = copy.deepcopy(self.runnable)
        runnable[self.matrix["required_targets"][0]] = set()
        with self.assertRaisesRegex(ValueError, "target is missing or empty"):
            validate(self.matrix, runnable, self.root)
        (self.root / "README_zh.md").write_text(f"{START}\nstale\n{END}\n")
        with self.assertRaisesRegex(ValueError, "update the grid"):
            validate(self.matrix, self.runnable, self.root)


class ScriptLayoutTests(GateFixture):
    def test_production_and_fixture_scripts_share_the_same_limit(self) -> None:
        for relative in (
            "utility.py",
            "crates/new/scripts/check.py",
            "adapters/new/worker.py",
            "scripts/check.py",
            "tests/acceptance.py",
            "crates/new/tests/fixtures/bridge.js",
            "adapters/new/worker.mjs",
            "crates/new/src/bridge.cjs",
        ):
            file = self.root / relative
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text("\n" * 699)
            source_sizes(self.root)
            file.write_text("\n" * 700)
            with self.subTest(path=relative), self.assertRaisesRegex(ValueError, "700 lines"):
                source_sizes(self.root)
            file.unlink()

    def test_symlink_cannot_bypass_script_audit(self) -> None:
        directory = self.root / "adapters"
        directory.mkdir()
        (directory / "bridge.py").symlink_to(self.root / "outside.py")
        with self.assertRaisesRegex(ValueError, "symlink"):
            source_sizes(self.root)


if __name__ == "__main__":
    unittest.main()
