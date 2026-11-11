"""Drive command permissions through a real Cosh and an isolated, live daemon.

The caller provides a fresh root-owned daemon with real intent-provider credentials
and a private /etc mount containing the installed permissions template. Credentials
stay outside this driver. Its local model fixture selects one synthetic shell call;
the production intent endpoint generates the permission candidate.
"""

import argparse
import contextlib
import fcntl
import json
import os
import pty
import re
import select
import shlex
import signal
import socket
import socketserver
import sqlite3
import struct
import subprocess
import sys
import termios
import threading
import time
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from all_shell_host import (
    configure_sandbox_extension,
    install_sandbox_extension,
    run_all_shell_host,
)

CASES = (
    "allowed",
    "task-deny",
    "preset-forbid",
    "empty-candidate",
    "unknown-script",
    "sudo-original",
    "intent-malformed",
    "intent-clarification",
    "intent-timeout",
    "unsupported-host",
    "fixed",
    "tty-allow",
    "tty-deny",
)
FAULTS = {"intent-malformed", "intent-clarification", "intent-timeout"}
TTY_CASES = {"tty-allow", "tty-deny", "unknown-script"}
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


def save(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")
    path.chmod(0o600)


def records(path: Path) -> list[dict]:
    return (
        [json.loads(line) for line in path.read_text().splitlines()]
        if path.exists()
        else []
    )


def pick(value: dict, names: tuple[str, ...]) -> dict:
    return {name: value[name] for name in names if name in value}


def allows_write(permissions: dict, target: Path) -> bool:
    matched = [
        rule
        for rule in permissions["filesystem"]["rules"]
        if "write" in rule["rights"]
        and (target == Path(rule["path"]) or Path(rule["path"]) in target.parents)
    ]
    return any(rule["effect"] == "allow" for rule in matched) and not any(
        rule["effect"] == "deny" for rule in matched
    )


def check_receipt(row: dict) -> None:
    """Bind observed execution to this decision, including denied approvals."""
    decision = row["authorization"]["response"]
    receipt = row["receipts"][0]
    assert receipt["decision_id"] == decision["decision_id"], row
    assert receipt["policy_version"] == decision["policy_version"], row
    result = receipt["result"]
    execute = [item for item in row["claim_phases"] if item.get("phase") == "execute"]
    if row["case"] == "tty-deny":
        assert (
            receipt["approval"] is None and not result["started"] and not execute
        ), row
        assert receipt["host_finished"] is True, row
    else:
        assert result["installed"] and result["started"] and len(execute) == 1, row
        if row["case"] not in {"task-deny", "preset-forbid", "sudo-original"}:
            assert result["exit_code"] == 0 and result["error"] is None, row
        if row["case"] in TTY_CASES:
            assert receipt["approval"] is True, row


def rpc(endpoint: Path, method: str, params: dict | None = None) -> dict:
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(45)
        stream.connect(str(endpoint))
        stream.sendall(
            json.dumps({"method": method, "params": params or {}}).encode() + b"\n"
        )
        return json.loads(stream.makefile("rb").readline(4 * 1024 * 1024))


def stop(process: subprocess.Popen) -> None:
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)


