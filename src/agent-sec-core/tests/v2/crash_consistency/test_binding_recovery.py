"""Deletion must survive an older Apply and repeated crashes during cleanup."""

import json
import os

import pytest
from tests.v2.crash_consistency.conftest import until


def deleting_with_unknown(recovery, scope):
    result = recovery.daemon.request("scope", "delete", "--scope-id", scope["scopeId"])
    assert result["completed"] is False
    scopes, bindings = recovery.saved()
    assert scopes[0][1] == "DELETING"
    assert len(bindings) == 1 and bindings[0][1] == "PENDING_DELETE"
    assert json.loads(bindings[0][2])[0]["presence"] == "UNKNOWN"


def test_delete_then_crash_cleans_apply_committed_before_its_reply(recovery):
    gate = recovery.remote.pause("after_apply")
    recovery.restart()
    scope = recovery.scope(recovery.policy(), recovery.spawn())
    target_id = gate.wait()
    deleting_with_unknown(recovery, scope)
    recovery.crash()
    recovery.restart()
    # Apply committed remotely, but its reply was lost when the daemon crashed.
    gate.release.set()
    recovery.clean()
    assert recovery.remote.posts() == [target_id], "restart resumed superseded Apply"


def test_delete_during_apply_then_crash_cleans_committed_target(recovery):
    before = recovery.remote.pause("before_apply")
    committed = recovery.remote.pause("after_apply")
    recovery.restart()
    scope = recovery.scope(recovery.policy(), recovery.spawn())
    target_id = before.wait()
    deleting_with_unknown(recovery, scope)
    # Commit the older Apply after deletion intent, while its reply remains blocked.
    before.release.set()
    assert committed.wait() == target_id
    assert recovery.remote.present() == {target_id}
    assert not any(
        method.startswith("DELETE") for method, _ in recovery.remote.requests
    )
    recovery.crash()
    recovery.restart()
    committed.release.set()
    recovery.clean()
    assert recovery.remote.posts() == [target_id], "restart resumed superseded Apply"
    assert ("DELETE_204", target_id) in recovery.remote.requests
    assert ("DELETE_404", target_id) not in recovery.remote.requests


@pytest.mark.skipif(
    os.environ.get("ASC_TEST_DEFERRED_PROTOCOL") != "1",
    reason="deferred AgentSecCore/AgentSight late-Apply protocol; set ASC_TEST_DEFERRED_PROTOCOL=1",
)
def test_late_apply_cannot_outlive_confirmed_scope_cleanup(recovery):
    # A received POST may still be queued before AgentSight's coordinator lock.
    gate = recovery.remote.pause("before_apply")
    recovery.restart()
    scope = recovery.scope(recovery.policy(), recovery.spawn())
    target_id = gate.wait()
    deleting_with_unknown(recovery, scope)
    recovery.crash()
    recovery.restart()
    recovery.clean()
    assert ("DELETE_404", target_id) in recovery.remote.requests
    gate.release.set()
    until(
        lambda: list(recovery.remote.requests),
        lambda trace: ("POST_FINISHED", target_id) in trace,
    )
    # Keep the safety assertion even if current code clears responsibility too early.
    assert not recovery.remote.present(), (
        "late Apply resurrected a target after local deletion; "
        f"saved={recovery.saved()}, requests={recovery.remote.requests}"
    )
    assert recovery.saved() == ([], [])


def test_partial_scope_cleanup_survives_two_crashes_without_reapplying(recovery):
    recovery.restart()
    recovery.spawn()
    recovery.spawn()
    scope = recovery.scope(recovery.policy())
    recovery.ready(2)
    assert len(recovery.remote.present()) == 2
    # Arm both gates before deletion; either target's response can finish first.
    # Hold duplicate DELETE replies too, including those sent after a restart.
    first = recovery.remote.pause("after_delete", repeat=True)
    second = recovery.remote.pause("after_delete", repeat=True)
    recovery.daemon.request("scope", "delete", "--scope-id", scope["scopeId"])
    first_id = first.wait()
    assert second.wait() != first_id
    assert recovery.saved()[0][0][1] == "DELETING"
    recovery.crash()
    recovery.restart()
    first.release.set()
    until(lambda: recovery.saved()[1], lambda bindings: len(bindings) == 1)
    assert (
        recovery.saved()[0][0][1] == "DELETING"
    ), "Scope vanished before its last Binding"
    recovery.crash()
    recovery.restart()
    second.release.set()
    recovery.clean()
    assert len(recovery.remote.posts()) == 2, "cleanup recovery issued another Apply"
