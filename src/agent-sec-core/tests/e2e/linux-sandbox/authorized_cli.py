#!/usr/bin/env python3
"""Exercise the upper PEP against a real root daemon, Cedar and the strict backend."""

import argparse
import json
import os
import select
import shlex
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from all_shell_host import run_all_shell_host
from cosh_host import run_host


def write_json(path: Path, value: dict) -> None:
    """Write root-owned startup configuration or caller permissions."""
    pending = path.with_suffix(".new")
    pending.write_text(json.dumps(value), encoding="utf-8")
    pending.chmod(0o600)
    os.replace(pending, path)


def rpc(endpoint: Path, method: str, params: dict | None = None) -> dict:
    """Probe the existing framed UDS protocol without using a mock server."""
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(5)
        connection.connect(str(endpoint))
        connection.sendall(
            json.dumps({"method": method, "params": params or {}}).encode() + b"\n"
        )
        with connection.makefile("rb") as reader:
            return json.loads(reader.readline(1024 * 1024))


def policy_source(requirement: str, excluded: Path | None = None) -> dict:
    """Use the same OS contract and Cedar schema as the running implementation."""
    type_condition = 'when { principal has agent_type && ["codex", "cosh"].contains(principal.agent_type) };'
    command = (
        'permit(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource) '
        + type_condition
    )
    warning = ""
    if requirement in {"warn", "ask"}:
        warning = (
            r'@advice("{\"verdict\":\"warn\",\"message\":\"network disabled\"}")'
            + "\n"
            + command
        )
    if requirement == "ask":
        command = '@advice("{\\"verdict\\":\\"escalate\\"}")\n' + command
    text = (
        'permit(principal is SecCore::Subject, action == SecCore::Action::"file.read", resource) '
        'when { resource.path == "/" || resource.path like "/*" };\n'
        'permit(principal is SecCore::Subject, action == SecCore::Action::"file.write", resource) '
        'when { resource.path == "/" || resource.path like "/*" };\n'
        'permit(principal is SecCore::Subject, action == SecCore::Action::"file.exec", resource) '
        + type_condition
        + "\n"
        + command
        + "\n"
        + warning
    )
    if excluded is not None:
        path = json.dumps(str(excluded))
        descendants = json.dumps(str(excluded) + "/*")
        text += (
            '\nforbid(principal is SecCore::Subject, action == SecCore::Action::"file.read", resource) '
            f"when {{ resource.path == {path} || resource.path like {descendants} }};"
        )
    if requirement == "modify":
        advice = {
            "verdict": "transform",
            "transform": {
                "path": "$target.args",
                "value": [
                    "-c",
                    "printf TARGET_STARTED >&2; printf transformed > modified",
                ],
            },
        }
        text += "\n@advice(" + json.dumps(json.dumps(advice)) + ")\n" + command
    if requirement == "deny":
        text += '\nforbid(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource);'
    return {
        "language": "cedar",
        "content": {
            "schema": {"schemaId": "seccore.system", "revision": 1},
            "text": text,
        },
    }


