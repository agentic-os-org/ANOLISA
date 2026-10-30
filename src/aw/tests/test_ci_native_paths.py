"""Check local and bundled native-test executables without installing Agent SDKs."""

from __future__ import annotations

import importlib.util
import io
import json
import subprocess
import sys
from pathlib import Path
from unittest.mock import AsyncMock, patch

from test_ci_checks import AW, GateFixture
import test_claude_sdk_native as claude


SPEC = importlib.util.spec_from_file_location(
    "agentscope_lifecycle", AW / "crates/aw-cli/tests/agentscope_lifecycle.py"
)
agentscope = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(agentscope)


class BeforeLaunch(Exception):
    """Stop after configuration is written, before any SDK or process starts."""


class NativePathTests(GateFixture):
    def executables(self) -> dict[str, Path]:
        paths = {
            "aw": self.root / "framework/aw",
            "tokenless": self.root / "backend/tokenless",
            "tokenless-provider": self.root / "compression/bridge",
            "sec-core-provider": self.root / "security/bridge",
        }
        for path in paths.values():
            path.parent.mkdir(exist_ok=True)
            path.write_text("fixture")
        return paths

    def command(self, agent: str, paths: dict[str, Path], bundled: bool = False) -> list[str]:
        manifest = json.loads((AW / "tests/native-matrix.json").read_text())
        name = "sdk-cli" if agent == "claude-sdk" else "lifecycle"
        case = next(case for case in manifest["agents"][agent]["cases"] if case["name"] == name)
        argv = [
            arg.format(python=sys.executable, bin=str(self.root), output=str(self.root))
            for arg in case["argv"]
        ][1:]
        argv[0] = str(AW / argv[0])
        argv[argv.index("--output") + 1] = str(self.root)
        if not bundled:
            for flag, path in paths.items():
                if "--" + flag in argv:
                    argv[argv.index("--" + flag) + 1] = str(path)
        return argv

    def test_claude_uses_explicit_provider_with_clean_and_stale_layouts(self) -> None:
        paths = self.executables()

        def run_case(
            aw: Path,
            worker: Path,
            output: Path,
            mode: str,
            tokenless: Path | None = None,
            tokenless_provider: Path | None = None,
        ) -> dict[str, str]:
            document = claude.document(
                sys.executable, worker, output / "state", mode, tokenless, tokenless_provider
            )
            if tokenless is not None:
                self.assertEqual(
                    document["spec"]["providers"]["tokenless"]["transport"]["argv"][0],
                    str(paths["tokenless-provider"]),
                )
            return {"mode": mode}

        for stale in (False, True):
            if stale:
                (paths["aw"].parent / "aw-provider-tokenless").write_text("stale")
            with self.subTest(stale=stale), patch.object(
                sys, "argv", self.command("claude-sdk", paths)
            ):
                with patch.object(claude, "run_case", side_effect=run_case) as cases:
                    with patch.object(
                        claude, "check_callback_timeouts", new=AsyncMock()
                    ), patch.object(
                        claude, "check_callback_payloads", new=AsyncMock()
                    ), patch.object(
                        claude, "check_preserved_callbacks", new=AsyncMock()
                    ), patch.object(
                        claude.signal, "signal"
                    ), patch.object(
                        sys, "stdout", new=io.StringIO()
                    ):
                        claude.main()
                self.assertEqual(sum(len(call.args) == 6 for call in cases.call_args_list), 8)

    def test_agentscope_uses_explicit_providers_with_clean_and_stale_layouts(self) -> None:
        paths = self.executables()
        for stale in (False, True):
            if stale:
                for name in ("aw-provider-sec-core", "aw-provider-tokenless"):
                    (paths["aw"].parent / name).write_text("stale")
            with self.subTest(stale=stale), patch.object(
                sys, "argv", self.command("agentscope", paths)
            ):
                with patch.object(agentscope, "version", return_value="2.0.6"):
                    with patch.object(agentscope, "process", side_effect=BeforeLaunch):
                        with self.assertRaises(BeforeLaunch):
                            agentscope.main()
            providers = json.loads((self.root / "config.json").read_text())["spec"]["providers"]
            for name, flag in (
                ("security", "sec-core-provider"),
                ("tokenless", "tokenless-provider"),
            ):
                self.assertEqual(providers[name]["transport"]["argv"][0], str(paths[flag]))

    def test_missing_provider_arguments_fail_before_native_setup(self) -> None:
        paths = self.executables()
        for agent, flags in (
            ("claude-sdk", ("--tokenless", "--tokenless-provider")),
            ("agentscope", ("--tokenless-provider", "--sec-core-provider")),
        ):
            for flag in flags:
                argv = self.command(agent, paths)
                del argv[argv.index(flag) : argv.index(flag) + 2]
                result = subprocess.run(
                    [sys.executable, "-B", *argv], capture_output=True, text=True, timeout=10
                )
                with self.subTest(agent=agent, missing=flag):
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn(flag, result.stderr)
                    self.assertNotIn("Traceback", result.stderr)

    def test_nightly_bundle_paths_match_the_explicit_arguments(self) -> None:
        for agent in ("claude-sdk", "agentscope"):
            argv = self.command(agent, {}, bundled=True)
            executables = [
                ("--aw", "aw"),
                ("--tokenless", "tokenless"),
                ("--tokenless-provider", "aw-provider-tokenless"),
            ]
            if agent == "agentscope":
                executables.append(("--sec-core-provider", "aw-provider-sec-core"))
            for flag, name in executables:
                self.assertEqual(argv[argv.index(flag) + 1], str(self.root / name))
