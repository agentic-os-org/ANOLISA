"""Reject false native security passes without requiring containers or account keys."""

import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from test_ci_checks import AW, GateFixture
from check_native import load
from native_qoder import validate_pin
from native_acceptance import redact_logs
from native_cleanup import remove_containers

FIXTURE = AW / "crates/aw-cli/tests/fixtures/sec_core"
sys.path.insert(0, str(FIXTURE))
SPEC = importlib.util.spec_from_file_location("aw_sec_native", FIXTURE / "native.py")
NATIVE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(NATIVE)
import runtime


class SecurityEvidenceTests(unittest.TestCase):
    def evidence(self, mode):
        success = mode in ("allow", "scan-deny")
        return {
            "agent": "qoder",
            "case": mode,
            "before": 1,
            "after": 1 if mode == "allow" else 0,
            "tools": ["Bash"],
            "marker_exists": mode == "allow",
            "sentinel_exists": True,
            "scan_calls": [
                {
                    "status": "ok" if success else "error",
                    "failure": None if success else "transport",
                    "failure_action": None if success else "block",
                    "policy_block": mode == "scan-deny",
                    "call": {"elapsed_ms": 2005},
                }
            ],
            "fault_probe_exit": 1,
            "rule_hits": 2,
            "sec_verdicts": (["warn", "warn"] if mode == "scan-deny" else ["pass", "pass"]),
            "final_status": {"result": {"audit_healthy": True, "bindings": 0, "events": 0}},
        }

    def test_tool_side_effects_and_native_callbacks_are_required(self):
        for mode in NATIVE.CASES:
            good = self.evidence(mode)
            NATIVE.verify_outcome(good)
            for field, value in [
                ("before", 0),
                ("after", 2),
                ("tools", ["unmapped"]),
                ("marker_exists", mode != "allow"),
                ("sentinel_exists", False),
                ("scan_calls", []),
            ]:
                bad = copy.deepcopy(good)
                bad[field] = value
                with self.subTest(mode=mode, field=field), self.assertRaises(AssertionError):
                    NATIVE.verify_outcome(bad)

    def test_unscanned_fail_open_and_short_timeout_reports_are_rejected(self):
        mutations = [
            ("scan-deny", lambda r: r.update(sec_verdicts=["warn"])),
            ("scan-deny", lambda r: r.update(rule_hits=1)),
            ("scan-deny", lambda r: r["scan_calls"][0].update(policy_block=False)),
            ("allow", lambda r: r["scan_calls"][0].update(policy_block=True)),
            (
                "backend-error",
                lambda r: r["scan_calls"][0].update(failure_action="report"),
            ),
            ("backend-error", lambda r: r["scan_calls"][0].update(status="ok")),
            (
                "timeout-block",
                lambda r: r["scan_calls"][0]["call"].update(elapsed_ms=10),
            ),
            ("timeout-block", lambda r: r.update(fault_probe_exit=0)),
            (
                "allow",
                lambda r: r["final_status"]["result"].update(audit_healthy=False),
            ),
            ("allow", lambda r: r["final_status"]["result"].update(bindings=1)),
        ]
        for mode, mutate in mutations:
            report = self.evidence(mode)
            mutate(report)
            with self.subTest(mode=mode), self.assertRaises(AssertionError):
                NATIVE.verify_outcome(report)

    def test_qwenpaw_blocked_calls_require_native_after(self):
        for mode in NATIVE.CASES:
            report = self.evidence(mode)
            report.update(agent="qwenpaw", tools=["execute_shell_command"], after=1)
            NATIVE.verify_outcome(report)
            report["after"] = 0
            with self.assertRaises(AssertionError):
                NATIVE.verify_outcome(report)