def receipt(data: Path, outcome: dict, principal: dict) -> dict:
    """An acknowledged decision must exist exactly once in each real sink."""
    decision_id = outcome["decision_id"]
    assert outcome["receipt_error"] is None, outcome
    events = [
        json.loads(line)
        for line in (data / "security-events.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    events = [
        event
        for event in events
        if event["event_type"] == "authorization_receipt"
        and event["details"].get("decision_id") == decision_id
    ]
    assert len(events) == 1, events
    with sqlite3.connect(data / "security-events.db") as connection:
        rows = connection.execute(
            "SELECT details FROM security_events WHERE event_type = ?",
            ("authorization_receipt",),
        ).fetchall()
    rows = [json.loads(row[0]) for row in rows]
    rows = [row for row in rows if row.get("decision_id") == decision_id]
    assert len(rows) == 1, rows
    assert events[0]["details"] == rows[0]
    assert events[0]["uid"] == 0 and events[0]["pid"] > 0
    assert rows[0]["verdict"] == outcome["verdict"]
    assert rows[0]["policy_version"] == outcome["policy_version"]
    assert rows[0]["principal"] == principal
    assert rows[0]["result"] == outcome["result"]
    assert rows[0]["warnings"] == outcome["warnings"]
    return rows[0]


def exercise(daemon: Path, example: Path, helper: Path, root: Path) -> None:
    """Keep all policy source ancestors root-owned and non-writable by other users."""
    work, data = root / "work", root / "audit"
    work.mkdir(mode=0o700)
    data.mkdir(mode=0o700)
    endpoint, config = root / "daemon.sock", root / "authorization.json"
    permissions = {
        "version": 1,
        "filesystem": {
            "default": "deny",
            "rules": [
                {"effect": "allow", "path": "/", "rights": ["read"]},
                {"effect": "allow", "path": str(work), "rights": ["read", "write"]},
                {"effect": "allow", "path": "/tmp", "rights": ["read", "write"]},
            ],
        },
        "execution": {"deny_paths": []},
        "network": {"mode": "off"},
    }
    permission_file, skillsec = root / "permissions.json", root / "skillsec.json"
    write_json(permission_file, permissions)
    write_json(
        skillsec,
        {
            "stateDir": str(root / "state"),
            "managedSkillDirs": [str(work)],
            "enableDefaultSkillDirs": False,
        },
    )
    environment = os.environ | {
        "AGENT_SEC_DATA_DIR": str(data),
        "AGENT_SEC_DAEMON_SOCKET": str(endpoint),
        "AGENT_SEC_OLLAMA_MODEL": "sandbox-e2e-unavailable",
        "AGENT_SEC_MODEL_SERVICE_BASE_URL": "http://127.0.0.1:0",
        "PATH": "/usr/bin:/bin",
        "RUST_LOG": "info",
    }
    example_principal = {"agent_type": "codex"}
    command = [
        str(example),
        "--helper",
        str(helper),
        "--socket",
        str(endpoint),
        "--permissions",
        str(permission_file),
        "--agent-type",
        "codex",
    ]
    daemon_command = [
        str(daemon),
        "--socket",
        str(endpoint),
        "--skillsec-config",
        str(skillsec),
    ]

    def stop(process: subprocess.Popen) -> None:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

    with (root / "daemon.log").open("w+", encoding="utf-8") as log:
        process = None

        def start(arguments: list[str]) -> subprocess.Popen:
            log.seek(0, os.SEEK_END)
            child = subprocess.Popen(arguments, env=environment, stdout=log, stderr=log)
            try:
                end = time.monotonic() + 20
                while True:
                    assert child.poll() is None, "daemon exited during startup"
                    try:
                        if "result" in rpc(endpoint, "daemon.health"):
                            return child
                    except (OSError, ValueError):
                        pass
                    assert time.monotonic() < end, "daemon startup timed out"
                    time.sleep(0.05)
            except BaseException:
                stop(child)
                raise

        try:
            process = start(daemon_command)
            created = rpc(
                endpoint,
                "policy.templates.create",
                {"policyName": "sandbox-baseline", "template": policy_source("allow")},
            )
            assert "result" in created, created
            current = created["result"]
            policy_id = current["policyId"]
            stop(process)
            write_json(
                config,
                {
                    "environment_permissions": permissions,
                    "ttl_seconds": 2,
                },
            )
            writable = root / "writable"
            writable.mkdir()
            writable.chmod(0o777)
            selected = writable / "selected.json"
            selected.symlink_to(config)
            linked_parent = root / "alias"
            linked_parent.symlink_to(config.parent, target_is_directory=True)
            for unsafe in (selected, linked_parent / config.name):
                rejected = subprocess.run(
                    daemon_command + ["--authorization-config", str(unsafe)],
                    env=environment,
                    capture_output=True,
                    text=True,
                    timeout=15,
                    check=False,
                )
                assert (
                    rejected.returncode == 1
                    and "authorization config" in rejected.stderr
                ), rejected
            process = start(daemon_command + ["--authorization-config", str(config)])
            type_request = {
                "type": "os",
                "request_id": "type-match",
                "execution_id": "type-match",
                "principal": {},
                "launch": {
                    "program": "true",
                    "resolved_program": "/usr/bin/true",
                    "args": [],
                    "cwd": str(work),
                },
                "requested_permissions": permissions,
            }
            for principal, expected in [
                ({"agent_type": "codex"}, "ALLOW"),
                ({"agent_type": "qoder"}, "DENY"),
                ({}, "DENY"),
            ]:
                type_request["principal"] = principal
                evaluated = rpc(endpoint, "authorization.evaluate", type_request)
                assert evaluated.get("result", {}).get("verdict") == expected, evaluated
                reported = rpc(
                    endpoint,
                    "authorization.report",
                    {
                        "decision_id": evaluated["result"]["decision_id"],
                        "request_id": type_request["request_id"],
                        "execution_id": type_request["execution_id"],
                        "approval": None,
                        "recorded_at": int(time.time()),
                        "result": {
                            "installed": False,
                            "started": False,
                            "exit_code": None,
                            "signal": None,
                            "cancel_signal": None,
                            "error": "authorization-only probe",
                        },
                    },
                )
                assert "result" in reported, reported
            persisted = rpc(
                endpoint,
                "policy.templates.get",
                {"id": policy_id, "revision": current["revision"]},
            )
            assert persisted.get("result") == current, persisted

            def publish(requirement: str, excluded: Path | None = None) -> dict:
                nonlocal current
                source = policy_source(requirement, excluded)
                updated = rpc(
                    endpoint,
                    "policy.templates.update",
                    {
                        "policyId": policy_id,
                        "policyName": "sandbox-baseline",
                        "template": source,
                    },
                )
                assert "result" in updated, updated
                assert updated["result"]["template"] == source, updated
                if source == current["template"]:
                    assert updated["result"]["revision"] == current["revision"], updated
                else:
                    assert updated["result"]["revision"] > current["revision"], updated
                current = updated["result"]
                return current

            def invoke(name: str, expected: str, status: int) -> dict:
                completed = subprocess.run(
                    command
                    + [
                        "--",
                        "/bin/sh",
                        "-c",
                        f"printf TARGET_STARTED >&2; printf complete > {name}",
                    ],
                    cwd=work,
                    env=environment,
                    capture_output=True,
                    text=True,
                    timeout=20,
                    check=False,
                )
                assert completed.returncode == status, (
                    completed.stdout,
                    completed.stderr,
                )
                outcome = json.loads(completed.stdout)
                assert outcome["verdict"] == expected, outcome
                if outcome["warnings"]:
                    assert (
                        completed.stderr.count("POLICY_WARNING network disabled") == 1
                    )
                    assert completed.stderr.index(
                        "POLICY_WARNING"
                    ) < completed.stderr.index("TARGET_STARTED")
                    assert "APPROVAL_REQUIRED" not in completed.stderr
                receipt(data, outcome, example_principal)
                return outcome

            allowed = invoke("allowed", "ALLOW", 0)
            assert allowed["result"]["installed"] and allowed["result"]["started"]
            assert (work / "allowed").read_text() == "complete"
            print(
                "PASS persisted PAP Cedar policy -> strict sandbox -> checked JSONL/SQLite receipt",
                flush=True,
            )

            private = work / "private"
            private.mkdir()
            (private / "key").write_text("secret\n")
            (work / "public").write_text("public\n")
            publish("allow", private)
            check = 'read line < public && [ "$line" = public ] && ! (read hidden < private/key)'
            child_check = subprocess.run(
                command
                + [
                    "--",
                    "/bin/sh",
                    "-c",
                    check + ' && /bin/sh -c "$1"',
                    "parent",
                    check,
                ],
                cwd=work,
                env=environment,
                capture_output=True,
                text=True,
                timeout=20,
                check=False,
            )
            assert child_check.returncode == 0, (child_check.stdout, child_check.stderr)
            narrowed = json.loads(child_check.stdout)
            assert (
                narrowed["verdict"] == "MODIFY" and narrowed["result"]["exit_code"] == 0
            ), narrowed
            receipt(data, narrowed, example_principal)

            alias = root / "view"
            alias.symlink_to(work, target_is_directory=True)
            probe = work / "probe"
            shutil.copy2("/bin/true", probe)
            selective = json.loads(json.dumps(permissions))
            selective["filesystem"]["rules"] = [
                {"effect": "allow", "path": path, "rights": ["read"]}
                for path in ["/bin", "/lib", "/lib64", str(alias)]
                if Path(path).exists()
            ]
            selective["execution"]["deny_paths"] = [str(probe)]
            write_json(permission_file, selective)
            script = (
                'read line < "$1/public" && [ "$line" = public ] '
                '&& ! (read hidden < "$1/private/key") || exit 90; '
                '"$1/probe"; [ "$?" = 126 ]'
            )
            result = subprocess.run(
                command + ["--", "/bin/bash", "-c", script, "test", str(alias)],
                cwd=work,
                env=environment,
                capture_output=True,
                text=True,
                timeout=20,
            )
            assert result.returncode == 0, (result.stdout, result.stderr)
            receipt(data, json.loads(result.stdout), example_principal)
            write_json(permission_file, permissions)
            print(
                "PASS selective dynamic Bash mounts: aliases share read denial and noexec",
                flush=True,
            )
            publish("warn")
            warned = invoke("warned", "ALLOW", 0)
            assert warned["warnings"] == ["network disabled"], warned
            assert (
                warned["result"]["started"]
                and (work / "warned").read_text() == "complete"
            )
            print(
                "PASS warn shown before execution without approval and retained in both receipts",
                flush=True,
            )

            before = (
                (data / "security-events.jsonl")
                .read_text()
                .count('"authorization_receipt"')
            )
            publish("modify")
            modified = invoke("unmodified", "MODIFY", 0)
            assert (work / "modified").read_text() == "transformed"
            assert not (work / "unmodified").exists()
            after = (
                (data / "security-events.jsonl")
                .read_text()
                .count('"authorization_receipt"')
            )
            assert (
                after == before + 1
            ), "complete MODIFY unexpectedly required a DEFER round trip"
            assert modified["policy_version"] != warned["policy_version"]
            print(
                "PASS PAP transform -> one decision -> modified command executes",
                flush=True,
            )

            publish("deny")
            denied = invoke("denied", "DENY", 1)
            assert not denied["result"]["started"] and not (work / "denied").exists()
            print(
                "PASS PAP update immediately denies new authorization without file synchronization",
                flush=True,
            )

            publish("ask")
            pending = subprocess.Popen(
                command
                + ["--ask", "--", "/bin/sh", "-c", "printf complete > approved"],
                cwd=work,
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                assert select.select([pending.stderr], [], [], 15)[
                    0
                ], "approval was not requested"
                warning = pending.stderr.readline()
                assert "POLICY_WARNING network disabled" in warning, warning
                prompt = pending.stderr.readline()
                assert "APPROVAL_REQUIRED" in prompt, (warning, prompt)
                assert not (
                    work / "approved"
                ).exists(), "target started before approval"
                # A second PDP evaluation would see this deny. The bound grant must continue locally.
                publish("deny")
                time.sleep(3)
                assert not (
                    work / "approved"
                ).exists(), "target started during approval wait"
                stdout, stderr = pending.communicate("yes\n", timeout=20)
                assert pending.returncode == 0, (stdout, prompt, stderr)
                approved = json.loads(stdout)
                assert approved["warnings"] == ["network disabled"], approved
                assert "POLICY_WARNING" not in stderr, stderr
                assert (
                    approved["verdict"] == "STEP_UP"
                    and approved["policy_version"] != denied["policy_version"]
                )
                assert (
                    approved["result"]["started"]
                    and (work / "approved").read_text() == "complete"
                )
                assert receipt(data, approved, example_principal)["approval"] is True
                fresh = invoke("after-approval", "DENY", 1)
                assert fresh["policy_version"] != approved["policy_version"]
                print(
                    "PASS STEP_UP -> approval beyond claim deadline -> same grant executes without reevaluation",
                    flush=True,
                )
            finally:
                if pending.poll() is None:
                    pending.kill()
                    pending.communicate(timeout=5)
            publish("allow")
            allowed_again = invoke("before-delete", "ALLOW", 0)
            assert allowed_again["result"]["started"]
            cli = example.parent.parent / "agent-sec-cli"
            hook = (
                Path(__file__).resolve().parents[3]
                / "cosh-extension/hooks/sandbox-guard.py"
            )
            # Legacy sandbox re-entry must not traverse a runner-owned private home
            # after root loses host DAC capabilities in the user namespace.
            legacy_bin = root / "legacy-bin"
            legacy_bin.mkdir(mode=0o700)
            shutil.copy2(helper, legacy_bin / "linux-sandbox")
            hook_env = environment | {
                "PATH": f"{legacy_bin}:{cli.parent}:/usr/bin:/bin",
                "AGENT_SEC_COSH_SANDBOX_ALL_SHELL": "1",
            }
            hook_env.pop("AGENT_SEC_COSH_SANDBOX_PERMISSIONS", None)

            def fire_hook(
                event: str, tool_command: str, env: dict | None = None
            ) -> dict:
                completed = subprocess.run(
                    [sys.executable, str(hook)],
                    input=json.dumps(
                        {
                            "session_id": "cosh-e2e",
                            "hook_event_name": event,
                            "tool_name": "shell",
                            "cwd": str(work),
                            "tool_input": {"command": tool_command},
                        }
                    ),
                    # Direct protocol only; real Cosh cases supply their own signal.
                    env=(env or hook_env) | {"COSH_HOOK_EMPTY_OUTPUT_POLICY": "block"},
                    cwd=work,
                    capture_output=True,
                    text=True,
                    timeout=60,
                    check=False,
                )
                assert completed.returncode == 0, completed.stderr
                return json.loads(completed.stdout)

            def prepare_hook(name: str, expected: str) -> str:
                command = f"rm -rf cache; printf complete > {name}"
                result = fire_hook("PreToolUse", command)
                assert result["decision"] == expected, result
                if expected == "ask":
                    displayed_command = shlex.join(
                        [str(Path("/bin/bash").resolve()), "-c", command]
                    )
                    assert result["reason"] == (
                        f"允许本次执行吗？\n\n    {displayed_command}\n\n"
                        "原因：策略要求人工确认。"
                    ), result
                    assert result["systemMessage"] == result["reason"], result
                if expected == "block":
                    assert "hookSpecificOutput" not in result
                    return ""
                assert not (work / name).exists(), "PreToolUse executed the command"
                return result["hookSpecificOutput"]["tool_input"]["command"]

            def execute_hook(wrapped: str, expected: int = 0) -> None:
                completed = subprocess.run(
                    shlex.split(wrapped),
                    cwd=work,
                    env=hook_env,
                    capture_output=True,
                    text=True,
                    timeout=30,
                    check=False,
                )
                assert completed.returncode == expected, (
                    completed.stdout,
                    completed.stderr,
                )
                assert fire_hook("PostToolUse", wrapped) == {}

            def check_hook_receipt(
                wrapped: str, started: bool, approval: bool | None
            ) -> None:
                decision_id = shlex.split(wrapped)[-1]
                matches = [
                    json.loads(line)
                    for line in (data / "security-events.jsonl")
                    .read_text()
                    .splitlines()
                    if json.loads(line).get("details", {}).get("decision_id")
                    == decision_id
                ]
                assert len(matches) == 1, matches
                details = matches[0]["details"]
                assert details["principal"] == {"agent_type": "cosh"}, details
                assert details["result"]["started"] is started, details
                assert details["approval"] is approval, details
                receipt(
                    data,
                    {
                        "decision_id": decision_id,
                        "receipt_error": None,
                        "policy_version": details["policy_version"],
                        "warnings": details["warnings"],
                        "verdict": details["verdict"],
                        "result": details["result"],
                    },
                    {"agent_type": "cosh"},
                )

            publish("warn")
            output = fire_hook(
                "PreToolUse", "rm -rf cache; printf complete > hook-allowed"
            )
            assert (
                output["decision"] == "allow"
                and "network disabled" in output["systemMessage"]
            ), output
            wrapped = output["hookSpecificOutput"]["tool_input"]["command"]
            assert not (work / "hook-allowed").exists()
            execute_hook(wrapped)
            assert (work / "hook-allowed").read_text() == "complete"
            check_hook_receipt(wrapped, True, None)
            execute_hook(wrapped, 1)
            check_hook_receipt(wrapped, True, None)

            publish("ask")
            declined = prepare_hook("hook-declined", "ask")
            assert fire_hook("PostToolUse", declined) == {}
            assert not (work / "hook-declined").exists()
            check_hook_receipt(declined, False, None)
            execute_hook(declined, 1)

            approved = prepare_hook("hook-approved", "ask")
            time.sleep(3)
            publish("deny")
            execute_hook(approved)
            assert (work / "hook-approved").read_text() == "complete"
            check_hook_receipt(approved, True, True)
            prepare_hook("hook-denied", "block")
            assert not (work / "hook-denied").exists()

            publish("modify")
            (work / "modified").unlink()
            modified_hook = prepare_hook("hook-unmodified", "allow")
            execute_hook(modified_hook)
            assert not (work / "hook-unmodified").exists()
            assert (work / "modified").read_text() == "transformed"
            check_hook_receipt(modified_hook, True, None)

            publish("allow")
            protected = root / "protected"
            protected.write_text("unchanged")
            output = fire_hook("PreToolUse", f"rm -f {shlex.quote(str(protected))}")
            assert output["decision"] == "allow", output
            isolated = output["hookSpecificOutput"]["tool_input"]["command"]
            execute_hook(isolated, 1)
            assert protected.read_text() == "unchanged"
            check_hook_receipt(isolated, True, None)
            unavailable_hook = fire_hook(
                "PreToolUse",
                "rm -rf cache",
                hook_env | {"AGENT_SEC_DAEMON_SOCKET": str(root / "missing.sock")},
            )
            assert unavailable_hook["decision"] == "block", unavailable_hook
            print(
                "PASS packaged Cosh hook -> PAP/PDP -> native ask handoff -> strict execution and receipts; deny, warn, MODIFY, delayed approval, decline, replay, isolation and unavailable daemon",
                flush=True,
            )
            for case in (
                "blocked",
                "allowed",
                "approved",
                "declined",
                "timeout",
            ):
                publish("ask" if case in {"approved", "declined"} else "allow")
                run_host(
                    case,
                    root,
                    work,
                    hook,
                    hook_env,
                    data / "security-events.jsonl",
                    cli,
                )

            for case in (
                "disabled",
                "disabled-dangerous",
                "disabled-blocked",
                "enabled-blocked",
                "allowed",
                "legacy-env-ignored",
                "default-boundaries",
                "git-worktree",
                "symlink-git",
                "denied",
                "denied-write",
                "approved",
                "declined",
                "sudo-preserved",
                "sudo-system",
                "invalid-flag",
                "missing-template",
                "invalid-template",
                "writable-template",
                "writable-template-parent",
                "symlink-template",
                "missing-cli",
                "missing-daemon",
                "invalid-response",
                "authorization-timeout",
                "hook-timeout",
                "hook-crash",
                "hook-empty",
                "hook-empty-disabled",
                "hook-invalid-output",
                "finish-failure",
            ):
                publish(
                    "ask"
                    if case in {"approved", "declined", "sudo-preserved"}
                    else "deny" if case == "denied" else "allow"
                )
                run_all_shell_host(
                    case,
                    root,
                    work,
                    hook,
                    hook_env,
                    data / "security-events.jsonl",
                )

            publish("allow")
            deleted = rpc(
                endpoint,
                "policy.templates.delete",
                {"id": policy_id, "revision": current["revision"]},
            )
            assert "result" in deleted, deleted
            denied = subprocess.run(
                command + ["--", "/bin/sh", "-c", "printf stale > after-delete"],
                cwd=work,
                env=environment,
                capture_output=True,
                text=True,
                timeout=20,
                check=False,
            )
            assert denied.returncode != 0, (denied.stdout, denied.stderr)
            assert json.loads(denied.stdout)["verdict"] == "DENY", denied
            assert not (
                work / "after-delete"
            ).exists(), "deleted policy fell back to a cached grant"
            print(
                "PASS PAP deletion disables authorization without cached-policy fallback",
                flush=True,
            )
        except BaseException:
            log.flush()
            log.seek(0)
            print(log.read(), flush=True)
            raise
        finally:
            if process is not None:
                stop(process)


def main() -> None:
    """Run with explicit built binaries; missing prerequisites are test failures."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("daemon", type=Path)
    parser.add_argument("example", type=Path)
    parser.add_argument("helper", type=Path)
    parser.add_argument("--isolation-root", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    assert (
        os.geteuid() == 0
    ), "run in Linux CI with sudo; daemon credentials must be UID 0"
    binaries = [
        path.resolve(strict=True) for path in (args.daemon, args.example, args.helper)
    ]
    if args.isolation_root is not None:
        prepare_private_template(args.isolation_root)
        run = args.isolation_root / "run"
        run.mkdir(mode=0o700)
        exercise(*binaries, run)
        return
    with tempfile.TemporaryDirectory(prefix="asc-authorized-", dir="/run") as directory:
        root = Path(directory)
        (root / "parent-mountns").write_text(os.readlink("/proc/self/ns/mnt"))
        completed = subprocess.run(
            [
                "unshare",
                "--mount",
                "--fork",
                sys.executable,
                str(Path(__file__).resolve()),
                *(str(path) for path in binaries),
                "--isolation-root",
                str(root),
            ],
            check=False,
        )
        raise SystemExit(completed.returncode)


def prepare_private_template(root: Path) -> None:
    """Install test defaults and a private passwd home without changing host /etc."""
    assert os.readlink("/proc/self/ns/mnt") != (root / "parent-mountns").read_text()
    subprocess.run(["mount", "--make-rprivate", "/"], check=True)
    upper, work = root / "etc-upper", root / "etc-work"
    upper.mkdir(mode=0o700)
    work.mkdir(mode=0o700)
    subprocess.run(
        [
            "mount",
            "-t",
            "overlay",
            "overlay",
            "-o",
            f"lowerdir=/etc,upperdir={upper},workdir={work}",
            "/etc",
        ],
        check=True,
    )
    template = Path("/etc/agent-sec/sandbox/permissions.json.template")
    for path in (template.parent.parent, template.parent):
        assert not path.is_symlink(), path
        path.mkdir(mode=0o755, exist_ok=True)
        path.chmod(0o755)
        os.chown(path, 0, 0)
    template.unlink(missing_ok=True)
    source = (
        Path(__file__).resolve().parents[3]
        / "packaging/sandbox/permissions.json.template"
    )
    template.write_bytes(source.read_bytes())
    template.chmod(0o644)
    os.chown(template, 0, 0)
    # The HOME rejection case must not use the real account's home as a workspace.
    home = root / "passwd-home"
    home.mkdir(mode=0o700)
    passwd = Path("/etc/passwd")
    assert not passwd.is_symlink(), passwd
    rows = []
    for row in passwd.read_text().splitlines():
        fields = row.split(":")
        if len(fields) == 7 and fields[2] == str(os.geteuid()):
            fields[5] = str(home)
        rows.append(":".join(fields))
    passwd.write_text("\n".join(rows) + "\n")
    os.environ["ASC_SANDBOX_TEST_PRIVATE_ETC"] = "1"


if __name__ == "__main__":
    main()
