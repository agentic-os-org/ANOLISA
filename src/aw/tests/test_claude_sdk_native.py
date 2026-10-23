"""Opt-in real Claude SDK/CLI test using an owned local HTTP model fixture."""

from __future__ import annotations
import argparse
import asyncio
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from typing import Any

sys.dont_write_bytecode = True

from claude_sdk_callbacks import (
    check_callback_payloads,
    check_callback_timeouts,
    check_preserved_callbacks,
)


def document(
    python: str,
    worker: Path,
    state: Path,
    mode: str,
    tokenless: Path | None = None,
    tokenless_provider: Path | None = None,
) -> dict[str, Any]:
    value = {
        "apiVersion": "aw/v1alpha1",
        "kind": "AWConfiguration",
        "metadata": {"name": "claude-native"},
        "spec": {
            "daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": str(state)},
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {"claude": {"adapter": "claude-sdk", "argv": [python, str(worker)]}},
            "providers": {
                "policy": {
                    "transport": {
                        "type": "stdio",
                        "argv": [python, str(worker.parent / "policy.py")],
                        "location": "agent",
                    },
                    "timeout_ms": 1000,
                    "max_output_bytes": 4096,
                }
            },
            "events": {
                "tool.before": {
                    "enabled": True,
                    "required": True,
                    "steps": [
                        {
                            "id": "check",
                            "provider": "policy",
                            "effects": ["observe", "block"],
                            "on_error": "report" if mode == "before-report-failure" else "block",
                        }
                    ],
                },
                "tool.after": {
                    "enabled": True,
                    "required": True,
                    "steps": [
                        {
                            "id": "record",
                            "provider": "policy",
                            "effects": ["observe"],
                            "on_error": "report",
                        }
                    ],
                },
            },
        },
    }

    if mode == "many-denials":
        value["spec"]["events"]["tool.before"]["steps"].append(
            {"id": "later", "provider": "policy", "effects": ["observe"], "on_error": "report"}
        )
    if mode.startswith("native-hook-"):
        value["spec"]["providers"]["native"] = {
            **value["spec"]["providers"]["policy"],
            "protocol": "native-hook/v1alpha1",
            "config": {},
        }
        value["spec"]["events"]["tool.before"]["steps"].extend(
            [
                {
                    "id": "native",
                    "provider": "native",
                    "native": {},
                    "on_error": "block" if mode == "native-hook-invalid-chain" else "report",
                },
                {"id": "later", "provider": "policy", "effects": ["observe"], "on_error": "report"},
            ]
        )
        if mode == "native-hook-invalid-chain":
            value["spec"]["execution"]["default_event_budget_ms"] = 55000
    if tokenless is not None:
        if tokenless_provider is None:
            raise ValueError("Tokenless acceptance requires an explicit Provider executable")
        value["spec"]["providers"]["tokenless"] = {
            "protocol": "aw-provider/v1alpha1",
            "transport": {
                "type": "stdio",
                "location": "agent",
                "argv": [
                    str(tokenless_provider),
                    "--cli",
                    str(
                        tokenless
                        if mode not in {"bash-compress-failure", "bash-compress-tool-failure"}
                        else Path("/bin/false")
                    ),
                    "--data-dir",
                    str(state.parent / "tokenless"),
                ],
            },
            "timeout_ms": 2000,
            "max_output_bytes": 1048576,
            "config": {"version": 1},
        }
        if mode in {"bash-pretool", "bash-pretool-deny"}:
            value["spec"]["events"]["tool.before"]["steps"].append(
                {
                    "id": "rewrite",
                    "provider": "tokenless",
                    "operation": "pre_tool",
                    "effects": ["block"],
                    "on_error": "report",
                }
            )
        value["spec"]["events"]["tool.after"]["steps"].append(
            {
                "id": "compress",
                "provider": "tokenless",
                "operation": "post_tool",
                "effects": ["replace_result"],
                "on_error": "report",
            }
        )
    return value


