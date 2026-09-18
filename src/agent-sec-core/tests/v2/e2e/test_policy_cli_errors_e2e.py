"""Failure-path coverage: transport errors, usage errors, and authorization.

The CLI exit-code contract is exercised end to end:

* a daemon error envelope goes to stderr with exit code 1;
* a transport failure (no daemon listening) exits 1 with a diagnostic on
  stderr and nothing on stdout;
* a usage error (invalid arguments) exits 2 without touching a daemon;
* an unauthorized caller receives the daemon's ``permission_denied`` envelope.
"""

import json


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