@contextlib.contextmanager
def trace_protocol(endpoint: Path, tap: Path, evidence: Path) -> Iterator[None]:
    """Relay exact bytes; retain only fields that cannot contain launch.env."""

    class Handler(socketserver.StreamRequestHandler):
        def handle(self) -> None:
            request = self.rfile.readline(4 * 1024 * 1024)
            with socket.socket(socket.AF_UNIX) as upstream:
                upstream.settimeout(60)
                upstream.connect(str(endpoint))
                upstream.sendall(request)
                response = upstream.makefile("rb").readline(4 * 1024 * 1024)
            incoming, outgoing = json.loads(request), json.loads(response)
            method, params = incoming["method"], incoming.get("params", {})
            result = outgoing.get("result", {})
            row = {"method": method, "ok": "result" in outgoing}
            if method == "intent.propose_command":
                row["request_keys"] = sorted(params)
                row["request"] = pick(params, ("format_version", "command", "cwd"))
                row["response"] = pick(
                    result, ("outcome", "policy", "caveats", "reasons")
                )
            elif method == "authorization.evaluate":
                row["request"] = pick(params, ("principal", "requested_permissions"))
                row["request"]["launch"] = pick(
                    params["launch"], ("program", "resolved_program", "args", "cwd")
                )
                row["response"] = pick(
                    result,
                    (
                        "decision_id",
                        "verdict",
                        "reason",
                        "policy_version",
                        "effective_permissions",
                        "warnings",
                    ),
                )
            elif method == "authorization.claim":
                row.update(pick(params, ("phase", "decision_id")))
            else:
                row = None
            if row is not None:
                with self.server.record_lock, evidence.open("a") as log:
                    log.write(json.dumps(row) + "\n")
            self.wfile.write(response)

    with socketserver.ThreadingUnixStreamServer(str(tap), Handler) as server:
        server.record_lock = threading.Lock()
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield
        finally:
            server.shutdown()
            thread.join()
            tap.unlink(missing_ok=True)


