"""Real CLI/daemon assignment lifecycle over UDS, without a matching target.

See test_policy_delivery_e2e.py for real discovery and HTTP delivery to a mock.
"""

import json
from pathlib import Path


def test_help_and_version_do_not_require_a_daemon(cli):
    # --help / --version resolve entirely in the parser; no socket, no daemon.
    for flag in ("--help", "--version"):
        result = cli(flag)
        assert result.returncode == 0, result.stderr
        assert result.stdout != ""


def _write_template(tmp_path: Path, name: str, files: list[str]) -> str:
    """Writes a reusable file-deletion policy and returns its path."""
    path = tmp_path / name
    path.write_text(
        json.dumps(
            {
                "specVersion": "0.1",
                "rules": [
                    {
                        "effect": "block",
                        "category": "file",
                        "action": "write",
                        "target": {"type": "file", "path": path},
                        "where": {"operation": {"eq": "delete"}},
                    }
                    for path in files
                ],
            }
        )
    )
    return str(path)


def test_policy_assignment_snapshot_and_cleanup(daemon, tmp_path):
    template_v1 = _write_template(
        tmp_path, "policy-v1.json", ["/workspace/important/**"]
    )
    template_v2 = _write_template(tmp_path, "policy-v2.json", ["/srv/data"])
    policy = daemon.request(
        "policy",
        "create",
        "--name",
        "protect-important-files",
        "--language",
        "json",
        "--file",
        template_v1,
    )
    policy_id = policy["policyId"]
    assert policy["revision"] == 1
    scope = daemon.request(
        "scope",
        "create",
        "--executable",
        str(tmp_path / "never-executed"),
        "--policy-id",
        policy_id,
        "--policy-revision",
        "1",
    )
    scope_id = scope["scopeId"]
    assert "revision" not in scope
    assert scope["policySnapshots"] == [policy]
    assert scope["status"] == "ACTIVE"
    updated = daemon.request(
        "policy",
        "update",
        "--policy-id",
        policy_id,
        "--name",
        "protect-v2",
        "--language",
        "json",
        "--file",
        template_v2,
    )
    assert updated["revision"] == 2
    assert (
        daemon.request("policy", "get", "--policy-id", policy_id, "--revision", "2")
        == updated
    )
    stale = daemon.cli(
        "scope",
        "create",
        "--pid",
        "4242",
        "--policy-id",
        policy_id,
        "--policy-revision",
        "1",
    )
    assert stale.returncode == 1
    assert json.loads(stale.stderr)["error"]["code"] == "not_found"
    assert daemon.request("scope", "get", "--scope-id", scope_id) == scope
    assert daemon.request("scope", "retry", "--scope-id", scope_id) == scope
    for resource, total in (("policy", 1), ("scope", 1), ("binding", 0)):
        listing = daemon.request(resource, "list", "--limit", "10", "--offset", "0")
        assert set(listing) == {"items", "total"}
        assert listing["total"] == total
        assert len(listing["items"]) == total
    missing = daemon.cli("binding", "get", "--binding-id", "missing")
    assert missing.returncode == 1
    assert json.loads(missing.stderr)["error"]["code"] == "not_found"
    daemon.request("policy", "delete", "--policy-id", policy_id, "--revision", "2")
    assert daemon.request("scope", "get", "--scope-id", scope_id) == scope
    for _ in range(2):
        assert daemon.request("scope", "delete", "--scope-id", scope_id) == {
            "scopeId": scope_id,
            "completed": True,
        }
    assert daemon.request("scope", "list")["total"] == 0
    for resource, operation in (
        ("scope", "update"),
        ("binding", "create"),
        ("binding", "update"),
        ("binding", "delete"),
    ):
        assert daemon.cli(resource, operation).returncode == 2


def test_raw_cedar_files_round_trip_through_the_daemon(cli, daemon, tmp_path):
    # `policy default` output is the canonical raw Cedar file a user edits and imports.
    exported = cli("policy", "default")
    assert exported.returncode == 0, exported.stderr
    baseline = tmp_path / "baseline.cedar"
    baseline.write_text(exported.stdout)
    schema = {"schemaId": "seccore.system", "revision": 1}
    policy = daemon.request(
        "policy", "create", "--name", "baseline", "--file", str(baseline)
    )
    assert policy["template"] == {
        "language": "cedar",
        "content": {"schema": schema, "text": exported.stdout},
    }

    protect = tmp_path / "protect.cedar"
    protect.write_text(
        'forbid(principal, action == SecCore::Action::"file.write", resource)\n'
        'when { context.input.operation == SecCore::WriteOperation::"delete"\n'
        '  && resource.path == "/workspace/important" };\n'
    )
    updated = daemon.request(
        "policy",
        "update",
        "--policy-id",
        policy["policyId"],
        "--name",
        "protect",
        "--language",
        "cedar",
        "--schema-id",
        "seccore.system",
        "--schema-revision",
        "1",
        "--file",
        str(protect),
    )
    assert updated["revision"] == 2
    assert updated["template"]["content"] == {
        "schema": schema,
        "text": protect.read_text(),
    }

    envelope = tmp_path / "protect.json"
    envelope.write_text(json.dumps(updated["template"]))
    assert (
        daemon.request(
            "policy",
            "update",
            "--policy-id",
            policy["policyId"],
            "--name",
            "protect",
            "--language",
            "json",
            "--file",
            str(envelope),
        )
        == updated
    )

    native = _write_template(tmp_path, "native.json", ["/workspace/important"])
    for source in (native, str(envelope)):
        for operation in ("create", "update"):
            args = ["policy", operation, "--name", "legacy", "--file", source]
            if operation == "update":
                args.extend(["--policy-id", policy["policyId"]])
            rejected = daemon.cli(*args)
            assert rejected.returncode == 1, rejected.stderr
            assert rejected.stdout == ""
            assert json.loads(rejected.stderr)["error"]["code"] == "invalid_argument"
            assert daemon.request("policy", "list")["items"] == [updated]

    # An unknown revision is rejected by the daemon, never resolved to another one.
    unknown = daemon.cli(
        "policy",
        "create",
        "--name",
        "future",
        "--schema-revision",
        "99",
        "--file",
        str(protect),
    )
    assert unknown.returncode == 1
    assert json.loads(unknown.stderr)["error"]["code"] == "invalid_argument"
    assert daemon.request("policy", "list")["items"] == [updated]
