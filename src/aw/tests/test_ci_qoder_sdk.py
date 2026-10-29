"""Keep Qoder SDK native credentials out of installation and require acceptance reports."""

import json
import os
import sys
from unittest.mock import patch

from test_ci_checks import AW, GateFixture
from check_native import load
from native_acceptance import Commands
import native_acceptance


class QoderSdkNativeTests(GateFixture):
    def test_pinned_sdk_installs_its_worker_without_credentials(self) -> None:
        row = load(AW)["agents"]["qoder-sdk"]
        work, evidence = self.root / "work", self.root / "evidence"
        work.mkdir()
        evidence.mkdir()
        commands = Commands(evidence, {})
        observed = []

        def run(argv, cwd, **kwargs):
            self.assertNotIn("QODER_PERSONAL_ACCESS_TOKEN", commands.environment)
            observed.append(argv)
            return "{}"

        with patch.object(commands, "run", side_effect=run):
            native_acceptance.setup(row, work, commands)
        install = next(index for index, argv in enumerate(observed) if argv[:2] == ["npm", "install"])
        self.assertIn(row["npm_packages"][0], observed[install])
        self.assertEqual(observed[install + 1], [
            "node", str(work / "npm/node_modules/@qoder-ai/qoder-agent-sdk/scripts/postinstall.cjs")
        ])

    def test_sdk_runtime_alone_receives_the_pat_and_missing_pat_fails(self) -> None:
        manifest = load(AW)
        row = manifest["agents"]["qoder-sdk"]
        for credential in ("", "test-private-token"):
            output = self.root / ("with-token" if credential else "without-token")
            observed = []

            def setup(_row, work, commands):
                self.assertNotIn("QODER_PERSONAL_ACCESS_TOKEN", commands.environment)
                observed.append("setup")
                return {"python": sys.executable, "npm": str(work / "npm")}

            def command(commands, argv, cwd, timeout=600, **kwargs):
                if any(path in argv for path in (
                    "crates/aw-cli/tests/fixtures/qoder_sdk/native-profile.test.mjs",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/native-command.test.mjs",
                )):
                    self.assertEqual(argv[-1], str(self.root / "bin/aw"))
                if any(path in argv for path in (
                    "crates/aw-cli/tests/fixtures/qoder_sdk/native.py",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/post-native.py",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/parser.test.mjs",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/daemon-native.test.mjs",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/native-profile.test.mjs",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/native-command.test.mjs",
                )):
                    self.assertEqual(
                        commands.environment["QODER_PERSONAL_ACCESS_TOKEN"], credential
                    )
                    observed.append("native")
                else:
                    self.assertNotIn("QODER_PERSONAL_ACCESS_TOKEN", commands.environment)
                return ""

            argv = [
                "native_acceptance.py",
                "--agent",
                "qoder-sdk",
                "--candidate-sha",
                "a" * 40,
                "--bin",
                str(self.root / "bin"),
                "--output",
                str(output),
            ]
            with patch.object(sys, "argv", argv), patch.dict(
                os.environ, {"QODER_PERSONAL_ACCESS_TOKEN": credential}
            ), patch.object(native_acceptance, "plan"), patch.object(
                native_acceptance, "verify_bundle"
            ), patch.object(
                native_acceptance, "load", return_value=manifest
            ), patch.object(
                native_acceptance.platform, "python_version", return_value=row["python"]
            ), patch.object(
                native_acceptance, "setup", side_effect=setup
            ), patch.object(
                Commands, "run", autospec=True, side_effect=command
            ), patch.object(
                native_acceptance, "verify_case"
            ) as reports:
                if credential:
                    native_acceptance.main()
                    self.assertEqual(observed, ["setup"] + ["native"] * 8)
                    self.assertEqual(reports.call_count, 8)
                else:
                    with self.assertRaisesRegex(ValueError, "requires.*Secret"):
                        native_acceptance.main()
                    self.assertEqual(observed, [])
                    reports.assert_not_called()
            self.assertEqual(
                json.loads((output / "result.json").read_text())["status"],
                "passed" if credential else "failed",
            )
