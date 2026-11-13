"""Exercise all-shell authorization and failures through the real Cosh core."""

import json
import os
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
from collections.abc import Iterator
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

TEMPLATE = Path("/etc/agent-sec/sandbox/permissions.json.template")
TEMPLATE_FAILURES = {
    "missing-template",
    "invalid-template",
    "writable-template",
    "writable-template-parent",
    "symlink-template",
}
METADATA_SYMLINKS = {
    "symlink-git": ".git",
}


def install_sandbox_extension(home: Path, source_hook: Path) -> tuple[Path, Path]:
    """Load the real extension schema while omitting unrelated scanner hooks."""
    manifest = json.loads((source_hook.parents[1] / "cosh-extension.json").read_text())
    selected = {"sandbox-guard", "sandbox-receipt", "sandbox-failure-handler"}
    events = {}
    names = set()
    for event, groups in manifest["hooks"].items():
        for group in groups:
            definitions = [item for item in group["hooks"] if item["name"] in selected]
            if definitions:
                events.setdefault(event, []).append(group | {"hooks": definitions})
                names.update(item["name"] for item in definitions)
    assert names == selected
    manifest["hooks"] = events
    extension = home / ".copilot-shell/extensions/agent-sec-core"
    shutil.copytree(source_hook.parent, extension / "hooks")
    path = extension / "cosh-extension.json"
    path.write_text(json.dumps(manifest, indent=2))
    return extension / "hooks" / source_hook.name, path


def configure_sandbox_extension(manifest: Path, commands: dict) -> None:
    """Preserve product hook attributes when adding tracing or a bounded fault."""
    definition = json.loads(manifest.read_text())
    for event, groups in definition["hooks"].items():
        for group in groups:
            for hook in group["hooks"]:
                hook["command"], hook["timeout"] = commands[event]
    manifest.write_text(json.dumps(definition, indent=2))


@contextmanager
def template_failure(case: str) -> Iterator[None]:
    """Fault only the explicitly prepared private /etc view, then restore it."""
    if case not in TEMPLATE_FAILURES:
        yield
        return
    assert (
        os.environ.get("ASC_SANDBOX_TEST_PRIVATE_ETC") == "1"
    ), "template fault cases require a run-owned private /etc mount"
    assert os.readlink("/proc/self/ns/mnt") != os.readlink("/proc/1/ns/mnt")
    original = TEMPLATE.read_bytes()
    mode = TEMPLATE.stat().st_mode & 0o777
    parent_mode = TEMPLATE.parent.stat().st_mode & 0o777
    backup = TEMPLATE.with_name("permissions.test-backup")
    assert not backup.exists()
    try:
        if case == "missing-template":
            TEMPLATE.unlink()
        elif case == "invalid-template":
            TEMPLATE.write_text("{")
        elif case == "writable-template":
            TEMPLATE.chmod(0o666)
        elif case == "writable-template-parent":
            TEMPLATE.parent.chmod(0o777)
        elif case == "symlink-template":
            TEMPLATE.rename(backup)
            TEMPLATE.symlink_to(backup)
        yield
    finally:
        TEMPLATE.unlink(missing_ok=True)
        backup.unlink(missing_ok=True)
        TEMPLATE.write_bytes(original)
        TEMPLATE.chmod(mode)
        TEMPLATE.parent.chmod(parent_mode)


def run_all_shell_host(
    case: str, root: Path, work: Path, hook: Path, env: dict, audit: Path
) -> None:
    """Exercise one real Host turn, restoring any private template fault."""
    with (
        template_failure(case),
        tempfile.TemporaryDirectory(
            prefix="asc-shell-default-", dir="/tmp"
        ) as temporary,
    ):
        _run_all_shell_host(case, root, work, hook, env, audit, Path(temporary))


