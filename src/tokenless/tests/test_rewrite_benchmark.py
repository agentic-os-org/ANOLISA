#!/usr/bin/env python3
"""Keep latency measurements from accepting failed or skipped rewrites."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import shlex
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "benchmark/rewrite-latency/bench.py"
BINARY = ROOT / "target/debug/tokenless"
SPEC = importlib.util.spec_from_file_location("rewrite_benchmark", SOURCE)
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


class RewriteBenchmarkTest(unittest.TestCase):
    def setUp(self) -> None:
        self.request = bench.pre_tool_request("git status", "call-1")
        self.rewritten = "/plugin/bin/tokenless rtk git status"
        self.arguments = {"command": self.rewritten, "timeout": 30}
        self.response = {
            "protocol_version": 2,
            "operation": "pre_tool",
            "result": {
                "action": "replace_arguments",
                "output_optimization": "rtk",
                "arguments": self.arguments,
            },
        }

    def validate(self, expected: str = "replace_arguments") -> str:
        return bench.validate_response(
            "pre_tool", json.dumps(self.response), self.request, expected
        )

    def test_hook_and_core_use_identical_arguments(self) -> None:
        for command, _ in bench.CASES.values():
            request = bench.pre_tool_request(command, "call-1")
            payload = bench.hook_payload(request, Path("/workspace"))
            self.assertEqual(payload["tool_input"], request["input"]["arguments"])
            self.assertEqual(payload["cwd"], "/workspace")
            self.assertEqual(
                payload["tool_use_id"], request["attribution"]["tool_use_id"]
            )

    def test_accepts_applied_rewrite(self) -> None:
        self.assertEqual(self.validate(), self.rewritten)

    def test_accepts_legacy_rtk_launcher(self) -> None:
        self.arguments["command"] = "/legacy/bin/rtk git status"
        self.assertEqual(self.validate(), "/legacy/bin/rtk git status")

    def test_version_contract_preserves_grep_policy_change(self) -> None:
        legacy, old_actions = bench.version_contract(Path("/old"), "tokenless 0.9.1")
        current, new_actions = bench.version_contract(Path("/new"), "tokenless 0.9.4")
        self.assertEqual(legacy, ["/old/bin/rtk"])
        self.assertEqual(current, ["/new/bin/tokenless", "rtk"])
        self.assertEqual(old_actions["native_grep"], "replace_arguments")
        self.assertEqual(new_actions["native_grep"], "passthrough")
        for name in bench.CASES.keys() - {"native_grep"}:
            self.assertEqual(old_actions[name], new_actions[name])

    def test_rejects_passthrough_as_rewrite(self) -> None:
        self.response["result"].update(
            action="passthrough",
            output_optimization="none",
            arguments=self.request["input"]["arguments"],
        )
        with self.assertRaises(ValueError):
            self.validate()
        self.assertEqual(self.validate("passthrough"), "git status")

    def test_rejects_silent_hook_failure(self) -> None:
        with self.assertRaises(KeyError):
            bench.validate_response("hook", "{}", self.request, "replace_arguments")

    def test_rejects_malformed_or_wrong_operation(self) -> None:
        with self.assertRaises(ValueError):
            bench.validate_response(
                "pre_tool", "not json", self.request, "replace_arguments"
            )
        self.response["operation"] = "post_tool"
        with self.assertRaises(ValueError):
            self.validate()

    def test_rejects_unchanged_command_and_lost_arguments(self) -> None:
        self.arguments["command"] = "git status"
        with self.assertRaises(ValueError):
            self.validate()
        self.arguments["command"] = self.rewritten
        del self.arguments["timeout"]
        with self.assertRaises(ValueError):
            self.validate()

    def test_rejects_disagreeing_host_commands(self) -> None:
        response = {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": self.arguments,
                "tool_input": {"command": "git status"},
            }
        }
        with self.assertRaises(ValueError):
            bench.validate_response(
                "hook", json.dumps(response), self.request, "replace_arguments"
            )

    def test_normalization_retains_command_semantics(self) -> None:
        command = "env TOKENLESS_DATA_DIR=/state TOKENLESS_RTK_RUN_ID=" + "a" * 32
        command += " /plugin/bin/tokenless rtk git status && echo sentinel"
        normalized = bench.normalize_command(command, Path("/plugin"), Path("/state"))
        self.assertEqual(
            normalized,
            "env TOKENLESS_DATA_DIR=<state> TOKENLESS_RTK_RUN_ID=<run> "
            "<rtk> git status && echo sentinel",
        )
        legacy = command.replace("/plugin/bin/tokenless rtk", "/plugin/bin/rtk")
        self.assertEqual(
            bench.normalize_command(legacy, Path("/plugin"), Path("/state")),
            normalized,
        )

    def test_nearest_rank_percentiles_include_every_sample(self) -> None:
        result = bench.summarize(list(range(1, 31)))
        self.assertEqual(result["n"], 30)
        self.assertEqual(result["median_ms"], 15.5)
        self.assertEqual(result["p90_ms"], 27)
        self.assertEqual(result["p95_ms"], 29)

    def test_normalization_accepts_core_and_python_path_quoting(self) -> None:
        # These literal spellings follow Core's POSIX shell_quote, including
        # its stricter safe-character set and different apostrophe escaping.
        for suffix, quoted in (
            ("+", "'/plugin+/bin/tokenless'"),
            ("'s", "'/plugin'\\''s/bin/tokenless'"),
        ):
            plugin = Path("/plugin" + suffix)
            tail = "git status && printf '%s' 'a+b'"
            for path in (quoted, shlex.quote(str(plugin / "bin/tokenless"))):
                with self.subTest(suffix=suffix, path=path):
                    command = f"env TOKENLESS_DATA_DIR='/state+' {path} rtk {tail}"
                    self.assertEqual(
                        bench.normalize_command(command, plugin, Path("/state+")),
                        f"env TOKENLESS_DATA_DIR=<state> <rtk> {tail}",
                    )
                    legacy = command.replace("/bin/tokenless", "/bin/rtk").replace(
                        " rtk ", " "
                    )
                    self.assertEqual(
                        bench.normalize_command(legacy, plugin, Path("/state+")),
                        f"env TOKENLESS_DATA_DIR=<state> <rtk> {tail}",
                    )

    @unittest.skipUnless(os.name == "posix", "POSIX symlinks required")
    def test_variants_normalize_symlinked_state_without_core(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            real = root / "real"
            real.mkdir()
            alias = root / "alias"
            alias.symlink_to(real, target_is_directory=True)
            variants = {}
            for label in ("before", "after"):
                plugin = root / label
                (plugin / "bin").mkdir(parents=True)
                (plugin / "bin/tokenless").write_bytes(b"fixture")
                variants[label] = plugin
            overrides = {"DSH_SHELL": "1"}
            for prefix in ("TOKENLESS", "DSH_TOKENLESS"):
                for name in ("DATA_DIR", "STATS_DB", "STASH_DB"):
                    overrides[f"{prefix}_{name}"] = str(root / "external")
            observed = {}

            def respond(
                args: list[str], data: str, env: dict, cwd: Path
            ) -> tuple[float, subprocess.CompletedProcess]:
                state = Path(env["TOKENLESS_DATA_DIR"])
                observed[state.name] = state
                self.assertEqual(env["TOKENLESS_STATS_DB"], str(state / "stats.db"))
                self.assertEqual(env["TOKENLESS_STASH_DB"], str(state / "stash.db"))
                for name in ("DATA_DIR", "STATS_DB", "STASH_DB"):
                    self.assertNotIn(f"DSH_TOKENLESS_{name}", env)
                if args[-1] == "--version":
                    output = "rtk 0.49.0" if "rtk" in args else "tokenless 0.9.4"
                elif "rtk" in args:
                    output = "rtk git status"
                else:
                    # Core emits canonical paths even if its input used a symlink.
                    plugin = Path(env["QODER_PLUGIN_ROOT"])
                    command = (
                        f"env TOKENLESS_DATA_DIR={shlex.quote(str(state.resolve()))} "
                        f"{shlex.quote(str(plugin / 'bin/tokenless'))} rtk git status"
                    )
                    payload = json.loads(data)
                    if args[0] == "bash":
                        arguments = {**payload["tool_input"], "command": command}
                        value = {
                            "hookSpecificOutput": {
                                "hookEventName": "PreToolUse",
                                "updatedInput": arguments,
                                "tool_input": {"command": command},
                            }
                        }
                    else:
                        value = {
                            "protocol_version": 2,
                            "operation": "pre_tool",
                            "result": {
                                "action": "replace_arguments",
                                "output_optimization": "rtk",
                                "arguments": {
                                    **payload["input"]["arguments"],
                                    "command": command,
                                },
                            },
                        }
                    output = json.dumps(value)
                return 1.0, subprocess.CompletedProcess(args, 0, output, "")

            with (
                patch.dict(os.environ, overrides),
                patch.object(tempfile, "tempdir", str(alias)),
                patch.object(bench, "CASES", {"git": bench.CASES["git"]}),
                patch.object(bench, "run_process", side_effect=respond),
            ):
                report = bench.benchmark(variants, root, samples=1, warmup=0, seed=1)
            self.assertEqual(set(observed), set(variants))
            self.assertEqual(len(set(observed.values())), 2)
            for state in observed.values():
                self.assertEqual(state, state.resolve())
            for variant in report["variants"].values():
                for layer in ("git/hook", "git/pre_tool"):
                    self.assertEqual(
                        variant["normalized_commands"][layer],
                        "env TOKENLESS_DATA_DIR=<state> <rtk> git status",
                    )


@unittest.skipUnless(os.name == "posix" and BINARY.is_file(), "POSIX Core required")
class BenchmarkStateIsolationTest(unittest.TestCase):
    def stage_plugin(self, tree: Path) -> Path:
        adapters = ROOT / "adapters/tokenless"
        common = tree / "common/hooks"
        shutil.copytree(
            adapters / "common/hooks",
            common,
            ignore=shutil.ignore_patterns("__pycache__"),
        )
        shutil.copy2(adapters / "hook_dispatch.py", tree)
        plugin = tree / "qwenwork"
        shutil.copytree(adapters / "qwenwork", plugin, symlinks=True)
        # Exercise the actual shared Hook on both Linux and macOS.
        shutil.copy2(common / "run-hook.sh", plugin / "hooks/run-hook.sh")
        (plugin / "bin").mkdir()
        os.link(BINARY, plugin / "bin/tokenless")
        return plugin

    def test_core_rewrites_match_across_differently_quoted_paths(self) -> None:
        with tempfile.TemporaryDirectory(dir=ROOT / "target") as temp:
            root = Path(temp).resolve()
            variants = {
                label: self.stage_plugin(root / label) for label in ("before+", "after")
            }
            with patch.object(
                bench,
                "CASES",
                {name: bench.CASES[name] for name in ("git", "compound")},
            ):
                report = bench.benchmark(variants, root, samples=1, warmup=0, seed=1)
            before, after = report["variants"].values()
            self.assertEqual(
                before["normalized_commands"], after["normalized_commands"]
            )
            for name in ("git", "compound"):
                for layer in ("hook", "pre_tool"):
                    command = before["normalized_commands"][f"{name}/{layer}"]
                    self.assertIn("<rtk> git status", command)
                    self.assertIn("TOKENLESS_DATA_DIR=<state>", command)

    def test_inherited_databases_remain_untouched(self) -> None:
        for managed in (False, True):
            with self.subTest(managed=managed), tempfile.TemporaryDirectory(
                dir=ROOT / "target"
            ) as temp:
                root = Path(temp).resolve()
                external = root / "external"
                external.mkdir()
                for name in ("stats.db", "stash.db"):
                    with closing(sqlite3.connect(external / name)) as db:
                        db.execute("CREATE TABLE sentinel (value TEXT)")
                        db.commit()

                def snapshot() -> dict[str, str]:
                    return {
                        p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                        for p in external.iterdir()
                    }

                original = snapshot()
                prefix = "DSH_TOKENLESS" if managed else "TOKENLESS"
                overrides = {
                    f"{namespace}_{name}": ""
                    for namespace in ("TOKENLESS", "DSH_TOKENLESS")
                    for name in ("DATA_DIR", "STATS_DB", "STASH_DB")
                }
                overrides.update(
                    {
                        "DSH_SHELL": "1" if managed else "0",
                        f"{prefix}_DATA_DIR": str(external),
                        f"{prefix}_STATS_DB": str(external / "stats.db"),
                        f"{prefix}_STASH_DB": str(external / "stash.db"),
                    }
                )
                variants = {}
                for label in ("before", "after"):
                    variants[label] = self.stage_plugin(root / label)

                observed = {}
                run_process = bench.run_process

                def observe(
                    args: list[str], data: str, env: dict, cwd: Path
                ) -> tuple[float, subprocess.CompletedProcess]:
                    result = run_process(args, data, env, cwd)
                    if "compress" in args:
                        state = Path(env["TOKENLESS_DATA_DIR"])
                        stats = state / "stats.db"
                        self.assertEqual(
                            snapshot(),
                            original,
                            "benchmark modified the caller's databases",
                        )
                        self.assertTrue(stats.is_file(), "private stats DB missing")
                        with closing(sqlite3.connect(stats)) as db:
                            db.execute("SELECT COUNT(*) FROM stats").fetchone()
                        observed[state.name] = stats
                        self.assertEqual(
                            env["TOKENLESS_STASH_DB"], str(state / "stash.db")
                        )
                        for name in ("DATA_DIR", "STATS_DB", "STASH_DB"):
                            self.assertNotIn(f"DSH_TOKENLESS_{name}", env)
                    return result

                with (
                    patch.dict(os.environ, overrides),
                    patch.object(
                        bench, "CASES", {"unsupported": bench.CASES["unsupported"]}
                    ),
                    patch.object(bench, "run_process", side_effect=observe),
                ):
                    bench.benchmark(variants, root, samples=1, warmup=0, seed=1)
                self.assertEqual(set(observed), set(variants))
                self.assertEqual(len(set(observed.values())), 2)
                self.assertEqual(snapshot(), original)


if __name__ == "__main__":
    unittest.main()
