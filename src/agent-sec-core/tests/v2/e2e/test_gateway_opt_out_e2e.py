"""DPROC-GW-005: default-disabled Gateway and explicit startup recovery."""

import json
import signal
import subprocess
import time
from collections.abc import Callable
from pathlib import Path

import pytest

from tests.v2.e2e.conftest import (
    _POLL_INTERVAL,
    _SOCKET_WAIT_SECONDS,
    DaemonHandle,
    _terminate,
)


def test_gateway_opt_out_keeps_file_policy_and_refuses_credential_work(
    start_daemon: Callable[..., DaemonHandle], tmp_path: Path
) -> None:
    daemon = start_daemon()
    unavailable = daemon.cli("gateway", "status")
    assert unavailable.returncode == 1
    assert json.loads(unavailable.stderr)["error"]["code"] == "unavailable"

    template = tmp_path / "file.cedar"
    template.write_text(
        'forbid(principal, action == SecCore::Action::"file.write", resource)\n'
        'when { context.input.operation == SecCore::WriteOperation::"delete"\n'
        '  && resource.path == "/workspace/important" };\n'
    )
    policy = daemon.request(
        "policy", "create", "--name", "without-gateway", "--file", str(template)
    )
    scope = daemon.request(
        "scope",
        "create",
        "--executable",
        str(tmp_path / "never-executed"),
        "--policy-id",
        policy["policyId"],
        "--policy-revision",
        str(policy["revision"]),
    )
    assert daemon.request("scope", "get", "--scope-id", scope["scopeId"]) == scope
    assert scope["policySnapshots"] == [policy]
    assert daemon.request("scope", "delete", "--scope-id", scope["scopeId"])[
        "completed"
    ]
    daemon.request(
        "policy",
        "delete",
        "--policy-id",
        policy["policyId"],
        "--revision",
        str(policy["revision"]),
    )

    credential_template = (
        Path(__file__).resolve().parents[3]
        / "v2/crates/asc-gateway-plan/policies/github.cedar"
    )
    unavailable = daemon.cli(
        "policy",
        "create",
        "--name",
        "credential-without-gateway",
        "--schema-revision",
        "2",
        "--file",
        str(credential_template),
    )
    assert unavailable.returncode == 1
    assert json.loads(unavailable.stderr)["error"]["code"] == "unavailable"
    assert daemon.request("policy", "list")["total"] == 0

    daemon.process.send_signal(signal.SIGTERM)
    daemon.process.communicate(timeout=5)
    assert daemon.process.returncode == 0
    assert not daemon.socket_path.exists()


@pytest.mark.parametrize("artifact", ["directory", "dangling_symlink"])
def test_gateway_opt_out_refuses_existing_state_before_rpc(
    daemon_bin: str,
    daemon_settings: Callable[[Path], tuple[Path, dict[str, str]]],
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    artifact: str,
) -> None:
    data = tmp_path / "data"
    data.mkdir(mode=0o700)
    state = data / "gateway"
    preserved = state / "preserved"
    missing = tmp_path / "missing"
    if artifact == "directory":
        state.mkdir()
        preserved.write_text("keep existing state")
    else:
        state.symlink_to(missing)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(data))
    endpoint = tmp_path / "refused.sock"
    config, environment = daemon_settings(endpoint)
    process = subprocess.Popen(
        [
            daemon_bin,
            "serve",
            "--socket",
            str(endpoint),
            "--skillsec-config",
            str(config),
        ],
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + _SOCKET_WAIT_SECONDS
        while process.poll() is None:
            assert not endpoint.exists(), "refused daemon exposed its RPC socket"
            assert time.monotonic() < deadline, "refused daemon did not exit"
            time.sleep(_POLL_INTERVAL)
        _, stderr = process.communicate(timeout=5)
        assert process.returncode == 1, stderr
        assert "--enable-gateway" in stderr and str(state) in stderr
        assert not endpoint.exists()
        if artifact == "directory":
            assert preserved.read_text() == "keep existing state"
        else:
            assert state.is_symlink() and state.readlink() == missing
            assert not missing.exists()
    finally:
        _terminate(process)


def test_explicit_gateway_startup_restores_management_after_default_refusal(
    start_daemon: Callable[..., DaemonHandle],
    daemon_bin: str,
    daemon_settings: Callable[[Path], tuple[Path, dict[str, str]]],
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    data = tmp_path / "data"
    data.mkdir(mode=0o700)
    state = data / "gateway"
    state.mkdir(mode=0o700)
    projection = state / "state.json"
    projection.write_text(
        json.dumps({"version": 3, "enabled": False, "generation": 0, "scopes": {}})
    )
    projection.chmod(0o600)
    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(data))
    endpoint = tmp_path / "daemon.sock"
    config, environment = daemon_settings(endpoint)
    refused = subprocess.run(
        [
            daemon_bin,
            "serve",
            "--socket",
            str(endpoint),
            "--skillsec-config",
            str(config),
        ],
        env=environment,
        capture_output=True,
        text=True,
        timeout=_SOCKET_WAIT_SECONDS,
        check=False,
    )
    assert refused.returncode == 1, refused.stderr
    assert "--enable-gateway" in refused.stderr
    assert not endpoint.exists()
    daemon = start_daemon(disable_gateway=False)
    assert daemon.request("gateway", "status") == {
        "enabled": False,
        "ready": False,
        "scopes": [],
    }