def _run_all_shell_host(
    case: str,
    root: Path,
    work: Path,
    hook: Path,
    env: dict,
    audit: Path,
    temporary: Path,
) -> None:
    """Use synthetic tool input with real Hook, PDP and sandbox execution."""
    home = root / ("all-shell-" + case)
    config = home / ".copilot-shell"
    config.mkdir(parents=True)
    hook, extension_manifest = install_sandbox_extension(home, hook)
    directory = work / ("all-shell-" + case)
    directory.mkdir()
    marker = directory / "executions"
    protected = home / "protected"
    protected.write_text("unchanged")
    # The same caller can write this file outside the sandbox; DAC is not the oracle.
    protected.write_text("native-control")
    assert protected.read_text() == "native-control"
    protected.write_text("unchanged")
    controls = {"native_protected_write": True}
    (directory / "skill.py").write_text(
        "import os\n"
        "from pathlib import Path\n"
        f"with Path({str(marker)!r}).open('a') as output: output.write('once\\n')\n"
        "print('PYTHON_EFFECTIVE_UID:' + str(os.geteuid()))\n"
        f"protected = Path({str(protected)!r})\n"
        "try:\n"
        "    protected.write_text('escaped')\n"
        "except OSError:\n"
        "    print('PROTECTED_WRITE_DENIED')\n"
        "print('PYTHON_RESULT_PRESERVED')\n"
    )
    if case == "denied-write":
        (directory / "skill.py").write_text(
            "from pathlib import Path\n"
            f"with Path({str(marker)!r}).open('a') as output: output.write('once\\n')\n"
            f"Path({str(protected)!r}).write_text('escaped')\n"
        )
    shell_command = "python3 skill.py"
    if case in METADATA_SYMLINKS:
        metadata_target = directory / "original-metadata"
        metadata_target.mkdir()
        (metadata_target / "protected").write_text("unchanged")
        metadata_link = directory / METADATA_SYMLINKS[case]
        metadata_link.symlink_to(metadata_target)
        # Prove the caller can replace the name while leaving its target intact.
        metadata_link.unlink()
        metadata_link.mkdir()
        (metadata_link / "replacement").write_text("native-control")
        shutil.rmtree(metadata_link)
        metadata_link.symlink_to(metadata_target)
        metadata_inode = metadata_link.lstat().st_ino
        controls["native_metadata_name_replacement"] = True
        with (directory / "skill.py").open("a") as script:
            script.write(
                f"link = Path({str(metadata_link)!r})\n"
                "link.unlink()\nlink.mkdir()\n"
                "(link / 'replacement').write_text('escaped')\n"
            )
    workspace = directory
    if case in {"disabled-blocked", "enabled-blocked"}:
        shell_command = "python3 skill.py # shutdown"
    if case == "disabled-dangerous":
        shell_command = "rm -rf cache; python3 skill.py"
    if case in {"sudo-preserved", "sudo-system"}:
        shell_command = "sudo -n python3 skill.py"
    trace = home / "hook-output.jsonl"
    input_trace = home / "hook-input.jsonl"
    wrapper = home / "trace_hook.py"
    wrapper.write_text(
        "import pathlib, subprocess, sys\n"
        "payload = sys.stdin.buffer.read()\n"
        f"with pathlib.Path({str(input_trace)!r}).open('ab') as log: log.write(payload + b'\\n')\n"
        "result = subprocess.run(sys.argv[2:], input=payload, "
        "capture_output=True, check=False)\n"
        "with pathlib.Path(sys.argv[1]).open('ab') as log: "
        "log.write(result.stdout + b'\\n')\n"
        "sys.stdout.buffer.write(result.stdout)\n"
        "sys.stderr.buffer.write(result.stderr)\n"
        "sys.exit(result.returncode)\n"
    )
    pre = shlex.join(
        [sys.executable, str(wrapper), str(trace), sys.executable, str(hook)]
    )
    post = shlex.join([sys.executable, str(hook)])
    environment = env | {
        "AGENT_SEC_COSH_SANDBOX_ALL_SHELL": "1",
        "HOME": str(home),
        "COSH_STATES_DIR": str(home / "states"),
        "COSH_TELEMETRY_DISABLED_PATH": str(config / "telemetry_disabled"),
    }
    environment.pop("AGENT_SEC_COSH_SANDBOX_PERMISSIONS", None)
    if case in {
        "disabled",
        "disabled-blocked",
        "disabled-dangerous",
        "hook-empty-disabled",
    }:
        environment["AGENT_SEC_COSH_SANDBOX_ALL_SHELL"] = "0"
    if case == "invalid-flag":
        environment["AGENT_SEC_COSH_SANDBOX_ALL_SHELL"] = "perhaps"
    if case == "legacy-env-ignored":
        environment["AGENT_SEC_COSH_SANDBOX_PERMISSIONS"] = "/missing/ignored.json"
    if case == "sudo-preserved":
        binaries = home / "sudo-bin"
        binaries.mkdir()
        sudo = binaries / "sudo"
        sudo.write_text(
            "#!/bin/sh\n"
            "printf 'SUDO_ORIGINAL_ARGUMENTS:%s\\n' \"$*\"\n"
            'test "$1" = -n || exit 9\nshift\nexec "$@"\n'
        )
        sudo.chmod(0o700)
        environment["PATH"] = str(binaries) + ":" + environment["PATH"]
    if case == "missing-daemon":
        environment["AGENT_SEC_DAEMON_SOCKET"] = str(home / "missing.sock")
    if case in {"missing-cli", "invalid-response", "authorization-timeout"}:
        binaries = home / "bin"
        binaries.mkdir()
        # Cosh resolves sh through PATH before starting the Hook or shell tool.
        (binaries / "sh").symlink_to("/bin/sh")
        (binaries / "python3").symlink_to(sys.executable)
        environment["PATH"] = str(binaries)
        if case != "missing-cli":
            fake_cli = binaries / "agent-sec-cli"
            fake_cli.write_text(
                f"#!{sys.executable}\n"
                + (
                    "import time\ntime.sleep(70)\n"
                    if case == "authorization-timeout"
                    else "print('[]')\n"
                )
            )
            fake_cli.chmod(0o700)
        readiness = subprocess.run(
            ["sh", "-c", "exec python3 -c 'print(\"HARNESS_PATH_READY\")'"],
            env=environment,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        assert (
            readiness.returncode == 0
            and readiness.stdout.strip() == "HARNESS_PATH_READY"
        ), readiness
    if case == "finish-failure":
        fault = home / "finish_fault.py"
        fault.write_text(
            "import runpy, sys\n"
            f"sys.path.insert(0, {str(hook.parent)!r})\n"
            "import sandbox_authorization\n"
            "original = sandbox_authorization.subprocess.run\n"
            "def fail_finish(args, **kwargs):\n"
            "    if 'finish' in args: raise OSError('injected receipt failure')\n"
            "    return original(args, **kwargs)\n"
            "sandbox_authorization.subprocess.run = fail_finish\n"
            f"runpy.run_path({str(hook)!r}, run_name='__main__')\n"
        )
        post = shlex.join([sys.executable, str(fault)])
    timeout = 60000
    hook_fault_marker = home / "hook-fault-invoked"
    if case == "hook-timeout":
        pre = f"printf once > {shlex.quote(str(hook_fault_marker))}; exec /bin/sleep 70"
        timeout = 100
    if case == "hook-crash":
        pre = f"printf once > {shlex.quote(str(hook_fault_marker))}; kill -KILL $$"
    if case in {"hook-empty", "hook-empty-disabled", "hook-invalid-output"}:
        fault = home / "output_fault.py"
        fault.write_text(
            "from pathlib import Path\n"
            f"Path({str(hook_fault_marker)!r}).write_text('once')\n"
            + ("print('not-json')\n" if case == "hook-invalid-output" else "")
        )
        pre = shlex.join([sys.executable, str(fault)])

    class Model(BaseHTTPRequestHandler):
        calls = 0
        requests = []

        def log_message(self, *_args: object) -> None:
            pass

        def do_POST(self) -> None:
            Model.requests.append(
                json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            )
            Model.calls += 1
            delta = (
                {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "all-shell-call",
                            "type": "function",
                            "function": {
                                "name": "shell",
                                "arguments": json.dumps({"command": shell_command}),
                            },
                        }
                    ]
                }
                if Model.calls == 1
                else {"content": "Done."}
            )
            chunks = [(delta, None), ({}, "tool_calls" if Model.calls == 1 else "stop")]
            body = (
                "".join(
                    "data: "
                    + json.dumps(
                        {"choices": [{"index": 0, "delta": d, "finish_reason": f}]}
                    )
                    + "\n\n"
                    for d, f in chunks
                )
                + "data: [DONE]\n\n"
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    before = len(audit.read_text().splitlines()) if audit.exists() else 0
    messages, approvals = [], 0
    with HTTPServer(("127.0.0.1", 0), Model) as server:
        threading.Thread(target=server.serve_forever, daemon=True).start()
        if case in {"default-boundaries", "git-worktree"}:
            metadata = {}
            names = []
            writable_metadata = []
            if case == "git-worktree":
                gitdir = directory / "repository/.git/worktrees/work"
                gitdir.mkdir(parents=True)
                (directory / ".git").write_text(
                    "gitdir: repository/.git/worktrees/work\n"
                )
                (gitdir / "commondir").write_text("../..\n")
                for pointer in (directory / ".git", gitdir / "commondir"):
                    metadata[str(pointer)] = pointer.read_text()
                names += ["repository/.git/worktrees/work"]
                # Like Codex's default path selection, follow gitdir, not commondir.
                writable_metadata.append(directory / "repository/.git/ordinary")
            else:
                names += [".git"]
            for name in names:
                path = directory / name / "protected"
                path.parent.mkdir(exist_ok=True)
                path.write_text("unchanged")
                metadata[str(path)] = "unchanged"
            for path in writable_metadata:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("unchanged")
            # Outside cwd, within the requested temporary-directory grants.
            tmpdir = work / ("all-shell-tmpdir-" + case)
            (tmpdir / ".git").mkdir(parents=True)
            (tmpdir / ".git/protected").write_text("unchanged")
            metadata[str(tmpdir / ".git/protected")] = "unchanged"
            environment["TMPDIR"] = str(tmpdir)
            # Confirm the same endpoint is reachable without sandbox enforcement.
            with socket.create_connection(("127.0.0.1", server.server_port), timeout=2):
                pass
            controls["native_loopback_connect"] = True
            with (directory / "skill.py").open("a") as script:
                script.write(
                    "import socket\n"
                    f"Path({str(temporary / 'default-write')!r}).write_text('allowed')\n"
                    "print('TMP_WRITE_ALLOWED')\n"
                    f"Path({str(tmpdir / 'default-write')!r}).write_text('allowed')\n"
                    "print('TMPDIR_WRITE_ALLOWED')\n"
                    f"for value, original in {metadata!r}.items():\n"
                    "    path = Path(value)\n"
                    "    assert path.read_text() == original\n"
                    "    try: path.write_text('escaped')\n"
                    "    except OSError: print('METADATA_WRITE_DENIED:' + path.parent.name)\n"
                    "    else: raise AssertionError('protected metadata changed')\n"
                    f"for value in {[str(path) for path in writable_metadata]!r}:\n"
                    "    path = Path(value)\n"
                    "    path.write_text('allowed')\n"
                    "    print('WORKSPACE_WRITE_ALLOWED:' + path.parent.name)\n"
                    "try:\n"
                    f"    socket.create_connection(('127.0.0.1', {server.server_port}), timeout=2)\n"
                    "except OSError: print('NETWORK_DENIED')\n"
                    "else: raise AssertionError('network escaped')\n"
                )
        settings = (
            '[ai]\nactive_provider="test"\n[ai.providers.test]\ntype="openai"\n'
            f'api_key="test"\nmodel="local"\nbase_url="http://127.0.0.1:{server.server_port}/v1"\n'
            "[hooks]\nenabled=true\n"
        )
        configure_sandbox_extension(
            extension_manifest,
            {
                "PreToolUse": (pre, timeout),
                "PostToolUse": (post, 15000),
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
        (config / "config.toml").write_text(settings)
        (config / "telemetry_disabled").touch()
        with (
            (home / "stderr").open("w") as stderr,
            (home / "messages.jsonl").open("w") as transcript,
            subprocess.Popen(
                [
                    os.environ["ASC_COSH_CORE"],
                    "--headless",
                    "--workspace",
                    str(workspace),
                    "--approval-mode",
                    "trust",
                    "Call the shell tool once.",
                ],
                cwd=workspace,
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=stderr,
                text=True,
                start_new_session=True,
            ) as host,
        ):

            def stop() -> None:
                if host.poll() is None:
                    try:
                        os.killpg(host.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass

            timer = threading.Timer(75, stop)
            timer.start()
            try:
                for line in host.stdout:
                    transcript.write(line)
                    transcript.flush()
                    message = json.loads(line)
                    messages.append(message)
                    if message.get("type") != "control_request":
                        continue
                    assert case in {"approved", "declined", "sudo-preserved"}, (
                        case,
                        messages,
                        (home / "stderr").read_text(),
                    )
                    approvals += 1
                    assert not marker.exists(), "command ran before native approval"
                    response = (
                        {"behavior": "allow"}
                        if case in {"approved", "sudo-preserved"}
                        else {"behavior": "deny", "message": "declined"}
                    )
                    host.stdin.write(
                        json.dumps(
                            {
                                "type": "control_response",
                                "response": {
                                    "subtype": "success",
                                    "request_id": message["request_id"],
                                    "response": response,
                                },
                            }
                        )
                        + "\n"
                    )
                    host.stdin.flush()
                assert host.wait() == 0, (messages, (home / "stderr").read_text())
            finally:
                timer.cancel()
                stop()
                host.wait(timeout=5)
                server.shutdown()
    (home / "messages.json").write_text(json.dumps(messages, indent=2))
    (home / "provider.json").write_text(json.dumps(Model.requests, indent=2))
    assert Model.calls >= 2, Model.requests
    assert approvals == int(
        case in {"approved", "declined", "sudo-preserved"}
    ), messages
    started = case in {
        "allowed",
        "approved",
        "disabled",
        "disabled-dangerous",
        "hook-empty-disabled",
        "finish-failure",
        "denied-write",
        "default-boundaries",
        "git-worktree",
        "legacy-env-ignored",
        "sudo-preserved",
    } or (case == "sudo-system" and marker.exists())
    assert marker.exists() == started, (case, messages)
    if started:
        assert marker.read_text() == "once\n", "command executed more than once"
        if case != "denied-write":
            assert "PYTHON_RESULT_PRESERVED" in json.dumps(
                Model.requests
            ), Model.requests
        assert protected.read_text() == (
            "escaped" if case in {"disabled", "hook-empty-disabled"} else "unchanged"
        )
        if case not in {"disabled", "hook-empty-disabled", "denied-write"}:
            assert "PROTECTED_WRITE_DENIED" in json.dumps(
                Model.requests
            ), Model.requests
    else:
        assert protected.read_text() == "unchanged"
    if case in METADATA_SYMLINKS:
        assert metadata_link.is_symlink(), "protected metadata name was replaced"
        assert metadata_link.lstat().st_ino == metadata_inode
        assert metadata_link.readlink() == metadata_target
        assert (metadata_target / "protected").read_text() == "unchanged"
        assert not (metadata_link / "replacement").exists()
    if case in {"default-boundaries", "git-worktree"}:
        results = json.dumps(Model.requests)
        for token in (
            "TMP_WRITE_ALLOWED",
            "TMPDIR_WRITE_ALLOWED",
            "NETWORK_DENIED",
        ):
            assert token in results, results
        assert (temporary / "default-write").read_text() == "allowed"
        assert (tmpdir / "default-write").read_text() == "allowed"
        for value, original in metadata.items():
            assert Path(value).read_text() == original
        for path in writable_metadata:
            assert path.read_text() == "allowed"
        if case == "git-worktree":
            assert "METADATA_WRITE_DENIED:work" in results, results
        else:
            assert "METADATA_WRITE_DENIED:.git" in results, results
    if case == "sudo-preserved":
        assert "SUDO_ORIGINAL_ARGUMENTS:-n python3 skill.py" in json.dumps(
            Model.requests
        )
        assert "sudo -n python3 skill.py" in json.dumps(messages)
    rows = [
        json.loads(line)
        for line in (audit.read_text().splitlines()[before:] if audit.exists() else [])
    ]
    receipts = [
        row["details"]
        for row in rows
        if row.get("event_type") == "authorization_receipt"
    ]
    (home / "receipts.json").write_text(json.dumps(receipts, indent=2))
    (home / "case.json").write_text(
        json.dumps(
            {
                "case": case,
                "command": shell_command,
                "cwd": str(workspace),
                "all_shell": environment["AGENT_SEC_COSH_SANDBOX_ALL_SHELL"],
                "legacy_permissions_env": environment.get(
                    "AGENT_SEC_COSH_SANDBOX_PERMISSIONS"
                ),
                "template": str(TEMPLATE),
                "controls": controls,
                "caller_uid": os.geteuid(),
                "marker": marker.read_text() if marker.exists() else None,
                "sudo_system_outcome": (
                    ("executed" if started else "rejected")
                    if case == "sudo-system"
                    else None
                ),
            },
            indent=2,
        )
    )
    hook_faults = {
        "hook-timeout",
        "hook-crash",
        "hook-empty",
        "hook-empty-disabled",
        "hook-invalid-output",
    }
    if case in hook_faults:
        assert hook_fault_marker.read_text() == "once", "extension hook did not run"
    else:
        outputs = [
            json.loads(line) for line in trace.read_text().splitlines() if line.strip()
        ]
        assert len(outputs) == 1, outputs
        expected = (
            "ask"
            if case in {"approved", "declined", "sudo-preserved"}
            else "allow" if started or case == "sudo-system" else "block"
        )
        assert outputs[0]["decision"] == expected, outputs
        if case in {"disabled-blocked", "enabled-blocked"}:
            assert "shutdown" in outputs[0]["reason"], outputs
            assert "hookSpecificOutput" not in outputs[0], outputs
        inputs = [json.loads(line) for line in input_trace.read_text().splitlines()]
        assert (
            len(inputs) == 1 and inputs[0]["tool_input"]["command"] == shell_command
        ), inputs
        if (started and case not in {"disabled", "disabled-dangerous"}) or case in {
            "declined",
            "sudo-system",
        }:
            handoff = shlex.split(
                outputs[0]["hookSpecificOutput"]["tool_input"]["command"]
            )
            assert [row["decision_id"] for row in receipts] == [handoff[-1]], receipts
    if case in {
        "allowed",
        "approved",
        "declined",
        "denied",
        "finish-failure",
        "denied-write",
        "default-boundaries",
        "git-worktree",
        "legacy-env-ignored",
        "sudo-preserved",
        "sudo-system",
    }:
        assert len(receipts) == 1, (case, receipts)
        result = receipts[0]["result"]
        assert result["started"] == (started or case == "sudo-system"), receipts
        if started or case == "sudo-system":
            assert result["installed"], receipts
            assert (result["exit_code"] == 0) == (
                started and case != "denied-write"
            ), receipts
    else:
        assert not receipts, (case, receipts)
    if case == "finish-failure":
        assert "injected receipt failure" in json.dumps(messages), messages
    errors = {
        "invalid-flag": "must be 1/true or 0/false",
        "missing-template": "invalid sandbox permission template",
        "invalid-template": "invalid sandbox permission template",
        "writable-template": "root-owned regular file",
        "writable-template-parent": "template directory must be root-owned",
        "symlink-template": "invalid sandbox permission template",
        "missing-cli": "agent-sec-cli is required",
        "missing-daemon": "Sandbox authorization blocked execution",
        "invalid-response": "invalid sandbox authorization response",
        "authorization-timeout": "timed out",
        "hook-timeout": "timed out",
        "hook-crash": "signal",
        "hook-empty": "returned empty output",
        "hook-invalid-output": "returned invalid JSON",
    }
    if case in errors:
        assert errors[case] in json.dumps(messages), messages
    if case in METADATA_SYMLINKS:
        assert "protected path must not contain symlinks" in json.dumps(
            messages
        ), messages
    if case == "sudo-system":
        observed = json.dumps(Model.requests)
        assert (
            "PYTHON_EFFECTIVE_UID:" in observed
            if started
            else "sudo:" in observed.lower()
        ), Model.requests
    print(f"PASS real Cosh core all-shell {case}", flush=True)
