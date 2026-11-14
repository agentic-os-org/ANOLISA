"""Keep Qoder SDK native credentials out of installation and require acceptance reports."""

import hashlib
import json
import subprocess
import os
from pathlib import Path
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

        def run(argv, cwd, timeout=600, **kwargs):
            self.assertNotIn("QODER_PERSONAL_ACCESS_TOKEN", commands.environment)
            observed.append(argv)
            if argv[:2] == ["npm", "install"]:
                directory = Path(argv[argv.index("--prefix") + 1])
                directory.mkdir(exist_ok=True)
                (directory / "package-lock.json").write_text("{}")
            return "{}"

        with patch.object(commands, "run", side_effect=run):
            native_acceptance.setup(row, work, commands)
        install = next(index for index, argv in enumerate(observed) if argv[:2] == ["npm", "install"])
        self.assertIn(row["npm_packages"][0], observed[install])
        self.assertEqual(observed[install + 1], [
            "node", str(work / "npm/node_modules/@qoder-ai/qoder-agent-sdk/scripts/postinstall.cjs")
        ])
        for key, package in row["npm_variants"].items():
            install = next(index for index, argv in enumerate(observed) if package in argv)
            self.assertEqual(observed[install + 1], [
                "node", str(work / key / "node_modules/@qoder-ai/qoder-agent-sdk/scripts/postinstall.cjs")
            ])
            self.assertTrue((evidence / f"{key}-lock.json").is_file())

    def test_sdk_runtime_alone_receives_the_pat_and_missing_pat_fails(self) -> None:
        manifest = load(AW)
        row = manifest["agents"]["qoder-sdk"]
        for credential in ("", "test-private-token"):
            output = self.root / ("with-token" if credential else "without-token")
            observed = []

            def setup(_row, work, commands):
                self.assertNotIn("QODER_PERSONAL_ACCESS_TOKEN", commands.environment)
                observed.append("setup")
                return {"python": sys.executable, "npm": str(work / "npm"),
                        **{key: str(work / key) for key in row["npm_variants"]}}

            def command(commands, argv, cwd, timeout=600, **kwargs):
                if any(path in argv for path in (
                    "crates/aw-cli/tests/fixtures/qoder_sdk/native-profile.test.mjs",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/native-command.test.mjs",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/deadline-native.test.mjs",
                )):
                    self.assertEqual(argv[-1], str(self.root / "bin/aw"))
                if any(path in argv for path in (
                    "crates/aw-cli/tests/fixtures/sec_core/skill_native.py",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/native.py",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/native-direct.py",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/post-native.py",
                    "crates/aw-cli/tests/fixtures/sec_core/input_submission.py",
                    "crates/aw-cli/tests/fixtures/sec_core/tool_pii.py",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/parser.test.mjs",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/daemon-native.test.mjs",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/native-profile.test.mjs",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/native-command.test.mjs",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/deadline-native.test.mjs",
                    "crates/aw-cli/tests/fixtures/qoder_sdk/release_lifecycle.py",
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
                    self.assertEqual(observed, ["setup"] + ["native"] * 26)
                    self.assertEqual(reports.call_count, 26)
                else:
                    with self.assertRaisesRegex(ValueError, "requires.*Secret"):
                        native_acceptance.main()
                    self.assertEqual(observed, [])
                    reports.assert_not_called()
            self.assertEqual(
                json.loads((output / "result.json").read_text())["status"],
                "passed" if credential else "failed",
            )

    def test_worker_fingerprints_require_the_declared_target_and_bytes(self) -> None:
        script = AW / "crates/aw-cli/tests/fixtures/qoder_sdk/release_lifecycle.py"
        package = self.root / "package"
        worker = package / "dist/_worker"
        worker.mkdir(parents=True)
        (package / "package.json").write_text(json.dumps({"version": "fixture"}))
        (package / "dist/index.js").write_text("sdk")
        content = b"firePostToolUseEvent getUpdatedToolOutput createSuccessResult"
        (worker / "qoder-worker-runtime.obf.mjs").write_bytes(content)
        for target in ["linux-arm64", "linux-x64"]:
            for valid in [True, False]:
                (worker / "runtime-info.json").write_text(json.dumps({"version": "worker", "target": target}))
                output = self.root / (target + str(valid))
                command = [sys.executable, str(script), "--package", str(package),
                           "--sdk-version", "fixture", "--worker-version", "worker",
                           "--sdk-sha256", hashlib.sha256(b"sdk").hexdigest(),
                           "--worker-target", target, "--worker-sha256",
                           hashlib.sha256(content).hexdigest() if valid else "0" * 64,
                           "--output", str(output)]
                result = subprocess.run(command, capture_output=True, timeout=5)
                self.assertEqual(result.returncode == 0, valid)
                self.assertEqual((output / "result.json").exists(), valid)
                if valid:
                    report = json.loads((output / "result.json").read_text())
                    self.assertEqual(report["worker_target"], target)
                    self.assertEqual(report["worker_sha256"], hashlib.sha256(content).hexdigest())
            command[command.index("--worker-target") + 1] = "linux-x64" if target == "linux-arm64" else "linux-arm64"
            command[command.index("--worker-sha256") + 1] = hashlib.sha256(content).hexdigest()
            self.assertNotEqual(subprocess.run(command, capture_output=True, timeout=5).returncode, 0)
