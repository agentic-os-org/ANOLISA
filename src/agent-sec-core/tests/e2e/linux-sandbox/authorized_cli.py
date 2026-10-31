#!/usr/bin/env python3
"""Exercise the upper PEP against a real root daemon, Cedar and the strict backend."""

import argparse
import json
import os
import select
import socket
import sqlite3
import subprocess
import tempfile
import time
from pathlib import Path


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


def policy_source(requirement: str) -> dict:
    """Use the same OS contract and Cedar schema as the running implementation."""
    command = 'permit(principal, action == SecCore::Action::"process.spawn", resource) '
    command += 'when { principal has agent_type && principal.agent_type == "codex" };'
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
        'permit(principal, action == SecCore::Action::"file.read", resource) '
        'when { resource.path == "/" || resource.path like "/*" };\n'
        'permit(principal, action == SecCore::Action::"file.write", resource) '
        'when { resource.path == "/" || resource.path like "/*" };\n'
        'permit(principal, action == SecCore::Action::"file.exec", resource);\n'
        + command
        + "\n"
        + warning
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
        text += (
            '\nforbid(principal, action == SecCore::Action::"process.spawn", resource);'
        )
    return {
        "language": "cedar",
        "content": {
            "schema": {"schemaId": "seccore.system", "revision": 2},
            "text": text,
        },
    }


def receipt(data: Path, outcome: dict) -> dict:
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
    assert rows[0]["principal"] == {
        "agent_id": "acceptance-agent",
        "agent_type": "codex",
    }
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
    command = [
        str(example),
        "--helper",
        str(helper),
        "--socket",
        str(endpoint),
        "--permissions",
        str(permission_file),
        "--agent-id",
        "acceptance-agent",
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
                    "baseline_policy": policy_id,
                    "environment_permissions": permissions,
                    "ttl_seconds": 2,
                },
            )
            process = start(daemon_command + ["--authorization-config", str(config)])
            persisted = rpc(
                endpoint,
                "policy.templates.get",
                {"id": policy_id, "revision": current["revision"]},
            )
            assert persisted.get("result") == current, persisted

            def publish(requirement: str) -> dict:
                nonlocal current
                updated = rpc(
                    endpoint,
                    "policy.templates.update",
                    {
                        "policyId": policy_id,
                        "policyName": "sandbox-baseline",
                        "template": policy_source(requirement),
                    },
                )
                assert "result" in updated, updated
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
                receipt(data, outcome)
                return outcome

            allowed = invoke("allowed", "ALLOW", 0)
            assert allowed["result"]["installed"] and allowed["result"]["started"]
            assert (work / "allowed").read_text() == "complete"
            print(
                "PASS persisted PAP Cedar policy -> strict sandbox -> checked JSONL/SQLite receipt",
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
                assert receipt(data, approved)["approval"] is True
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
            deleted = rpc(
                endpoint,
                "policy.templates.delete",
                {"id": policy_id, "revision": current["revision"]},
            )
            assert "result" in deleted, deleted
            unavailable = subprocess.run(
                command + ["--", "/bin/sh", "-c", "printf stale > after-delete"],
                cwd=work,
                env=environment,
                capture_output=True,
                text=True,
                timeout=20,
                check=False,
            )
            assert unavailable.returncode != 0, (unavailable.stdout, unavailable.stderr)
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
    args = parser.parse_args()
    assert (
        os.geteuid() == 0
    ), "run in Linux CI with sudo; daemon credentials must be UID 0"
    binaries = [
        path.resolve(strict=True) for path in (args.daemon, args.example, args.helper)
    ]
    with tempfile.TemporaryDirectory(prefix="asc-authorized-", dir="/run") as directory:
        exercise(*binaries, Path(directory))


if __name__ == "__main__":
    main()
