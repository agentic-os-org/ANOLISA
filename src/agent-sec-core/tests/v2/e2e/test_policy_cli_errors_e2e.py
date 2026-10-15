"""Failure-path coverage: transport errors, usage errors, and authorization.

The CLI exit-code contract is exercised end to end:

* a daemon error envelope goes to stderr with exit code 1;
* a transport failure (no daemon listening) exits 1 with a diagnostic on
  stderr and nothing on stdout;
* a usage error (invalid arguments) exits 2 without touching a daemon;
* an unauthorized caller receives the daemon's ``permission_denied`` envelope.
"""

import json
import tempfile
from pathlib import Path

from tests.v2.e2e.test_policy_cli_e2e import _write_template


def test_transport_failure_when_daemon_absent(cli, tmp_path):
    missing = tmp_path / "absent.sock"
    result = cli("--socket", str(missing), "policy", "list")
    # Connect failure is a local/transport error, not a daemon envelope.
    assert result.returncode == 1
    assert result.stdout == ""
    assert result.stderr != ""


def test_usage_errors_exit_two_without_a_daemon(cli):
    usage_cases = [
        # --socket must be absolute.
        ("--socket", "relative.sock", "policy", "list"),
        # Repeated --socket at different levels is ambiguous.
        ("--socket", "/run/a.sock", "policy", "list", "--socket", "/run/b.sock"),
        # --timeout-ms below the accepted range.
        ("--socket", "/run/a.sock", "--timeout-ms", "0", "policy", "list"),
    ]
    for case in usage_cases:
        result = cli(*case)
        assert (
            result.returncode == 2
        ), f"{case} -> rc={result.returncode}: {result.stderr}"


def test_unauthorized_caller_gets_permission_denied(unauthorized_daemon):
    result = unauthorized_daemon.cli("policy", "list", "--limit", "10", "--offset", "0")
    assert result.returncode == 1
    assert result.stdout == ""
    envelope = json.loads(result.stderr)
    assert envelope["error"]["code"] == "permission_denied"
    assert envelope["error"]["message"] != ""
    assert envelope["requestId"]


def test_explicit_nonroot_administrator_can_manage_assignments_and_is_revoked_on_restart(
    start_daemon, monkeypatch
):
    # The socket and authored input must be reachable without exposing the root-owned database.
    with tempfile.TemporaryDirectory(prefix="asc-v2-admin-", dir="/tmp") as directory:
        runtime = Path(directory)
        runtime.chmod(0o755)
        monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(runtime / "data"))
        template = Path(_write_template(runtime, "policy.json", ["/protected/**"]))
        template.chmod(0o644)
        socket_path = str(runtime / "daemon.sock")
        daemon = start_daemon(admin_uids=[1001], name=socket_path)
        daemon.caller_uid = 1001
        policy = daemon.request(
            "policy", "create", "--name", "ordinary-admin", "--file", str(template)
        )
        scope = daemon.request(
            "scope",
            "create",
            "--pid",
            str(2**32 - 1),
            "--policy-id",
            policy["policyId"],
            "--policy-revision",
            "1",
        )
        updated = daemon.request(
            "policy",
            "update",
            "--policy-id",
            policy["policyId"],
            "--name",
            "updated",
            "--file",
            str(template),
        )
        assert updated["revision"] == 2
        assert daemon.request("scope", "get", "--scope-id", scope["scopeId"]) == scope
        assert daemon.request("scope", "retry", "--scope-id", scope["scopeId"]) == scope
        assert daemon.request("policy", "list")["items"] == [updated]
        assert daemon.request("binding", "list")["total"] == 0
        for _ in range(2):
            assert daemon.request("scope", "delete", "--scope-id", scope["scopeId"])[
                "completed"
            ]
        assert (
            daemon.request(
                "policy", "get", "--policy-id", policy["policyId"], "--revision", "2"
            )
            == updated
        )
        assert (
            daemon.request(
                "policy", "delete", "--policy-id", policy["policyId"], "--revision", "2"
            )
            == updated
        )
        persisted = daemon.request(
            "policy", "create", "--name", "persisted", "--file", str(template)
        )
        daemon.process.terminate()
        assert daemon.process.wait(timeout=5) == 0
        restarted = start_daemon(admin_uids=[], name=socket_path)
        assert (
            restarted.request(
                "policy", "get", "--policy-id", persisted["policyId"], "--revision", "1"
            )
            == persisted
        )
        restarted.caller_uid = 1001
        for command in [
            ("policy", "list"),
            (
                "policy",
                "delete",
                "--policy-id",
                persisted["policyId"],
                "--revision",
                "1",
            ),
        ]:
            result = restarted.cli(*command)
            assert result.returncode == 1
            assert result.stdout == ""
            assert json.loads(result.stderr)["error"]["code"] == "permission_denied"
        restarted.process.terminate()
        assert restarted.process.wait(timeout=5) == 0