@contextlib.contextmanager
def model(command: str) -> Iterator[tuple[int, list, type]]:
    """Retain only tool results, never full provider requests or model context."""
    results = []

    class Model(BaseHTTPRequestHandler):
        calls = 0

        def log_message(self, *_args: object) -> None:
            pass

        def do_POST(self) -> None:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            Model.calls += 1
            results.extend(
                message["content"]
                for message in request["messages"]
                if message["role"] == "tool"
            )
            delta = (
                {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "command-permission-call",
                            "type": "function",
                            "function": {
                                "name": "shell",
                                "arguments": json.dumps({"command": command}),
                            },
                        }
                    ]
                }
                if Model.calls == 1
                else {"content": "DYNAMIC_HOST_COMPLETE"}
            )
            chunks = [(delta, None), ({}, "tool_calls" if Model.calls == 1 else "stop")]
            body = (
                "".join(
                    "data: "
                    + json.dumps(
                        {
                            "choices": [
                                {"index": 0, "delta": delta, "finish_reason": finish}
                            ]
                        }
                    )
                    + "\n\n"
                    for delta, finish in chunks
                )
                + "data: [DONE]\n\n"
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    with ThreadingHTTPServer(("127.0.0.1", 0), Model) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield server.server_port, results, Model
        finally:
            server.shutdown()
            thread.join()


class Harness:
    def __init__(self, root: Path, binaries: Path, hook: Path) -> None:
        self.root, self.binaries, self.hook = root, binaries, hook
        self.endpoint, self.tap = root / "daemon.sock", root / "tap.sock"
        self.audit, self.trace = (
            root / "audit/security-events.jsonl",
            root / "protocol.jsonl",
        )
        self.work, self.marker = root / "work", root / "work/marker"
        self.work.mkdir(exist_ok=True)
        (self.work / ".git").mkdir(exist_ok=True)
        self.protected = self.work / ".git/protected"
        self.protected.write_text("unchanged")
        self.runtime = root / "runtime.cedar"
        self.runtime.write_text(
            'permit(principal, action == SecCore::Action::"process.spawn", resource) when { resource.program == "/bin/bash" };\n'
            'permit(principal, action == SecCore::Action::"file.read", resource) when { resource.path == "/" || resource.path like "/*" };\n'
            'forbid(principal, action == SecCore::Action::"file.write", resource) when { resource.path == '
            + json.dumps(str(self.work / ".git"))
            + " || resource.path like "
            + json.dumps(str(self.work / ".git") + "/*")
            + ' };\nforbid(principal, action == SecCore::Action::"network.use", resource);\n'
        )
        self.policies = (
            json.loads((root / "policies.json").read_text())
            if (root / "policies.json").exists()
            else {}
        )
        listed = rpc(self.endpoint, "policy.templates.list")["result"]
        assert {item["policyId"] for item in listed["items"]} == {
            item["policyId"] for item in self.policies.values()
        }, "use a fresh, private PAP store"
        if not self.policies:
            self.publish(
                "baseline",
                'permit(principal is SecCore::Subject, action, resource) when { principal has agent_type && principal.agent_type == "cosh" };',
            )
            self.publish("task", self.task_text("allow"))

    def task_text(self, mode: str) -> str:
        if mode == "deny":
            return (
                'forbid(principal is SecCore::Subject, action == SecCore::Action::"file.write", resource) when { resource.path == '
                + json.dumps(str(self.marker))
                + " };"
            )
        rule = 'permit(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource) when { principal has agent_type && principal.agent_type == "cosh" };'
        return (
            '@advice("{\\"verdict\\":\\"escalate\\"}")\n' if mode == "ask" else ""
        ) + rule

    def publish(self, name: str, text: str) -> None:
        source = {
            "language": "cedar",
            "content": {
                "schema": {"schemaId": "seccore.system", "revision": 1},
                "text": text,
            },
        }
        if name in self.policies:
            old = self.policies[name]
            result = rpc(
                self.endpoint,
                "policy.templates.update",
                {
                    "policyId": old["policyId"],
                    "policyName": "command-test-" + name,
                    "template": source,
                },
            )
        else:
            result = rpc(
                self.endpoint,
                "policy.templates.create",
                {"policyName": "command-test-" + name, "template": source},
            )
        assert "result" in result, result
        self.policies[name] = result["result"]
        current = self.policies[name]
        assert (
            rpc(
                self.endpoint,
                "policy.templates.get",
                {"id": current["policyId"], "revision": current["revision"]},
            )["result"]
            == current
        )
        save(self.root / "policies.json", self.policies)

    def prepare(self, case: str, home: Path, port: int) -> dict:
        hook, manifest = install_sandbox_extension(home, self.hook)
        config = home / ".copilot-shell"
        (home / "tmp").mkdir()
        (home / "empty").mkdir()
        (config / "telemetry_disabled").touch()
        (config / "config.toml").write_text(
            '[ai]\nactive_provider="fixture"\n[ai.providers.fixture]\ntype="openai"\napi_key="synthetic"\nmodel="fixture"\nbase_url="http://127.0.0.1:'
            + str(port)
            + '/v1"\n[hooks]\nenabled=true\n'
        )
        configure_sandbox_extension(
            manifest,
            {
                "PreToolUse": (shlex.join([sys.executable, str(hook)]), 60000),
                "PostToolUse": (
                    shlex.join([sys.executable, str(hook)]),
                    15000,
                ),
                "PostToolUseFailure": (
                    shlex.join(
                        [
                            sys.executable,
                            str(hook.with_name("sandbox-failure-handler.py")),
                        ]
                    ),
                    15000,
                ),
            },
        )
        product = json.loads((self.hook.parents[1] / "cosh-extension.json").read_text())
        installed = json.loads(manifest.read_text())
        for event, groups in installed["hooks"].items():
            commands = {
                item["name"]: item["command"]
                for group in product["hooks"][event]
                for item in group["hooks"]
            }
            for group in groups:
                for item in group["hooks"]:
                    expected = commands[item["name"]].replace(
                        "${extensionPath}", str(manifest.parent)
                    )
                    assert shlex.split(item["command"])[1:] == shlex.split(expected)[1:]
                    assert Path(shlex.split(expected)[1]).is_file()
        if case == "unsupported-host":
            definition = json.loads(manifest.read_text())
            for group in definition["hooks"]["PreToolUse"]:
                for item in group["hooks"]:
                    item.pop("require_output_when_env", None)
            save(manifest, definition)
        env = {
            "PATH": str(self.binaries) + ":/usr/bin:/bin",
            "HOME": str(home),
            "USER": "root",
            "LOGNAME": "root",
            "SHELL": "/bin/bash",
            "LANG": "C.UTF-8",
            "TERM": "xterm-256color",
            "COSH_STATES_DIR": str(home / "states"),
            "XDG_DATA_HOME": str(home / "empty"),
            "XDG_DATA_DIRS": str(home / "empty"),
            "TMPDIR": str(home / "tmp"),
            "COSH_TELEMETRY_DISABLED_PATH": str(config / "telemetry_disabled"),
            "AGENT_SEC_DAEMON_SOCKET": str(self.tap),
            "AGENT_SEC_COSH_SANDBOX_ALL_SHELL": "1",
            "AGENT_SEC_COSH_SANDBOX_PERMISSION_SOURCE": "command",
            "AGENT_SEC_COSH_SANDBOX_RUNTIME_PERMISSIONS": str(self.runtime),
        }
        if case == "fixed":
            env.pop("AGENT_SEC_COSH_SANDBOX_PERMISSION_SOURCE")
            env.pop("AGENT_SEC_COSH_SANDBOX_RUNTIME_PERMISSIONS")
        if case in FAULTS:
            directory = home / "bin"
            directory.mkdir()
            shim = directory / "agent-sec-cli"
            response = (
                "time.sleep(45)"
                if case == "intent-timeout"
                else (
                    "print('[]')"
                    if case == "intent-malformed"
                    else 'print(\'{"outcome":"needs_clarification","reasons":["Specify the destination"]}\')'
                )
            )
            real = str(self.binaries / "agent-sec-cli")
            shim.write_text(
                f"#!{sys.executable}\nimport os,sys,time\nif 'intent' in sys.argv and 'propose-command' in sys.argv:\n    {response}\nelse:\n    os.execv({real!r}, [{real!r}] + sys.argv[1:])\n"
            )
            shim.chmod(0o700)
            env["PATH"] = str(directory) + ":" + env["PATH"]
        return env

    def payload(self, case: str) -> str:
        command = "printf 'once\\n' >> " + shlex.quote(str(self.marker))
        if case == "empty-candidate":
            return "true"
        if case == "preset-forbid":
            return command + "; printf blocked >> " + shlex.quote(str(self.protected))
        if case == "sudo-original":
            return "sudo -n /bin/bash -c " + shlex.quote(command)
        if case == "unknown-script":
            target = self.root / "outside-script-target"
            target.write_text("native-control")
            script = self.work / "opaque.py"
            script.write_text(
                "from pathlib import Path\ntry:\n    Path("
                + repr(str(target))
                + ").write_text('escaped')\nexcept OSError:\n    print('UNKNOWN_WRITE_DENIED')\nprint('UNKNOWN_SCRIPT_COMPLETE')\n"
            )
            return "/usr/bin/python3 " + shlex.quote(str(script))
        return command

    def headless(self, env: dict, home: Path) -> tuple[int, str]:
        args = [
            str(self.binaries / "cosh-core"),
            "--headless",
            "--workspace",
            str(self.work),
            "--approval-mode",
            "trust",
            "--tools",
            "shell",
            "Call the shell tool once.",
        ]
        with (home / "stderr.log").open("w") as log:
            process = subprocess.Popen(
                args,
                cwd=self.work,
                env=env,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=log,
                text=True,
                start_new_session=True,
            )
            try:
                output, _ = process.communicate(timeout=75)
            finally:
                stop(process)
        assert all(
            json.loads(line).get("type") != "control_request"
            for line in output.splitlines()
        ), "use the real TTY for approval cases"
        return process.returncode, output

    def tty(self, case: str, env: dict, home: Path) -> tuple[int, str]:
        env.update(
            COSH_CORE_PATH=str(self.binaries / "cosh-core"),
            COSH_SHELL_LANG="en-US",
            COSH_SHELL_ISOLATED="1",
            COSH_SHELL_RAW_SHELL="bash",
            COSH_SHELL_DEFAULT_SHELL="bash",
            COSH_SHELL_UPGRADE_CHECK="0",
            COSH_SHELL_BOOTSTRAP_PATH="0",
            COSH_SHELL_HEALTH_SCAN="disabled",
            COSH_RECOMMENDATIONS_ENABLED="0",
        )
        pid, master = pty.fork()
        if pid == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 48, 320, 0, 0))
            os.chdir(self.work)
            binary = str(self.binaries / "cosh-shell")
            os.execve(  # noqa: S606 - real PTY child
                binary, [binary, "raw", "cosh-core", "--shell", "bash"], env
            )
        raw = bytearray()

        def drain(seconds: float) -> None:
            deadline = time.monotonic() + seconds
            while (
                time.monotonic() < deadline
                and select.select(
                    [master], [], [], max(0, deadline - time.monotonic())
                )[0]
            ):
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                raw.extend(chunk)
                assert len(raw) < 4 * 1024 * 1024

        approved = False
        try:
            drain(2)
            os.write(master, b"/mode approval trust confirm\r")
            drain(0.6)
            os.write(master, b"/agent\r")
            drain(0.4)
            os.write(master, b"Call the shell tool once.\r")
            deadline = time.monotonic() + 75
            while time.monotonic() < deadline:
                drain(0.2)
                screen = ANSI.sub("", raw.decode(errors="replace"))
                if not approved and "[ Allow once ]" in screen:
                    decisions = [
                        item
                        for item in records(self.trace)
                        if item["method"] == "authorization.evaluate"
                    ]
                    assert decisions[-1]["response"]["verdict"] == "STEP_UP"
                    decision_id = decisions[-1]["response"]["decision_id"]
                    assert not any(
                        item.get("decision_id") == decision_id
                        and item.get("phase") == "execute"
                        for item in records(self.trace)
                    )
                    assert not any(
                        item.get("event_type") == "authorization_receipt"
                        and item["details"].get("decision_id") == decision_id
                        for item in records(self.audit)
                    )
                    assert (
                        self.marker.read_text() == ""
                    ), "executed before native approval"
                    proposals = [
                        item
                        for item in records(self.trace)
                        if item["method"] == "intent.propose_command"
                    ]
                    caveats = proposals[-1]["response"].get("caveats", [])
                    save(
                        home / "approval-observation.json",
                        {
                            "visible_before_execute": True,
                            "approved": case != "tty-deny",
                            "caveats": [
                                {
                                    "text": text,
                                    "present_in_pty_transcript": text in screen,
                                    "prefix_present_in_pty_transcript": text[:80]
                                    in screen,
                                }
                                for text in caveats
                            ],
                            "screen": screen[-14000:],
                        },
                    )
                    approved = True
                    os.write(master, b"\x1b" if case == "tty-deny" else b"\r")
                if "DYNAMIC_HOST_COMPLETE" in screen:
                    break
            assert approved and "DYNAMIC_HOST_COMPLETE" in screen, screen[-4000:]
            return 0, screen
        finally:
            (home / "tty.raw").write_bytes(raw)
            try:
                os.killpg(pid, signal.SIGHUP)
            except ProcessLookupError:
                pass
            for _ in range(100):
                if os.waitpid(pid, os.WNOHANG)[0]:
                    break
                time.sleep(0.05)
            else:
                os.killpg(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
            os.close(master)

    def run(self, case: str) -> dict:
        previous = sorted(self.root.glob(case + "-attempt-*"))
        assert sum((path / "result.json").exists() for path in previous) < (
            3 if case == "allowed" else 2
        ), "bounded to two Host observations per case; preserve setup failures separately"
        home = self.root / (case + "-attempt-" + str(len(previous) + 1))
        home.mkdir()
        self.marker.write_text("")
        self.protected.write_text("unchanged")
        self.publish(
            "task",
            self.task_text(
                "deny"
                if case == "task-deny"
                else "ask" if case in TTY_CASES else "allow"
            ),
        )
        command = self.payload(case)
        before, audit_before = len(records(self.trace)), len(records(self.audit))
        start = time.monotonic()
        with model(command) as (port, results, provider):
            env = self.prepare(case, home, port)
            code, transcript = (
                self.tty(case, env, home)
                if case in TTY_CASES
                else self.headless(env, home)
            )
            calls = provider.calls
        protocol = records(self.trace)[before:]
        receipts = [
            item["details"]
            for item in records(self.audit)[audit_before:]
            if item.get("event_type") == "authorization_receipt"
        ]
        with sqlite3.connect(self.root / "audit/security-events.db") as database:
            stored = [
                json.loads(row[0])
                for row in database.execute(
                    "SELECT details FROM security_events WHERE event_type='authorization_receipt'"
                )
            ]
        for receipt in receipts:
            assert [
                item for item in stored if item["decision_id"] == receipt["decision_id"]
            ] == [receipt]
        proposals = [
            item for item in protocol if item["method"] == "intent.propose_command"
        ]
        decisions = [
            item for item in protocol if item["method"] == "authorization.evaluate"
        ]
        caveats = proposals[0]["response"].get("caveats", []) if proposals else []
        row = {
            "case": case,
            "status": "OBSERVED",
            "proof_surface": ("real_tty" if case in TTY_CASES else "structured_host"),
            "command": command,
            "cwd": str(self.work),
            "host_exit": code,
            "provider_calls": calls,
            "duration_seconds": round(time.monotonic() - start, 3),
            "marker": self.marker.read_text(),
            "pap_sources": json.loads(json.dumps(self.policies)),
            "runtime_preset": self.runtime.read_text() if case != "fixed" else None,
            "candidate": proposals[0] if proposals else None,
            "authorization": decisions[0] if decisions else None,
            "claim_phases": [
                item for item in protocol if item["method"] == "authorization.claim"
            ],
            "receipts": receipts,
            "jsonl_sqlite_receipts_equal": True,
            "tool_results": results,
            "caveats": [
                {"text": text, "present_in_host_transcript": text in transcript}
                for text in caveats
            ],
            "temporary_cedar_files": [
                str(path) for path in (home / "tmp").glob("*.cedar")
            ],
            "fault_injection": (
                case if case in FAULTS or case == "unsupported-host" else None
            ),
        }
        if case in TTY_CASES:
            row["approval"] = pick(
                json.loads((home / "approval-observation.json").read_text()),
                ("visible_before_execute", "approved", "caveats"),
            )
            row["host_exit"] = None
            row["completion"] = (
                "Native UI reached model completion; interactive shell terminated by test controller."
            )
        save(home / "result.json", row)
        assert code == 0 and calls == 2 and not row["temporary_cedar_files"], row
        blocked = case in FAULTS or case == "unsupported-host"
        if blocked:
            assert (
                not proposals and not decisions and not receipts and row["marker"] == ""
            ), row
            assert row["duration_seconds"] < 60, row
            expected = {
                "intent-timeout": "timed out",
                "intent-malformed": "invalid command intent response",
                "intent-clarification": "command needs clarification",
                "unsupported-host": "does not enforce nonempty hook output",
            }[case]
            assert expected in json.dumps(results), row
        else:
            assert (
                len(proposals) == int(case != "fixed")
                and len(decisions) == 1
                and len(receipts) == 1
            ), row
            if proposals:
                assert proposals[0]["request_keys"] == [
                    "command",
                    "cwd",
                    "format_version",
                ], row
                assert proposals[0]["request"] == {
                    "format_version": "asc.intent.command.request.v1",
                    "command": command,
                    "cwd": str(self.work),
                }, row
                assert proposals[0]["response"]["outcome"] == "candidate", row
            launch = decisions[0]["request"]["launch"]
            assert launch["args"] == ["-c", command] and launch["cwd"] == str(
                self.work
            ), row
            assert launch["program"] == "/bin/bash", row
            check_receipt(row)
            assert any(
                item.get("phase") == "abandon" and item["ok"]
                for item in row["claim_phases"]
            ), row
            if case == "empty-candidate":
                assert proposals[0]["response"]["policy"] == "", row
                assert not any(
                    rule["effect"] == "allow" and "write" in rule["rights"]
                    for rule in decisions[0]["request"]["requested_permissions"][
                        "filesystem"
                    ]["rules"]
                ), row
            if case == "task-deny":
                decision = decisions[0]["response"]
                requested = decisions[0]["request"]["requested_permissions"]
                assert allows_write(requested, self.marker), row
                assert decision["verdict"] == "DENY" or not allows_write(
                    decision["effective_permissions"], self.marker
                ), row
                earlier = [
                    json.loads(path.read_text())
                    for path in self.root.glob("allowed-attempt-*/result.json")
                ]
                allowed = next(item for item in earlier if item["status"] == "PASS")
                assert (
                    allowed["command"] == command and allowed["cwd"] == row["cwd"]
                ), row
                assert (
                    allowed["authorization"]["request"]["requested_permissions"]
                    == requested
                ), row
                assert (
                    allowed["authorization"]["response"]["policy_version"]
                    != decision["policy_version"]
                ), row
            if case == "preset-forbid":
                requested = decisions[0]["request"]["requested_permissions"]
                assert not allows_write(requested, self.protected), row
                assert requested["network"]["mode"] == "off", row
            if case in {"task-deny", "tty-deny", "empty-candidate", "unknown-script"}:
                assert row["marker"] == "", row
            elif case == "sudo-original":
                assert row["marker"] in {"", "once\n"}, row
            else:
                assert row["marker"] == "once\n", row
            if case == "unknown-script":
                assert (
                    self.root / "outside-script-target"
                ).read_text() == "native-control", row
                assert "UNKNOWN_WRITE_DENIED" in json.dumps(results), row
                assert caveats, row
                assert all(
                    item["prefix_present_in_pty_transcript"]
                    for item in row["approval"]["caveats"]
                ), row
                row["outside_script_target"] = "native-control"
        assert self.protected.read_text() == "unchanged", row
        row["status"] = "PASS"
        if case in TTY_CASES and any(
            not item["present_in_pty_transcript"] for item in row["approval"]["caveats"]
        ):
            row["display_limit"] = (
                "The native approval transcript contains the caveat prefix but clips its full text."
            )
        save(home / "result.json", row)
        print("PASS", case, flush=True)
        return row


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--case", choices=CASES, action="append")
    parser.add_argument("--fixed-boundaries", action="store_true")
    args = parser.parse_args()
    assert os.geteuid() == 0
    assert args.root.is_absolute() and args.root.is_dir() and not args.root.is_symlink()
    assert (args.root / "daemon.sock").is_socket()
    assert os.environ.get("ASC_SANDBOX_TEST_PRIVATE_ETC") == "1"
    isolation = json.loads((args.root / "isolation.json").read_text())
    assert os.readlink("/proc/self/ns/mnt") != isolation["parent_mount_namespace"]
    hook = Path(__file__).resolve().parents[3] / "cosh-extension/hooks/sandbox-guard.py"
    harness = Harness(args.root, args.bin_dir.resolve(strict=True), hook)
    rows = []
    with trace_protocol(harness.endpoint, harness.tap, harness.trace):
        for case in args.case or CASES:
            rows.append(harness.run(case))
        if args.fixed_boundaries:
            harness.publish("task", harness.task_text("allow"))
            environment = {
                "PATH": str(harness.binaries) + ":/usr/bin:/bin",
                "AGENT_SEC_DAEMON_SOCKET": str(harness.tap),
                "ASC_SANDBOX_TEST_PRIVATE_ETC": "1",
            }
            os.environ["ASC_COSH_CORE"] = str(harness.binaries / "cosh-core")
            run_all_shell_host(
                "default-boundaries",
                args.root,
                harness.work,
                hook,
                environment,
                harness.audit,
            )
    save(
        args.root / "acceptance.json",
        {
            "format_version": 1,
            "rows": rows,
            "policy_contract": "Current eligible explicit-principal PAP sources; task restrictions use forbids, not the removed task_policy selector.",
        },
    )


if __name__ == "__main__":
    main()
