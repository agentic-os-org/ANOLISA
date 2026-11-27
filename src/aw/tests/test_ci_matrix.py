"""Exercise omitted coverage, ignored tests and script size limits without Cargo."""

import copy
import io
import json
import unittest
from pathlib import Path
from unittest.mock import patch

from test_ci_checks import GateFixture, gate
from check_inventory import check_inventory, collect, inventory
from check_layout import source_sizes
from check_matrix import (
    AW_PRODUCED_EVENTS,
    CAPABILITY_DOCS,
    COVERAGE_END,
    COVERAGE_EVENTS,
    COVERAGE_START,
    END,
    HOOK_EVENTS,
    START,
    capability_cells,
    coverage_table,
    main,
    table,
    validate,
)


class InventoryTests(GateFixture):
    def test_gate_collects_every_independent_provider_before_layout_validation(self) -> None:
        commands = []

        def run(command: list[str], cwd: Path, **kwargs: object) -> str:
            commands.append((command, cwd))
            return json.dumps({"workspace_root": str(cwd)})

        with (
            patch.object(gate, "candidate", return_value="a" * 40),
            patch.object(gate, "selftest"),
            patch.object(gate, "run", side_effect=run),
            patch("check_native.load"),
            patch.object(gate, "structure", side_effect=ValueError("stop before compile")) as layout,
            self.assertRaisesRegex(ValueError, "stop before compile"),
        ):
            gate.check()
        roots = [gate.REPO / "providers" / name
                 for name in ("agent-sec-core", "tokenless", "semantic-arbiter")]
        self.assertEqual(layout.call_args.args[0]["provider_workspaces"],
                         [{"workspace_root": str(root)} for root in roots])
        self.assertEqual([root for command, root in commands if command[1] == "fmt"],
                         [gate.AW, *roots])

    def test_build_once_and_reject_missing_empty_ignored_or_failed_targets(self) -> None:
        metadata = {
            "workspace_members": ["cli"],
            "packages": [
                {
                    "id": "cli",
                    "name": "aw-cli",
                    "targets": [{"name": "new_agent", "kind": ["test"]}],
                }
            ],
        }
        artifact = {
            "reason": "compiler-artifact",
            "package_id": "cli",
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
                    frozenset({"aw-cli/test/new_agent"})
                    if mode.startswith("opt-in")
                    else frozenset()
                )
                if mode in {"valid", "opt-in"}:
                    self.assertEqual(
                        collect(self.root, metadata, run, opt_in),
                        {
                            "aw-cli/test/new_agent": (
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
        self.root = self.root / "src/aw"
        self.root.mkdir(parents=True)
        self.matrix = json.loads((gate.AW / "tests/regression-matrix.json").read_text())
        self.runnable = {target: {"fixture"} for target in self.matrix["required_targets"]}
        for case in self.matrix["cases"].values():
            self.runnable.setdefault(case["target"], set()).add(case["test"])
        for name in CAPABILITY_DOCS:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(
                f"{START}\n{table(self.matrix)}\n{END}\n\n"
                f"{COVERAGE_START}\n{coverage_table(self.matrix)}\n{COVERAGE_END}\n"
            )

    def test_complete_manifest_is_accepted(self) -> None:
        validate(self.matrix, self.runnable, self.root)

    def test_independent_workspaces_preserve_the_complete_matrix(self) -> None:
        names = {target.split("/", 1)[0] for target in self.runnable}
        names.update(target.split("/", 1)[0] for target in self.matrix["opt_in_targets"])
        providers = {"aw-provider-sec-core", "aw-provider-tokenless", "aw-provider-semantic-arbiter"}

        def workspace(packages: set[str], root: Path) -> dict:
            return {
                "workspace_root": str(root),
                "workspace_members": sorted(packages),
                "packages": [{"id": name, "name": name} for name in sorted(packages)],
            }

        metadata = workspace(names - providers, self.root)
        metadata["provider_workspaces"] = [
            workspace({name}, self.root / name) for name in sorted(providers)
        ]
        (self.root / "tests").mkdir()
        for mode in ("valid", *sorted(providers), "missing-framework", "duplicate-target", "unknown-opt-in"):
            matrix = copy.deepcopy(self.matrix)
            if mode == "unknown-opt-in":
                matrix["opt_in_targets"]["unknown/test/acceptance"] = "fixture"
            (self.root / "tests/regression-matrix.json").write_text(json.dumps(matrix))

            def collected(root: Path, data: dict, run: object, opt_in: frozenset) -> dict:
                owned = set(data["workspace_members"])
                self.assertTrue(all(target.split("/", 1)[0] in owned for target in opt_in))
                if owned == {mode} or (mode == "missing-framework" and "aw-cli" in owned):
                    return {}
                result = {
                    target: tests for target, tests in self.runnable.items()
                    if target.split("/", 1)[0] in owned
                }
                if mode == "duplicate-target":
                    result["duplicate/test/target"] = {"case"}
                return result

            with self.subTest(mode=mode), patch("check_inventory.collect", side_effect=collected) as build:
                if mode == "valid":
                    check_inventory(self.root, metadata, object())
                    self.assertEqual(build.call_count, 4)
                else:
                    with self.assertRaises(ValueError):
                        check_inventory(self.root, metadata, object())

    def test_each_required_case_must_exist_and_be_runnable(self) -> None:
        for name, case in self.matrix["cases"].items():
            with self.subTest(case=name):
                runnable = copy.deepcopy(self.runnable)
                runnable[case["target"]].remove(case["test"])
                with self.assertRaisesRegex(ValueError, "missing or ignored"):
                    validate(self.matrix, runnable, self.root)

    def test_provider_omission_and_empty_or_unknown_coverage_fail(self) -> None:
        for provider in ("sec-core", "tokenless", "semantic-arbiter"):
            for mode in ("omitted", "empty", "unknown"):
                matrix = copy.deepcopy(self.matrix)
                if mode == "omitted":
                    matrix["providers"].pop(provider, None)
                else:
                    matrix["providers"][provider] = [] if mode == "empty" else ["undefined-case"]
                with self.subTest(provider=provider, mode=mode), self.assertRaises(ValueError):
                    validate(matrix, self.runnable, self.root)

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

    def test_empty_target_and_stale_or_missing_guide_grids_fail(self) -> None:
        runnable = copy.deepcopy(self.runnable)
        runnable[self.matrix["required_targets"][0]] = set()
        with self.assertRaisesRegex(ValueError, "target is missing or empty"):
            validate(self.matrix, runnable, self.root)
        valid_grid = f"{START}\n{table(self.matrix)}\n{END}\n"
        valid_coverage = f"{COVERAGE_START}\n{coverage_table(self.matrix)}\n{COVERAGE_END}\n"
        for name in CAPABILITY_DOCS:
            path = self.root / name
            original = path.read_text()
            for text, error in (
                (f"{START}\nstale\n{END}\n{valid_coverage}", "update the grid"),
                (f"no generated table\n{valid_coverage}", "missing unique matrix markers"),
                (valid_grid + valid_grid + valid_coverage, "missing unique matrix markers"),
                (f"<details>\n{valid_grid}</details>\n{valid_coverage}", "keep the Hook grid expanded"),
                (
                    f"<details>\n<details>inner</details>\n{valid_grid}</details>\n{valid_coverage}",
                    "keep the Hook grid expanded",
                ),
                (f"{valid_grid}{COVERAGE_START}\nstale\n{COVERAGE_END}\n", "update the coverage grid"),
                (valid_grid, "missing unique coverage markers"),
                (valid_grid + valid_coverage + valid_coverage, "missing unique coverage markers"),
                (
                    f"{valid_grid}<details>\n{valid_coverage}</details>\n",
                    "keep the Hook coverage grid expanded",
                ),
                (
                    f"{valid_grid}<details>\n<details>inner</details>\n{valid_coverage}</details>\n",
                    "keep the Hook coverage grid expanded",
                ),
                (
                    f"{valid_grid}<details>\n<!-- </details> -->\n{valid_coverage}</details>\n",
                    "keep the Hook coverage grid expanded",
                ),
            ):
                with self.subTest(document=name, content=text), self.assertRaisesRegex(
                    ValueError, error
                ):
                    path.write_text(text)
                    validate(self.matrix, self.runnable, self.root)
            path.write_text(original)

    def test_regeneration_updates_all_grids_without_changing_surrounding_text(self) -> None:
        (self.root / "tests").mkdir()
        (self.root / "tests/regression-matrix.json").write_text(json.dumps(self.matrix))
        for name in CAPABILITY_DOCS:
            (self.root / name).write_text(
                f"intro\n{START}\nstale\n{END}\nmiddle\n"
                f"{COVERAGE_START}\nstale\n{COVERAGE_END}\nfooter\n"
            )
        with patch("check_matrix.__file__", str(self.root / "scripts/check_matrix.py")), patch(
            "sys.argv", ["check_matrix.py", "--write-docs"]
        ):
            main()
        validate(self.matrix, self.runnable, self.root)
        for name in CAPABILITY_DOCS:
            self.assertEqual(
                (self.root / name).read_text(),
                f"intro\n{START}\n{table(self.matrix)}\n{END}\nmiddle\n"
                f"{COVERAGE_START}\n{coverage_table(self.matrix)}\n{COVERAGE_END}\nfooter\n",
            )

    def test_coverage_table_lists_13_agent_hooks_and_excludes_aw_produced_events(self) -> None:
        self.assertEqual(len(COVERAGE_EVENTS), 13)
        for event in AW_PRODUCED_EVENTS:
            self.assertNotIn(event, COVERAGE_EVENTS)
        rendered = coverage_table(self.matrix)
        lines = rendered.splitlines()
        header_columns = [cell.strip() for cell in lines[0].strip("|").split("|")]
        self.assertEqual(header_columns[0], "Agent")
        self.assertEqual(header_columns[1:], [f"`{event}`" for event in COVERAGE_EVENTS])
        for event in AW_PRODUCED_EVENTS:
            self.assertNotIn(event, rendered)
        self.assertEqual(len(lines), 2 + len(self.matrix["agents"]))
        rows_by_agent = {}
        for line in lines[2:]:
            cols = [cell.strip() for cell in line.strip("|").split("|")]
            rows_by_agent[cols[0].strip("`")] = dict(zip(COVERAGE_EVENTS, cols[1:]))
        self.assertEqual(list(rows_by_agent), sorted(self.matrix["agents"]))
        self.assertEqual(rows_by_agent["qoder"]["tool.before"], "o b ri")
        self.assertEqual(rows_by_agent["qoder"]["tool.after"], "o rr")
        self.assertEqual(rows_by_agent["qoder"]["permission.request"], "×")
        self.assertEqual(rows_by_agent["deepseek-harness"]["security.violation"], "o b")
        self.assertEqual(rows_by_agent["openclaw"]["input.submit"], "×")

        stdout = io.StringIO()
        with patch("sys.argv", ["check_matrix.py", "--coverage"]), patch("sys.stdout", stdout):
            main()
        self.assertEqual(stdout.getvalue(), rendered + "\n")

        for pair in (["--write-docs", "--coverage"], ["--coverage", "--cells-json"]):
            with self.subTest(flags=pair), patch(
                "sys.argv", ["check_matrix.py", *pair]
            ), self.assertRaises(SystemExit):
                main()

    def test_structured_16_hook_capability_cells_match_schemas_and_reject_unknown_hooks(self) -> None:
        config_schema = json.loads(
            (gate.AW / "crates/aw-config/schemas/configuration-v1alpha1.schema.json").read_text()
        )
        source_schema = json.loads(
            (
                gate.AW / "crates/aw-contracts/schemas/source-capability-v1alpha1.schema.json"
            ).read_text()
        )
        self.assertEqual(
            list(config_schema["properties"]["spec"]["properties"]["events"]["properties"]),
            list(HOOK_EVENTS),
        )
        self.assertEqual(source_schema["$defs"]["hookEvent"]["enum"], list(HOOK_EVENTS))
        cells = capability_cells(self.matrix)
        self.assertEqual(set(cells), set(self.matrix["agents"]))
        for agent, row in cells.items():
            self.assertEqual(list(row), list(HOOK_EVENTS))
            for event in HOOK_EVENTS:
                cell = row[event]
                self.assertEqual((cell["adapter"], cell["event"]), (agent, event))
                if event in self.matrix["agents"][agent]["events"]:
                    self.assertEqual(cell["status"], "supported")
                    self.assertEqual(cell["sources"], ["native"])
                    self.assertEqual(cell["mode"], "control")
                    self.assertEqual(
                        set(cell["effects"]),
                        set(self.matrix["agents"][agent]["events"][event]),
                    )
                else:
                    self.assertEqual(cell["status"], "unsupported")
                    self.assertEqual(cell["sources"], [])
                    self.assertIsNone(cell["mode"])
                    self.assertEqual(cell["effects"], [])

        unknown_hook = copy.deepcopy(self.matrix)
        unknown_hook["agents"]["qoder"]["events"]["unknown.hook"] = {"observe": ["catalog"]}
        with self.assertRaisesRegex(ValueError, "unknown semantic hook"):
            capability_cells(unknown_hook)

        missing_observe = copy.deepcopy(self.matrix)
        del missing_observe["agents"]["qoder"]["events"]["tool.before"]["observe"]
        with self.assertRaisesRegex(ValueError, "unsupported or incomplete effect set"):
            capability_cells(missing_observe)

        for event, bad_effect in (("session.end", "replace_input"), ("tool.after", "block")):
            bad_pair = copy.deepcopy(self.matrix)
            bad_pair["agents"]["qoder"]["events"][event] = {
                "observe": ["catalog"],
                bad_effect: ["catalog"],
            }
            with self.subTest(event=event, effect=bad_effect), self.assertRaisesRegex(
                ValueError, "unsupported or incomplete effect set"
            ):
                capability_cells(bad_pair)

        with patch("sys.argv", ["check_matrix.py", "--write-docs", "--cells-json"]), self.assertRaises(
            SystemExit
        ):
            main()


class ScriptLayoutTests(GateFixture):
    def test_production_and_fixture_scripts_share_the_same_limit(self) -> None:
        for relative in (
            "utility.py",
            "crates/new/scripts/check.py",
            "crates/aw-adapter/native/new/worker.py",
            "scripts/check.py",
            "tests/acceptance.py",
            "crates/new/tests/fixtures/bridge.js",
            "crates/aw-adapter/native/new/worker.mjs",
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
        directory = self.root / "crates/aw-adapter/native"
        directory.mkdir(parents=True)
        (directory / "bridge.py").symlink_to(self.root / "outside.py")
        with self.assertRaisesRegex(ValueError, "symlink"):
            source_sizes(self.root)


if __name__ == "__main__":
    unittest.main()
