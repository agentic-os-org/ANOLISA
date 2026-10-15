"""Storage admission and retry recovery through real daemon processes."""

import json
import sqlite3
import subprocess
from contextlib import closing

import pytest
from tests.v2.crash_consistency.conftest import until


@pytest.mark.parametrize(
    ("fault", "diagnostic"),
    [
        ("schema", "policy storage schema incompatible"),
        ("json", "policy storage corrupt"),
        ("permissions", "unsafe Policy data path"),
        ("busy", "policy storage busy"),
    ],
)
def test_unusable_policy_database_rejects_startup_without_losing_responsibility(
    recovery, daemon_settings, daemon_bin, fault, diagnostic
):
    gate = recovery.remote.pause("after_apply")
    recovery.restart()
    recovery.scope(recovery.policy(), recovery.spawn())
    gate.wait()
    recovery.crash()
    # Compare logical state: a normal WAL checkpoint may change main-file bytes.
    with closing(sqlite3.connect(recovery.database)) as database:
        if fault == "schema":
            database.execute("PRAGMA user_version=999")
        elif fault == "json":
            database.execute("PRAGMA ignore_check_constraints=ON")
            database.execute("UPDATE bindings SET spec_json='broken-json'")
        database.commit()
    if fault == "permissions":
        recovery.database.chmod(0o666)
    inode = recovery.database.stat().st_ino
    with closing(sqlite3.connect(recovery.database)) as database:
        before = list(database.iterdump())
        version = database.execute("PRAGMA user_version").fetchone()
    saved = recovery.saved()
    assert json.loads(saved[1][0][2])[0]["presence"] == "UNKNOWN"
    calls = list(recovery.remote.requests)
    socket_path = recovery.directory / "rejected.sock"
    config, environment = daemon_settings(socket_path)
    try:
        with closing(sqlite3.connect(recovery.database)) as lock:
            if fault == "busy":
                lock.execute("BEGIN IMMEDIATE")
            result = subprocess.run(
                [
                    daemon_bin,
                    "--socket",
                    str(socket_path),
                    "--skillsec-config",
                    str(config),
                ],
                env=environment,
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )
        assert result.returncode != 0
        assert diagnostic in result.stderr, result.stderr
        assert not socket_path.exists(), "unusable Policy storage opened UDS admission"
        assert recovery.database.stat().st_ino == inode, "startup replaced the database"
        with closing(sqlite3.connect(recovery.database)) as database:
            assert (
                list(database.iterdump()) == before
            ), "startup discarded authoritative SQL state"
            assert database.execute("PRAGMA user_version").fetchone() == version
        assert recovery.saved() == saved
        assert (
            recovery.remote.requests == calls
        ), "failed startup sent a remote operation"
    finally:
        recovery.database.chmod(0o600)


def test_shared_sqlite_write_lock_retains_both_results_and_recovers_without_http_replay(
    recovery,
):
    first = recovery.remote.pause("after_apply")
    second = recovery.remote.pause("after_apply")
    recovery.restart()
    policy = recovery.policy()
    scope = recovery.scope(policy)
    recovery.spawn()
    recovery.spawn()
    targets = {first.wait(), second.wait()}
    saved = recovery.saved()
    assert len(saved[1]) == 2
    assert all(row[1] == "APPLYING" for row in saved[1])
    assert all(json.loads(row[2])[0]["presence"] == "UNKNOWN" for row in saved[1])
    with closing(sqlite3.connect(recovery.database)) as lock:
        lock.execute("BEGIN IMMEDIATE")
        first.release.set()
        second.release.set()
        until(
            lambda: list(recovery.remote.requests),
            lambda trace: {key for method, key in trace if method == "POST_FINISHED"}
            == targets,
        )
        rejected = recovery.daemon.cli(
            "policy",
            "create",
            "--name",
            "blocked",
            "--file",
            str(recovery.directory / "policy.json"),
        )
        assert rejected.returncode == 1
        assert json.loads(rejected.stderr)["error"]["code"] == "unavailable"
        query = recovery.daemon.cli("policy", "list")
        if query.returncode == 0:
            assert json.loads(query.stdout)["items"] == [policy]
        else:
            # Strict connection reopening may itself need the blocked schema/write probe.
            assert query.stdout == ""
            assert json.loads(query.stderr)["error"]["code"] == "unavailable"
        assert (
            recovery.saved() == saved
        ), "failed SQL writes changed intent or target responsibility"
        assert set(recovery.remote.posts()) == targets
        assert len(recovery.remote.posts()) == 2
        lock.rollback()
    bindings = recovery.ready(2)
    assert {item["spec"]["bindingId"] for item in bindings} == {
        row[0] for row in saved[1]
    }
    assert (
        len(recovery.remote.posts()) == 2
    ), "storage recovery repeated completed HTTP Apply"
    recovery.daemon.request("scope", "delete", "--scope-id", scope["scopeId"])
    recovery.clean()


def test_pending_retry_budget_resets_in_a_new_daemon_process(recovery):
    recovery.remote.apply_failures = [503, 503]
    recovery.restart()
    scope = recovery.scope(recovery.policy(), recovery.spawn())
    before = until(
        lambda: recovery.daemon.request("binding", "list")["items"],
        lambda items: len(recovery.remote.posts()) == 2
        and len(items) == 1
        and items[0]["status"]["phase"] == "PENDING_APPLY"
        and "error" in items[0]["status"],
    )[0]
    recovery.crash()
    assert len(recovery.remote.posts()) == 2
    # Four more failures need a fresh five-attempt budget to reach the success.
    recovery.remote.apply_failures = [503] * 4
    recovery.restart()
    restored = recovery.ready(1, timeout=25)[0]
    assert restored["spec"] == before["spec"]
    assert len(recovery.remote.posts()) == 7
    recovery.daemon.request("scope", "delete", "--scope-id", scope["scopeId"])
    recovery.clean()


def test_failed_binding_survives_restart_until_explicit_scope_retry(recovery):
    recovery.remote.apply_failures = [403]
    recovery.restart()
    policy = recovery.policy()
    scope = recovery.scope(policy, recovery.spawn())
    failed = until(
        lambda: recovery.daemon.request("binding", "list")["items"],
        lambda items: len(items) == 1 and items[0]["status"]["phase"] == "APPLY_FAILED",
    )[0]
    recovery.crash()
    recovery.restart()
    other = recovery.scope(policy, recovery.spawn())
    # Completing independent work proves that recovery is running while Failed stays terminal.
    items = until(
        lambda: recovery.daemon.request("binding", "list")["items"],
        lambda items: len(items) == 2
        and any(item["status"]["phase"] == "READY" for item in items),
    )
    assert next(item for item in items if item["spec"] == failed["spec"]) == failed
    assert (
        len(recovery.remote.posts()) == 2
    ), "restart automatically retried a terminal failure"
    recovery.daemon.request("scope", "retry", "--scope-id", scope["scopeId"])
    assert next(item for item in recovery.ready(2) if item["spec"] == failed["spec"])[
        "status"
    ] == {"phase": "READY"}
    assert len(recovery.remote.posts()) == 3
    for assignment in [scope, other]:
        recovery.daemon.request("scope", "delete", "--scope-id", assignment["scopeId"])
    recovery.clean()
