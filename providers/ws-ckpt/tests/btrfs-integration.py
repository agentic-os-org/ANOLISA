"""Validate Qoder hooks and Scope submissions against real Btrfs snapshots."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


def run(
    *command: str | Path,
    input_text: str | None = None,
    env: dict[str, str] | None = None,
    cwd: Path | None = None,
) -> str:
    result = subprocess.run(
        command, input=input_text, env=env, cwd=cwd, capture_output=True, text=True, timeout=25
    )
    if result.returncode:
        raise RuntimeError(
            f"{command!r} exited {result.returncode}\n{result.stdout}\n{result.stderr}"
        )
    return result.stdout


def main(root: Path) -> None:
    aw = root / "bin/aw"
    ws_ckpt = root / "bin/ws-ckpt"
    traced_cli = root / "bin/traced-ws-ckpt"
    call_log = root / "launch/checkpoint-calls"
    workspace = root / "workspace"
    data = workspace / "data.txt"
    config = root / "launch/aw.yaml"
    state = root / "launch/aw-state"
    prefix = root / "home/prefix"
    provider = Path(os.environ["WS_CKPT_AW_PROVIDER"])
    bundle = root / "provider"
    skill = root / "skill/auto-snapshot-aw.py"

    def submit(phase: str, *expected_statuses: str, cwd: Path | None = None) -> None:
        # The Hook receives the AW binding even when the Skill control variables are absent.
        environment = {key: value for key, value in os.environ.items()
                       if not key.startswith("WS_CKPT_AW_")}
        run(aw, "run", "--config", config, "--agent", "qoder", "--", "--request-set-id", phase,
            env=environment, cwd=cwd)
        records = [
            json.loads(line)["record"]
            for path in (state / "journal").glob("*.jsonl")
            for line in path.read_text().splitlines()
        ]
        calls = [
            row
            for row in records
            if row.get("kind") == "provider" and row["call"]["step_id"] == "ws-ckpt-checkpoint"
        ]
        assert sorted(row["status"] for row in calls) == sorted(expected_statuses), calls

    def snapshots() -> dict[str, dict]:
        response = json.loads(
            run(ws_ckpt, "list", "--workspace", workspace, "--format", "json", "--limit", "100")
        )
        return {row["id"]: row["meta"] for row in response["snapshots"]}

    def submission_reason(request_id: str, agent: dict[str, str], session: str, turn: str) -> str:
        # The metadata-only AW journal does not retain Provider effects.
        request = {
            "api_version": "aw-provider/v1alpha1",
            "request_id": request_id,
            "method": "invoke",
            "operation": "checkpoint",
            "config_revision": hashlib.sha256(config.read_bytes()).hexdigest(),
            "budget_ms": 4000,
            "allowed_effects": ["observe"],
            "config": {},
            "event": {
                "name": "input.submit",
                "agent": agent,
                "session_id": session,
                "turn_id": turn,
                "input": {"text": "check this input"},
                "native": {},
            },
        }
        wire = json.dumps(request, sort_keys=True, separators=(",", ":"))
        request["input_digest"] = "sha256:" + hashlib.sha256(wire.encode()).hexdigest()
        reply = json.loads(run(provider, "--cli", traced_cli, input_text=json.dumps(request)))
        assert reply["status"] == "ok", reply
        return reply["effects"][0]["reason_code"]

    run(aw, "install", "--bundle", bundle, "--prefix", prefix)
    run(
        aw, "config", "init", "--prefix", prefix, "--config", config, "--state-dir", state,
        "--qoder", root / "bin/qoder-fixture", "--template", bundle / "templates/ws-ckpt.json",
        "--template-var", f"PROVIDERS_DIR={provider.parent.parent}",
        "--template-var", f"WS_CKPT_CLI={traced_cli}",
    )
    data.write_text("original\n")
    run(ws_ckpt, "init", "--workspace", workspace)
    try:
        assert json.loads(run(sys.executable, skill, "bind", "--workspace", workspace))["enabled"] is False
        submit("default-off", "ok")
        assert not call_log.exists() and snapshots() == {}, "default-off submission attempted a snapshot"
        print("PASS default off: zero checkpoint CLI calls", flush=True)

        assert json.loads(run(sys.executable, skill, "enable", cwd=workspace))["enabled"] is True
        submit("inside-workspace", "ok", "error", cwd=workspace)
        status = json.loads(run(sys.executable, skill, "status"))
        assert status["enabled"] is True, status
        assert status["last_result"] == "checkpoint_cwd_inside_workspace", status
        assert not call_log.exists() and snapshots() == {}, "unsafe launch invoked checkpoint CLI"
        print("PASS switch enabled inside workspace; Provider blocked the checkpoint CLI call", flush=True)

        submit("enabled", "ok", "error", "ok")
        enabled = snapshots()
        assert len(enabled) == 1, enabled
        snapshot_id, meta = next(iter(enabled.items()))
        assert meta["metadata"] == {"auto": True, "source": "aw", "event": "input.submit"}, meta
        assert call_log.read_text().splitlines() == ["checkpoint"]
        assert json.loads(run(sys.executable, skill, "status"))["last_result"] in {
            f"checkpoint_created.{snapshot_id}", f"checkpoint_present.{snapshot_id}",
        }
        print("PASS input.submit created a real Btrfs snapshot", flush=True)

        assert json.loads(run(sys.executable, skill, "disable"))["enabled"] is False
        data.write_text("manual snapshot contents\n")
        submit("disabled", "ok", "error", "ok", "ok")
        disabled_reason = submission_reason(
            "disabled-probe", {"adapter": "qoder", "binding_id": "qoder", "instance_id": "qoder-probe"},
            "fixture-session", "disabled-probe",
        )
        assert disabled_reason == "checkpoint_disabled", disabled_reason
        assert call_log.read_text().splitlines() == ["checkpoint"], (
            "disabled submission invoked checkpoint CLI"
        )
        assert snapshots().keys() == enabled.keys(), "disabled submission added a snapshot"
        print("PASS checkpoint_disabled: zero additional checkpoint CLI calls and zero new snapshots", flush=True)

        run(ws_ckpt, "checkpoint", "--workspace", workspace, "--snapshot", "manual-save")
        assert "manual-save" in snapshots(), "manual snapshot was not registered"
        data.write_text("changed after manual snapshot\n")
        run(ws_ckpt, "rollback", "--workspace", workspace, "--snapshot", "manual-save")
        assert data.read_text() == "manual snapshot contents\n", "rollback did not restore file contents"
        print("PASS manual snapshot and rollback with automatic snapshots off", flush=True)

        run(provider, "bind", "--agent", "claude", "--workspace", workspace)
        run(
            provider, "policy", "enable", "--workspace", workspace,
            env={**os.environ, "WS_CKPT_AW_TARGET": "claude"},
        )
        first_scope = {"adapter": "claude-sdk", "binding_id": "claude", "instance_id": "scope-one"}
        second_scope = {**first_scope, "instance_id": "scope-two"}
        data.write_text("first Scope contents\n")
        first_reason = submission_reason("first-submission", first_scope, "default", "1")
        assert first_reason.startswith("checkpoint_created."), first_reason
        first = first_reason.split(".", 1)[1]
        assert submission_reason("retry", first_scope, "default", "1") == f"checkpoint_present.{first}"
        data.write_text("second Scope contents\n")
        second_reason = submission_reason("second-submission", second_scope, "default", "1")
        assert second_reason.startswith("checkpoint_created."), second_reason
        second = second_reason.split(".", 1)[1]
        assert first != second and {first, second} <= snapshots().keys()
        for snapshot, content in ((first, "first Scope contents\n"), (second, "second Scope contents\n")):
            run(ws_ckpt, "rollback", "--workspace", workspace, "--snapshot", snapshot)
            assert data.read_text() == content, snapshot
        print("PASS Scope one retry reused its snapshot; Scope two created a separate restorable snapshot", flush=True)
    finally:
        if (state / "aw.sock").is_socket():
            run(aw, "service", "stop", "--config", config)


if __name__ == "__main__":
    main(Path(sys.argv[1]))
