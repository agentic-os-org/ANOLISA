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
                if "crates/aw-cli/tests/fixtures/qoder_sdk/native.py" in argv:
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
                    self.assertEqual(observed, ["setup", "native", "native"])
                    self.assertEqual(reports.call_count, 2)
                else:
                    with self.assertRaisesRegex(ValueError, "requires.*Secret"):
                        native_acceptance.main()
                    self.assertEqual(observed, [])
                    reports.assert_not_called()
            self.assertEqual(
                json.loads((output / "result.json").read_text())["status"],
                "passed" if credential else "failed",
            )
