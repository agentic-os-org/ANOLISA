"""Validate target projections and bound the authoritative parser subprocess."""

import json
import os
import subprocess
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

from aw_sdk import AwError
from aw_sdk.configuration import load_configuration


class ConfigurationTests(unittest.TestCase):
    profile = {"adapter": "fixture", "version": "v1", "entrypoint": "worker"}
    projection = {
        "target": "app",
        "daemon": {"endpoint": "auto", "state_dir": "/service"},
        "input_enabled": False,
        "target_requires_service": False,
    }

    def test_parser_arguments_and_target_availability(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aw-config-projection-") as directory:
            root = Path(directory)
            executable = root / "parser"
            for target in (None, "app", "-app"):
                for required in (False, True):
                    document = {
                        **self.projection, "target": target or "app",
                        "target_requires_service": required,
                    }
                    executable.write_text(f"#!{sys.executable}\nprint({json.dumps(document)!r})\n")
                    executable.chmod(0o700)
                    for requires_service in (False, True):
                        with patch(
                            "aw_sdk.configuration.subprocess.Popen", wraps=subprocess.Popen
                        ) as popen:
                            wiring = load_configuration(
                                "/config", self.profile, target, str(executable),
                                requires_service=requires_service,
                            )
                        expected = [
                            str(executable), "config", "show", "--config", "/config",
                            "--sdk-target",
                            "fixture" if target is None else f"fixture/{target}",
                        ]
                        self.assertEqual(popen.call_args.args[0], expected)
                        self.assertEqual(wiring["target"], target or "app")
                        self.assertEqual(wiring["endpoint"], "/service/aw.sock")
                        self.assertFalse(wiring["input_enabled"])
                        self.assertEqual(
                            wiring["availability"],
                            "required" if requires_service or required else "native_fallback",
                        )

    def test_explicit_target_is_preserved_without_splitting_or_fallback(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aw-config-selection-") as directory:
            executable = Path(directory) / "parser"
            document = json.dumps({"target": None})
            executable.write_text(f"#!{sys.executable}\nprint({document!r})\n")
            executable.chmod(0o700)
            for target in ("", "app/other", "/app", "app/"):
                with patch(
                    "aw_sdk.configuration.subprocess.Popen", wraps=subprocess.Popen
                ) as popen:
                    with self.assertRaisesRegex(AwError, "sdk_target_required"):
                        load_configuration("/config", self.profile, target, str(executable))
                self.assertEqual(
                    popen.call_args.args[0][-2:], ["--sdk-target", f"fixture/{target}"]
                )

    def test_projection_booleans_are_strict_even_when_service_is_required(self) -> None:
        for key in ("input_enabled", "target_requires_service"):
            for invalid in (None, 0, 1, "false", [], {}):
                document = {**self.projection, key: invalid}
                with patch(
                    "aw_sdk.configuration._configuration_output",
                    return_value=json.dumps(document),
                ):
                    with self.assertRaisesRegex(AwError, "sdk_configuration_invalid"):
                        load_configuration("/config", self.profile, None, "aw", requires_service=True)
            document = dict(self.projection)
            del document[key]
            with patch(
                "aw_sdk.configuration._configuration_output", return_value=json.dumps(document)
            ):
                with self.assertRaisesRegex(AwError, "sdk_configuration_invalid"):
                    load_configuration("/config", self.profile, None, "aw")

    def test_missing_or_mismatched_target_keeps_its_error_before_projection_checks(self) -> None:
        for selected, requested in ((None, None), (None, "missing"), ("other", "app"), ("", None)):
            with patch(
                "aw_sdk.configuration._configuration_output",
                return_value=json.dumps({"target": selected}),
            ):
                with self.assertRaisesRegex(AwError, "sdk_target_required"):
                    load_configuration("/config", self.profile, requested, "aw")

    def test_parser_rejection_does_not_fall_back_to_native(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aw-config-legacy-") as directory:
            executable = Path(directory) / "parser"
            executable.write_text(
                f"#!{sys.executable}\nimport sys\n"
                "sys.stderr.write('unsupported argument --sdk-target')\nsys.exit(2)\n"
            )
            executable.chmod(0o700)
            with self.assertRaises(subprocess.CalledProcessError) as raised:
                load_configuration("/config", self.profile, None, str(executable))
            self.assertEqual(raised.exception.returncode, 2)

    def test_stdout_and_stderr_overflow_stop_and_reap_the_writer(self) -> None:
        with tempfile.TemporaryDirectory(prefix="aw-config-limit-", dir="/tmp") as directory:
            root = Path(directory)
            for fd in (1, 2):
                with self.subTest(fd=fd):
                    executable = root / "writer"
                    executable.write_text(
                        f"#!{sys.executable}\nimport os\nfrom pathlib import Path\n"
                        f"Path({str(root / 'pid')!r}).write_text(str(os.getpid()))\n"
                        f"while True: os.write({fd}, b'x' * 65536)\n"
                    )
                    executable.chmod(0o700)
                    with self.assertRaisesRegex(AwError, "sdk_configuration_too_large"):
                        load_configuration(
                            str(root / "aw.yaml"),
                            {"adapter": "fixture", "version": "v1", "entrypoint": "worker"},
                            None,
                            str(executable),
                        )
                    pid = int((root / "pid").read_text())
                    self.assertFalse(Path(f"/proc/{pid}").exists())