def run_case(
    aw: Path,
    worker: Path,
    output: Path,
    mode: str,
    tokenless: Path | None = None,
    tokenless_provider: Path | None = None,
) -> dict[str, Any]:
    state_parent = Path(tempfile.mkdtemp(prefix="s-", dir=output))
    state = state_parent / "s"
    socket_path = state / "aw.sock"
    if len(os.fsencode(socket_path)) > 107:
        state_parent.rmdir()
        raise ValueError(
            f"Linux UDS path exceeds 107 bytes: {socket_path}; choose a shorter target output path"
        )
    case = Path(tempfile.mkdtemp(prefix=mode + "-", dir=output))
    config = case / "aw.json"
    config.write_text(
        json.dumps(document(sys.executable, worker, state, mode, tokenless, tokenless_provider))
    )
    if mode in {"bash-pretool", "bash-pretool-deny"}:
        subprocess.run(["git", "init", "-q", str(case)], check=True, timeout=10)
    env = {
        **os.environ,
        "CLAUDE_CONFIG_DIR": str(case / "claude-profile"),
        "CLAUDE_TEST_MODE": mode,
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        "TOKENLESS_STATS_ENABLED": "0",
        "TOKENLESS_SLS_ENABLED": "0",
        "TOKENLESS_DATA_DIR": str(state.parent / "tokenless"),
        "RTK_DB_PATH": str(state.parent / "rtk.db"),
    }
    command = [str(aw), "run", "--config", str(config), "--agent", "claude"]
    process = None
    daemon_pid = None
    try:
        record = {
            "command": command,
            "cwd": str(case),
            "pid": None,
            "ports": [],
            "stdout": str(case / "stdout.log"),
            "stderr": str(case / "stderr.log"),
            "timeout_seconds": 120 if mode in {"many-denials", "native-hook-invalid-chain"} else 45,
            "stop": None,
        }
        (case / "process.json").write_text(json.dumps(record))
        process = subprocess.Popen(
            command,
            cwd=case,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        record.update(pid=process.pid, stop=f"kill -TERM -- -{process.pid}")
        (case / "process.json").write_text(json.dumps(record))
        stdout, stderr = process.communicate(timeout=record["timeout_seconds"])
        (case / "stdout.log").write_text(stdout)
        (case / "stderr.log").write_text(stderr)
        if mode in {
            "unready",
            "settings",
            "cli-path",
            "plugins",
            "extra-args",
            "permission-hook-rewrite",
            "permission-prompt",
        }:
            assert process.returncode == 1, stderr
            if mode == "permission-hook-rewrite":
                assert "AW tool.before does not support PermissionRequest hooks" in stderr
            if mode == "permission-prompt":
                assert "AW tool.before does not support permission_prompt_tool_name" in stderr
            assert "native hooks ready" not in stderr
            assert not (case / "effect").exists()
            status = json.loads(
                subprocess.check_output(
                    [str(aw), "status", "--config", str(config)], env=env, timeout=10
                )
            )
            assert status["result"]["bindings"] == status["result"]["events"] == 0
            return {"mode": mode, "output": str(case), "events": 0}
        assert process.returncode == 0, stderr
        assert "AW claude-sdk native hooks ready" in stderr
        status = json.loads(
            subprocess.check_output(
                [str(aw), "status", "--config", str(config)], env=env, timeout=10
            )
        )
        assert status["result"]["bindings"] == status["result"]["events"] == 0, status
        daemon_pid = status["result"]["pid"]
        (case / "service-process.json").write_text(
            json.dumps(
                {
                    "pid": daemon_pid,
                    "state": str(state),
                    "socket": str(state / "aw.sock"),
                    "stop": [str(aw), "stop", "--config", str(config)],
                }
            )
        )
        report = json.loads((case / "report.json").read_text())
        allowed = mode in {
            "allow",
            "failure",
            "parallel",
            "multi-turn",
            "before-report-failure",
            "after-report-failure",
            "strict-mcp-config",
            "many-denials",
            "bash-compress",
            "bash-compress-observer-failure",
            "bash-compress-native-rewrite",
            "bash-compress-failure",
            "bash-pretool",
            "mcp-error-compress",
            "native-hook-neutral",
            "native-hook-rewrite",
            "native-hook-invalid-chain",
        }
        assert report["effect"] == allowed, report
        events = report["events"]
        before = [event for event in events if event["hook_event_name"] == "PreToolUse"]
        after = [
            event
            for event in events
            if event["hook_event_name"] in {"PostToolUse", "PostToolUseFailure"}
        ]
        expected = (
            35
            if mode in {"many-denials", "native-hook-invalid-chain"}
            else 2 if mode in {"parallel", "multi-turn", "bash-pretool", "bash-pretool-deny"} else 1
        )
        after_count = (
            1
            if mode in {"many-denials", "native-hook-invalid-chain", "bash-pretool"}
            else 0 if mode == "bash-pretool-deny" else expected
        )
        assert len(before) == expected, report
        assert len({event["session_id"] for event in events}) == 1, report
        assert len({event["tool_use_id"] for event in before}) == expected, report
        if mode in {
            "many-denials",
            "native-hook-invalid-chain",
            "bash-pretool",
            "bash-pretool-deny",
        }:
            assert len(after) == after_count, report
            assert {event["tool_use_id"] for event in after} <= {
                event["tool_use_id"] for event in before
            }, report
        elif allowed or mode in {"bash-failure", "bash-compress-tool-failure"}:
            assert {event["tool_use_id"] for event in before} == {
                event["tool_use_id"] for event in after
            }, report
        else:
            assert not after, report
        if mode in {"bash-failure", "bash-compress-tool-failure"}:
            assert after[0]["hook_event_name"] == "PostToolUseFailure", report
        if mode == "permission-rewrite":
            assert (case / "permission-called").exists(), report
        journals = []
        for path in (state / "journal").glob("*.jsonl"):
            records = [json.loads(line)["record"] for line in path.read_text().splitlines()]
            journals.extend(records)
        plans = [
            record["plan"]["event"]
            for record in journals
            if record.get("plan", {}).get("kind") == "event"
        ]
        assert len([event for event in plans if event["name"] == "tool.before"]) == expected, plans
        if allowed or mode in {"bash-failure", "bash-compress-tool-failure"}:
            assert (
                len([event for event in plans if event["name"] == "tool.after"]) == after_count
            ), plans
        completed = [record for record in journals if record.get("phase") == "completed"]
        assert completed, journals
        if mode.startswith("native-hook-"):
            if mode != "native-hook-deny-exit2":
                assert "native-local-diagnostic" in stderr, stderr
                assert "native-local-diagnostic" not in json.dumps(report["tool_results"]), report
            native = json.loads((case / "native-input.json").read_text())
            assert native == before[-1], native
            assert any(
                record.get("kind") == "native" and record["status"] == "ok" for record in completed
            )
            started = [record["step_id"] for record in journals if record.get("phase") == "started"]
            assert ("later" in started) == allowed, started
            if mode == "native-hook-invalid-chain":
                assert started.count("native") == 35 and started.count("later") == 1, started
                assert "invalid_native_response" in stderr and "aw hook failed:" in stderr, stderr
            if not allowed:
                assert "invalid_native_response" not in stderr, stderr
                assert any(
                    record.get("phase") == "closed" and record["status"] == "closed"
                    for record in journals
                )
                if mode != "native-hook-deny-no-reason":
                    assert "native command veto" in json.dumps(report["tool_results"]), report
            if mode == "native-hook-rewrite":
                assert "invalid_native_response" in stderr, stderr
                assert not (case / "policy-rewritten").exists()
        if mode in {
            "before-report-failure",
            "after-report-failure",
            "bash-compress-observer-failure",
            "bash-compress-failure",
        }:
            assert "aw hook failed:" in stderr, stderr
            assert any(
                record.get("status") == "error" and record.get("failure_action") == "report"
                for record in completed
            ), completed
        elif mode == "hook-failure":
            assert any(
                record.get("status") == "error" and record.get("failure_action") == "block"
                for record in completed
            ), completed
        elif mode == "many-denials":
            assert any(
                record.get("status") == "error" and record.get("failure_action") == "block"
                for record in completed
            ), completed
            assert any(record.get("policy_block") is True for record in completed), completed
        else:
            assert all(record.get("status") == "ok" for record in completed), completed
        if mode in {"deny", "native-allow-aw-deny"}:
            assert any(record.get("policy_block") is True for record in completed), completed
        if tokenless is not None:
            policies = [
                json.loads(line) for line in (case / "policy-events.jsonl").read_text().splitlines()
            ]
            assert all("result_projection" not in event["tool"] for event in policies)
            if mode.startswith("bash-compress") and mode != "bash-compress-tool-failure":
                original = report["original"]
                observed = [
                    event["tool"]["result"]["stdout"]
                    for event in policies
                    if event["name"] == "tool.after"
                ]
                assert observed == [original], observed
                if mode == "bash-compress-failure":
                    assert original in [
                        block["content"] for block in report["tool_results"]
                    ], report
                else:
                    result = report["tool_results"][-1]["content"]
                    assert len(result) < len(original), result
                    assert json.loads(result) == json.loads(original), result
            if mode in {"bash-pretool", "bash-pretool-deny"}:
                retry = json.loads((case / "retry.json").read_text())
                assert "TOKENLESS_AGENT_ID=claude " in retry["command"], retry
                assert before[0]["tool_input"]["command"] == "git status"
                assert before[1]["tool_input"] == retry
                if mode == "bash-pretool":
                    assert json.loads((case / "permission-input.json").read_text()) == retry
                else:
                    assert not (case / "permission-called").exists()
                assert before[0]["tool_use_id"] != before[1]["tool_use_id"]
                assert not any(event["tool_use_id"] == before[0]["tool_use_id"] for event in after)
                assert [
                    event["tool"]["input"]["command"]
                    for event in policies
                    if event["name"] == "tool.before"
                ] == ["git status", retry["command"]]
        if mode in {"many-denials", "native-hook-invalid-chain"}:
            terminal = [record for record in journals if record.get("phase") == "closed"]
            assert (
                len([record for record in terminal if record["status"] == "closed"]) == 34
            ), terminal
            assert not any(record["status"] == "expired" for record in terminal), terminal
        return {
            "mode": mode,
            "output": str(case),
            "session_id": before[0]["session_id"],
            "events": len(plans),
        }
    finally:
        if process is not None and process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
        stopped = subprocess.run(
            [str(aw), "stop", "--config", str(config)],
            env=env,
            capture_output=True,
            text=True,
            timeout=10,
        )
        (case / "stop.log").write_text(stopped.stdout + stopped.stderr)
        for _ in range(30):
            if not (state / "aw.sock").exists():
                break
            time.sleep(0.1)
        assert not (state / "aw.sock").exists(), "owned daemon socket remains"
        if state.exists():
            shutil.copytree(state, case / "state-evidence")
        shutil.rmtree(state_parent)
        assert not state_parent.exists()


def interrupted(signum: int, frame: Any) -> None:
    """Unwind run_case cleanup when the bounded test is terminated."""
    raise InterruptedError(f"received signal {signum}")


def main() -> None:
    signal.signal(signal.SIGTERM, interrupted)
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument(
        "--tokenless",
        type=Path,
        help="Also verify Bash optimization using a built Protocol v2 CLI and RTK on PATH",
    )
    parser.add_argument("--tokenless-provider", type=Path, help="Tokenless Provider executable")
    args = parser.parse_args()
    if (args.tokenless is None) != (args.tokenless_provider is None):
        parser.error("--tokenless and --tokenless-provider must be supplied together")
    args.output.mkdir(parents=True, exist_ok=True)
    asyncio.run(check_callback_timeouts(args.output.resolve()))
    asyncio.run(check_callback_payloads(args.output.resolve()))
    asyncio.run(check_preserved_callbacks())
    worker = (
        Path(__file__).resolve().parents[1]
        / "crates/aw-cli/tests/fixtures/claude-sdk/native_worker.py"
    )
    results = [
        run_case(args.aw.resolve(), worker, args.output.resolve(), mode)
        for mode in [
            "allow",
            "deny",
            "hook-failure",
            "before-report-failure",
            "after-report-failure",
            "failure",
            "bash-failure",
            "native-deny",
            "rewrite",
            "native-allow-aw-deny",
            "native-hook-deny",
            "native-hook-deny-no-reason",
            "native-hook-deny-exit2",
            "native-hook-deny-rewrite",
            "native-hook-neutral",
            "native-hook-rewrite",
            "native-hook-invalid-chain",
            "permission-rewrite",
            "permission-hook-rewrite",
            "permission-prompt",
            "many-denials",
            "parallel",
            "multi-turn",
            "strict-mcp-config",
            "unready",
            "settings",
            "cli-path",
            "plugins",
            "extra-args",
        ]
    ]
    if args.tokenless is not None:
        for mode in (
            "bash-compress",
            "bash-compress-observer-failure",
            "bash-compress-native-rewrite",
            "bash-compress-failure",
            "bash-compress-tool-failure",
            "bash-pretool",
            "bash-pretool-deny",
            "mcp-error-compress",
        ):
            results.append(
                run_case(
                    args.aw.resolve(),
                    worker,
                    args.output.resolve(),
                    mode,
                    args.tokenless.resolve(),
                    args.tokenless_provider.resolve(),
                )
            )
    sessions = [result["session_id"] for result in results if "session_id" in result]
    assert len(set(sessions)) == len(sessions)
    (args.output / "results.json").write_text(json.dumps(results, indent=2))
    print(f"{len(results)} real SDK/CLI scenarios passed")


if __name__ == "__main__":
    main()
