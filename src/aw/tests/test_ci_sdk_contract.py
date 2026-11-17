"""SDK contract sockets and native packaging remain portable across runner setups."""

import importlib.util
import json
from pathlib import Path
import socket
import sys
import tempfile
import unittest
from unittest.mock import patch

from test_ci_checks import AW


class SdkContractTests(unittest.TestCase):
    def test_semantic_socket_uses_short_private_state_under_long_tmpdir(self) -> None:
        tests = AW / "sdk/tests"
        spec = importlib.util.spec_from_file_location(
            "semantic_contract", tests / "semantic_contract.py"
        )
        semantic = importlib.util.module_from_spec(spec)
        with patch.object(sys, "path", [str(tests), *sys.path]):
            spec.loader.exec_module(semantic)
        with tempfile.TemporaryDirectory(prefix="aw-semantic-path-", dir="/tmp") as directory:
            root = Path(directory) / ("long-runner-directory-" * 6)
            root.mkdir()
            seen = []

            def case(binary, command, output, environment, empty, state):
                state.mkdir(mode=0o700)
                self.assertGreater(len(str(output)), 108)
                with socket.socket(socket.AF_UNIX) as server:
                    server.bind(str(state / "aw.sock"))
                seen.append(empty)

            with patch.object(semantic, "run_case", side_effect=case), patch.dict(
                "os.environ", {"TMPDIR": str(root)}
            ):
                semantic.run("aw", [], root, {})
            self.assertEqual(seen, [False, True])

    def test_native_package_profiles_seed_pip_and_build_backend(self) -> None:
        manifest = json.loads((AW / "tests/native-matrix.json").read_text())
        tools = {"pip==24.0", "setuptools==68.1.2", "wheel==0.42.0"}
        for agent in ("qwenpaw", "sdk"):
            self.assertTrue(tools <= set(manifest["agents"][agent]["requirements"]))
        row = manifest["agents"]["qwenpaw"]
        cases = [
            case
            for case in row["cases"]
            if case["name"]
            in {
                "after-only-sdk",
                "configured-after-sdk",
                "configured-before-sdk",
                "configured-input-sdk",
            }
        ]
        self.assertEqual(len(cases), 4)
        self.assertTrue(all(case["argv"][0] == "{python}" for case in cases))
        for case in cases:
            self.assertTrue(
                all(result["file"].startswith("fixtures/") for result in case["results"])
            )
