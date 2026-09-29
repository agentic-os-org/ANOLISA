"""End-to-end CRUD coverage for all 15 PAP commands over a real UDS daemon.

Each command is issued by the real ``agent-sec-cli`` process against a real
``agent-sec-daemon`` process; nothing is mocked. The flow mirrors the frozen wire
scenario the Rust suite exercises in-process
(``asc-daemon-protocol/tests/fixtures/pap-crud-e2e.json``): create, then update
policy/scope/binding to a second revision, read them back, list them, and
finally delete. The daemon keeps process-local state and reconciles asynchronously.
Mutation responses describe admission; GET/LIST may already show a later phase.
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
    """Writes a prevent_file_deletion policy template and returns its path."""
    path = tmp_path / name
    path.write_text(json.dumps({"kind": "prevent_file_deletion", "files": files}))
    return str(path)


def test_full_pap_crud_across_all_fifteen_commands(daemon, tmp_path):
    template_v1 = _write_template(
        tmp_path, "policy-v1.json", ["/workspace/important/**"]
    )
    template_v2 = _write_template(tmp_path, "policy-v2.json", ["/srv/data"])

    # --- create (policy, scope, binding) ---
    policy = daemon.request(
        "policy", "create", "--name", "protect-important-files", "--file", template_v1
    )
    policy_id = policy["policyId"]
    assert policy["revision"] == 1

    scope = daemon.request("scope", "create", "--pid", "4242")
    scope_id = scope["scopeId"]
    assert scope["revision"] == 1

    binding = daemon.request(
        "binding",
        "create",
        "--policy-id",
        policy_id,
        "--policy-revision",
        "1",
        "--scope-id",
        scope_id,
        "--scope-revision",
        "1",
    )
    binding_id = binding["spec"]["bindingId"]
    assert binding["status"] == {"phase": "PENDING_APPLY"}
    # The three resources are distinct identifiers, never aliased.
    assert len({policy_id, scope_id, binding_id}) == 3

    # --- update to a second revision ---
    updated_policy = daemon.request(
        "policy",
        "update",
        "--policy-id",
        policy_id,
        "--name",
        "protect-v2",
        "--file",
        template_v2,
    )
    assert updated_policy["revision"] == 2
    policy_revision = updated_policy["revision"]

    updated_scope = daemon.request(
        "scope", "update", "--scope-id", scope_id, "--cgroup-id", "99"
    )
    assert updated_scope["revision"] == 2
    scope_revision = updated_scope["revision"]

    updated_binding = daemon.request(
        "binding",
        "update",
        "--binding-id",
        binding_id,
        "--policy-id",
        policy_id,
        "--policy-revision",
        str(policy_revision),
        "--scope-id",
        scope_id,
        "--scope-revision",
        str(scope_revision),
    )
    assert updated_binding["spec"]["bindingId"] == binding_id

    # --- get (reads the current revision) ---
    got_policy = daemon.request(
        "policy", "get", "--policy-id", policy_id, "--revision", str(policy_revision)
    )
    assert got_policy["policyId"] == policy_id
    got_scope = daemon.request(
        "scope", "get", "--scope-id", scope_id, "--revision", str(scope_revision)
    )
    assert got_scope["scopeId"] == scope_id
    got_binding = daemon.request("binding", "get", "--binding-id", binding_id)
    assert got_binding["spec"]["bindingId"] == binding_id

    # --- list (paginated envelope {items, total}) ---
    for resource in ("policy", "scope", "binding"):
        listing = daemon.request(resource, "list", "--limit", "10", "--offset", "0")
        assert set(listing) == {"items", "total"}
        assert listing["total"] == 1
        assert len(listing["items"]) == 1

    # --- delete ---
    deleted_binding = daemon.request("binding", "delete", "--binding-id", binding_id)
    # The admission snapshot confirms the request, not completed target cleanup.
    assert deleted_binding["status"] == {"phase": "PENDING_DELETE"}

    deleted_policy = daemon.request(
        "policy", "delete", "--policy-id", policy_id, "--revision", str(policy_revision)
    )
    assert deleted_policy["policyId"] == policy_id
    deleted_scope = daemon.request(
        "scope", "delete", "--scope-id", scope_id, "--revision", str(scope_revision)
    )
    assert deleted_scope["scopeId"] == scope_id