class SecurityInventoryTests(GateFixture):
    def test_socket_lookup_survives_container_ownership_handoff(self) -> None:
        for mask in (0o077, 0o027):
            with self.subTest(umask=oct(mask)):
                case = self.root / str(mask)
                case.mkdir(mode=0o700)
                previous = os.umask(mask)
                try:
                    backend = runtime.Backend(case, case, None, case, "fixture-image")
                finally:
                    os.umask(previous)

                def handoff(command: list[str]) -> None:
                    self.assertEqual(command[:2], ["docker", "create"])
                    self.assertEqual(case.stat().st_mode & 0o777, 0o700)
                    # At root ownership transfer, the host runner has only the
                    # directory's "other" permissions. Exercise that access in an
                    # unprivileged child; root runners drop the child's identity
                    # after opening the directory so private ancestors stay private.
                    probe = subprocess.run(
                        [
                            sys.executable,
                            "-B",
                            "-c",
                            "import os,sys\n"
                            "from pathlib import Path\n"
                            "directory = Path(sys.argv[1])\n"
                            "fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)\n"
                            "mode = os.fstat(fd).st_mode & 0o777\n"
                            "try:\n"
                            " os.fchdir(fd)\n"
                            " if os.geteuid() == 0:\n"
                            "  os.fchown(fd, 65534, 65534)\n"
                            "  os.setgroups([])\n"
                            "  os.setgid(65534)\n"
                            "  os.setuid(65534)\n"
                            " assert os.geteuid() != 0\n"
                            " os.fchmod(fd, (mode & 0o007) << 6)\n"
                            " try: Path('daemon.sock').stat()\n"
                            " except FileNotFoundError: pass\n"
                            " else: raise AssertionError('unexpected socket')\n"
                            "finally:\n"
                            " os.fchmod(fd, mode)\n"
                            " os.close(fd)\n",
                            str(backend.runtime),
                        ],
                        capture_output=True,
                        text=True,
                        timeout=5,
                    )
                    self.assertEqual(probe.returncode, 0, probe.stderr)
                    raise InterruptedError("container handoff verified")

                with patch.object(runtime, "checked", side_effect=handoff):
                    with self.assertRaisesRegex(InterruptedError, "handoff verified"):
                        backend.start()

    def test_emergency_cleanup_selects_owned_containers_and_removes_after_exec_failure(self):
        for fail_exec in (False, True):
            calls = []

            def docker(command, **kwargs):
                self.assertNotIn("QODER_PERSONAL_ACCESS_TOKEN", kwargs["env"])
                calls.append(command)
                if command[1] == "ps":
                    self.assertEqual(command[-1], "label=aw.native-owner=fixture-owner")
                    return "owned-id\n" if len(calls) == 1 else ""
                if command[1] == "inspect":
                    return '{"Running":true,"Paused":true}'
                if command[1] == "exec" and fail_exec:
                    raise subprocess.TimeoutExpired(command, 15)
                return ""

            with patch("native_cleanup.subprocess.check_output", side_effect=docker):
                environment = {"QODER_PERSONAL_ACCESS_TOKEN": "test-private-token"}
                if fail_exec:
                    with self.assertRaisesRegex(RuntimeError, "cleanup failed"):
                        remove_containers("fixture-owner", environment)
                else:
                    self.assertEqual(remove_containers("fixture-owner", environment), ["owned-id"])
            self.assertIn(["docker", "unpause", "owned-id"], calls)
            self.assertIn(["docker", "rm", "-f", "owned-id"], calls)

    def test_backend_helpers_and_version_probes_do_not_inherit_the_qoder_token(self):
        code = "import json,os; print(json.dumps('QODER_PERSONAL_ACCESS_TOKEN' in os.environ))"
        with patch.dict(os.environ, {"QODER_PERSONAL_ACCESS_TOKEN": "test-private-token"}):
            probe = runtime.checked([sys.executable, "-c", code], env=dict(os.environ))
            self.assertFalse(json.loads(probe.stdout))
            cli = self.root / "agent-sec-cli"
            cli.write_text(f"#!{sys.executable}\n{code}\n")
            cli.chmod(0o700)
            backend = runtime.Backend(self.root, self.root, None, self.root, "fixture-image")
            self.assertFalse(json.loads(backend.scan("printf test")["stdout"]))
            backend.events()
            self.assertFalse(json.loads((self.root / "sec-events.json").read_text()))

    def test_cleanup_failures_keep_the_case_report_and_remaining_evidence(self):
        for failure in ("backend", "processes", "evidence"):
            with self.subTest(failure=failure):
                root = self.root / failure
                root.mkdir()
                output = root / "output"
                output.mkdir()
                args = SimpleNamespace(
                    agent="qoder",
                    output=output,
                    bin=root,
                    image="fixture-image",
                    agent_bin=Path("/bin/true"),
                    qoder_auth_dir=None,
                )
                journal = root / "allow/state/journal/records.jsonl"

                def start():
                    journal.parent.mkdir()
                    journal.write_text('{"evidence":"retained"}\n')

                def collect(_report, case, _evidence, _state):
                    (case / "work/marker.txt").write_text("42\n")

                with patch.object(NATIVE, "Backend") as backend_type, patch.object(
                    NATIVE, "Processes"
                ) as process_type, patch.object(NATIVE, "checked") as checked, patch.object(
                    NATIVE, "await_condition"
                ), patch.object(
                    NATIVE, "collect", side_effect=collect
                ), patch.object(
                    NATIVE.time, "sleep"
                ), patch.dict(
                    os.environ, {"QODER_PERSONAL_ACCESS_TOKEN": "test-private-token"}
                ), patch.object(
                    sys, "stdout", new=io.StringIO()
                ):
                    backend = backend_type.return_value
                    backend.start.side_effect = start
                    backend.scan.return_value = {
                        "exit_code": 0,
                        "stdout": '{"ok":true,"verdict":"pass"}',
                    }
                    backend.record = {"removed": True}
                    backend.socket = root / "absent.sock"
                    processes = process_type.return_value
                    processes.exited.return_value = False
                    processes.wait.return_value = 0
                    processes.items = []
                    checked.return_value.stdout = "{}"
                    if failure in ("backend", "processes"):
                        target = backend if failure == "backend" else processes
                        target.close.side_effect = OSError("cleanup unavailable")
                        report = NATIVE.run(args, "allow", root)
                    else:
                        with patch.object(
                            NATIVE.shutil, "copytree", side_effect=OSError("copy unavailable")
                        ):
                            report = NATIVE.run(args, "allow", root)
                    backend.close.assert_called_once()
                    processes.close.assert_called_once()
                self.assertFalse(report["passed"])
                self.assertIn(failure, report["cleanup_errors"])
                self.assertEqual(json.loads((output / "allow/result.json").read_text()), report)
                if failure != "evidence":
                    self.assertEqual(
                        (output / "allow/journal/records.jsonl").read_text(), journal.read_text()
                    )
                    self.assertTrue((output / "allow/tool-command.txt").is_file())

    def test_retained_logs_redact_the_supplied_account_token(self):
        nested = self.root / "case"
        nested.mkdir()
        for path in (self.root / "command.log", nested / "agent.log"):
            path.write_text("failure test-private-token; tool denied\n")
        redact_logs(self.root, "test-private-token")
        for path in self.root.rglob("*.log"):
            self.assertEqual(path.read_text(), "failure [REDACTED]; tool denied\n")

    def test_failed_evidence_copy_still_removes_the_owned_container(self):
        backend = runtime.Backend(self.root, self.root, None, self.root, "fixture-image")
        backend.created = True
        calls = []

        def command(argv, **_kwargs):
            calls.append(argv)
            if argv[:2] == ["docker", "cp"]:
                raise subprocess.CalledProcessError(1, argv)
            return SimpleNamespace(stdout="")

        with patch.object(runtime, "checked", command):
            with self.assertRaises(subprocess.CalledProcessError):
                backend.close()
        self.assertIn(["docker", "rm", "-f", backend.name], calls)
        self.assertTrue(backend.record["removed"])

    def test_ledger_failure_does_not_skip_other_process_cleanup(self):
        processes = runtime.Processes(self.root, self.root, {})
        first, second = object(), object()
        processes.items = [[first, None, {}], [second, None, {}]]
        calls = []

        def stop(child):
            calls.append(child)
            if child is second:
                raise OSError("ledger unavailable after reaping")

        with patch.object(processes, "stop", stop):
            with self.assertRaises(OSError):
                processes.close()
        self.assertEqual(calls, [second, first])

    def test_security_suites_cannot_disappear_or_become_manual(self):
        original = Path.read_text
        manifest = load(AW)
        for agent in ("qoder", "qwenpaw"):
            for omission in (
                "suite",
                "timeout",
                "manual",
                "container",
                "mutable-image",
                "entrypoint",
                "agent",
                "image",
                "agent-bin",
                "bin",
                "duplicate-agent",
                "python-option",
            ):
                edited = copy.deepcopy(manifest)
                row = edited["agents"][agent]
                suite = next(case for case in row["cases"] if case["name"] == "sec-core")
                if omission == "suite":
                    row["cases"].remove(suite)
                elif omission == "timeout":
                    suite["results"][0]["cases"]["names"].remove("timeout-block")
                elif omission == "manual":
                    row["mode"] = "manual"
                elif omission == "container":
                    del row["container"]
                elif omission == "mutable-image":
                    row["container"] = "ubuntu:24.04"
                    suite["argv"][suite["argv"].index("--image") + 1] = row["container"]
                elif omission == "entrypoint":
                    suite["argv"][1] = "tests/test_claude_sdk_native.py"
                elif omission == "duplicate-agent":
                    suite["argv"] += ["--agent", "qwenpaw"]
                elif omission == "python-option":
                    suite["argv"].insert(1, "-c")
                else:
                    suite["argv"][suite["argv"].index("--" + omission) + 1] = "wrong"

                def read(path, *args, **kwargs):
                    if path == AW / "tests/native-matrix.json":
                        return json.dumps(edited)
                    return original(path, *args, **kwargs)

                with self.subTest(agent=agent, omission=omission), patch.object(
                    Path, "read_text", read
                ):
                    with self.assertRaises(ValueError):
                        load(AW)

    def test_qoder_download_requires_reviewed_version_and_checksum(self):
        pins = load(AW)["agents"]["qoder"]
        for key in ("qoder_cli", "qoder_reload_cli"):
            pin = pins[key]
            validate_pin(pin)
            for malformed in ([], {**pin, "downloads": {"x86_64": [], "aarch64": {}}}):
                with self.subTest(key=key, malformed=malformed):
                    with self.assertRaisesRegex(ValueError, "Qoder"):
                        validate_pin(malformed)
            for mutation in ("checksum", "version", "url"):
                edited = copy.deepcopy(pin)
                if mutation == "version":
                    edited["version"] = "latest"
                else:
                    edited["downloads"]["x86_64"]["sha256" if mutation == "checksum" else "url"] = ""
                with self.assertRaises(ValueError):
                    validate_pin(edited)

    def test_missing_qoder_credential_writes_failed_evidence(self):
        output = self.root / "missing-credential"
        argv = [
            "native.py",
            "--agent",
            "qoder",
            "--bin",
            str(self.root),
            "--agent-bin",
            "/bin/true",
            "--image",
            "ubuntu:24.04",
            "--output",
            str(output),
        ]
        with patch.object(sys, "argv", argv), patch.dict(os.environ, {}, clear=True), patch.object(
            NATIVE, "checked"
        ) as checked:
            checked.return_value.stdout = "1.1.64\n"
            with self.assertRaisesRegex(ValueError, "QODER_PERSONAL_ACCESS_TOKEN"):
                NATIVE.main()
        result = json.loads((output / "results.json").read_text())
        self.assertFalse(result["passed"])
        self.assertEqual(result["cases"], [])


if __name__ == "__main__":
    unittest.main()
