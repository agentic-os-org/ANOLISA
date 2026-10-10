"""Accepted assignments recover autonomously, including lost target processes."""

from tests.v2.crash_consistency.conftest import until


def test_scope_recovers_discovery_and_preserves_binding_across_two_crashes(recovery):
    recovery.restart()
    policy = recovery.policy()
    scope = recovery.scope(policy)
    assert recovery.daemon.request("binding", "list")["total"] == 0
    updated = recovery.daemon.request(
        "policy",
        "update",
        "--policy-id",
        policy["policyId"],
        "--name",
        "updated",
        "--file",
        str(recovery.directory / "policy.json"),
    )
    assert updated["revision"] == 2
    recovery.crash()
    recovery.spawn()
    recovery.restart()
    binding = recovery.ready(1)[0]
    assert binding["spec"]["policy"] == policy
    assert (
        recovery.daemon.request("scope", "get", "--scope-id", scope["scopeId"]) == scope
    )
    assert len(recovery.remote.posts()) == 1
    recovery.crash()
    recovery.restart()
    assert recovery.ready(1) == [binding]
    assert len(recovery.remote.posts()) == 1, "READY was reapplied after restart"
    recovery.daemon.request("scope", "delete", "--scope-id", scope["scopeId"])
    recovery.clean()


def test_process_exit_during_crash_cleans_binding_without_repinning_scope(recovery):
    gate = recovery.remote.pause("health")
    recovery.restart()
    target = recovery.spawn()
    scope = recovery.scope(recovery.policy(), target)
    gate.wait()
    scopes, bindings = recovery.saved()
    assert scopes[0][2] is not None, "PID pin was not committed"
    assert len(bindings) == 1 and bindings[0][1] == "APPLYING"
    pin = scopes[0][2]
    recovery.crash()
    target.terminate()
    target.wait(timeout=5)
    recovery.restart()
    gate.release.set()
    until(
        lambda: recovery.daemon.request("binding", "list"),
        lambda value: value["total"] == 0,
    )
    restored = recovery.daemon.request("scope", "get", "--scope-id", scope["scopeId"])
    assert restored == scope and restored["status"] == "ACTIVE"
    assert recovery.saved()[0][0][2] == pin
    assert not recovery.remote.posts(), "an exited instance was applied after restart"
    assert not recovery.remote.present()
    recovery.daemon.request("scope", "delete", "--scope-id", scope["scopeId"])
    recovery.clean()
