"""Pinned native Codex CLI with disposable configuration and a local model; no cloud keys."""

import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

from codex_model import Model


def run(argv, cwd, environment, evidence, expected=0, timeout=40):
    index = len(list(evidence.glob("command-*.log")))
    log = evidence / f"command-{index:02}.log"
    with log.open("wb") as stream, subprocess.Popen(argv, cwd=cwd, env=environment,
            stdin=subprocess.DEVNULL, stdout=stream, stderr=subprocess.STDOUT,
            start_new_session=True) as child:
        try:
            with (evidence / "processes.jsonl").open("a") as ledger:
                ledger.write(json.dumps({"command": list(map(str, argv)), "cwd": str(cwd),
                    "pid": child.pid, "pgid": child.pid, "ports": [], "log": str(log),
                    "timeout_seconds": timeout, "stop": f"kill -INT -- -{child.pid}"}) + "\n")
            code = child.wait(timeout=timeout)
        except BaseException:
            # Let AW reap its separate native/Provider process groups before escalation.
            try:
                os.killpg(child.pid, signal.SIGINT)
            except ProcessLookupError:
                pass
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
            raise
        finally:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait()
    text = log.read_text(errors="replace")
    assert code == expected, (argv, code, text[-12000:])
    return text


def document(aw, codex, state, mode, evidence):
    policy = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": "codex-native"},
        "spec": {"daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": str(state)},
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {"codex": {"adapter": "codex", "argv": [str(codex), "exec"]}},
            "providers": {"policy": {"protocol": "aw-provider/v1alpha1", "transport": {
                "type": "stdio", "location": "agent", "argv": [sys.executable,
                str(Path(__file__).with_name("codex_policy.py")), mode, str(evidence / "events.jsonl")]},
                "config": {}, "timeout_ms": 1000, "max_output_bytes": 4096}},
            "events": {
                "input.submit": {"enabled": True, "steps": [
                    {"id": "prompt-security", "provider": "policy", "operation": "scan_prompt",
                     "effects": ["observe", "block"], "on_error": "block"}]},
                "tool.before": {"enabled": True, "steps": [
                    {"id": "security", "provider": "policy", "operation": "scan", "effects": ["observe", "block"], "on_error": "block"},
                    {"id": "retry", "provider": "policy", "operation": "retry", "effects": ["observe", "block"], "on_error": "report"}]},
                "tool.after": {"enabled": True, "steps": [
                    {"id": "record", "provider": "policy", "operation": "record", "effects": ["observe"], "on_error": "report"}]}}}}
    if mode.startswith("replace"):
        policy["spec"]["events"]["tool.after"]["steps"][0]["effects"] = ["observe", "replace_result"]
        policy["spec"]["events"]["tool.after"]["steps"][0]["on_error"] = "withhold_result"
    return policy


def case(args, mode):
    evidence = args.output / mode
    evidence.mkdir()
    with tempfile.TemporaryDirectory(prefix="aw-codex-") as temporary:
        root = Path(temporary)
        home, workspace, state = root / "native", root / "work", root / "state"
        home.mkdir(mode=0o700)
        workspace.mkdir()
        model = Model(evidence, mode)
        port = model.server.server_port
        (evidence / "model.json").write_text(json.dumps({"pid": os.getpid(), "port": port,
            "deadline_seconds": 40, "stop": "owned server.shutdown/server_close in finally"}))
        environment = {"PATH": os.environ["PATH"], "HOME": str(root), "CODEX_HOME": str(home),
                       "NO_PROXY": "127.0.0.1,localhost", "TERM": "dumb"}
        model_name = "gpt-6-astra" if mode == "replace-apply" else "gpt-5.4"
        native = (f'model="{model_name}"\nmodel_provider="fixture"\nmodel_reasoning_effort="low"\n'
                  'approval_policy="never"\nsandbox_mode="danger-full-access"\n'
                  '[model_providers.fixture]\nname="fixture"\nwire_api="responses"\n'
                  f'base_url="http://127.0.0.1:{port}/v1"\nrequires_openai_auth=false\nsupports_websockets=false\n'
                  f'[features]\ncode_mode={str(mode == "replace-code-mode").lower()}\n')
        if mode in ("replace-mcp", "replace-mcp-large", "replace-media"):
            mcp_args = [str(Path(__file__).with_name("codex_mcp.py"))]
            if mode == "replace-media":
                mcp_args.append("media")
            if mode == "replace-mcp-large":
                mcp_args.append("large")
            native += ('[mcp_servers.fixture]\n'
                       f'command={json.dumps(sys.executable)}\n'
                       f'args={json.dumps(mcp_args)}\n')
        if mode == "disabled":
            native += 'hooks=false\n'
        (home / "config.toml").write_text(native)
        config = root / "aw.json"
        policy = document(args.aw, args.codex, state, mode, evidence)
        if mode == "native-rewrite-required":
            policy["spec"]["events"]["tool.before"]["required"] = True
        config.write_text(json.dumps(policy))
        base = [str(args.aw), "run", "--config", str(config), "--agent", "codex", "--",
                "--ephemeral", "--json", "--skip-git-repo-check"]
        try:
            run([str(args.aw), "install", "--config", str(config), "--agent", "codex"], workspace, environment, evidence)
            hooks = home / "hooks.json"
            installed = hooks.read_bytes()
            if mode == "native-deny":
                value = json.loads(installed)
                value["hooks"]["PreToolUse"].append({"matcher": "Bash", "hooks": [
                    {"type": "command", "command": "printf 'Native user denial' >&2; exit 2", "timeout": 3}]})
                hooks.write_text(json.dumps(value))
                installed = hooks.read_bytes()
            if mode.startswith("native-rewrite"):
                rewrite = root / "rewrite.py"
                rewrite.write_text('import json, sys\n'
                    'event = json.load(sys.stdin)\n'
                    'updated = dict(event["tool_input"])\n'
                    'updated["command"] = "printf REWRITTEN > AW_REWRITTEN"\n'
                    'print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", '
                    '"permissionDecision": "allow", "updatedInput": updated}}))\n')
                value = json.loads(installed)
                value["hooks"]["PreToolUse"].append({"matcher": "Bash", "hooks": [
                    {"type": "command", "command": f"{sys.executable} {rewrite}", "timeout": 3}]})
                hooks.write_text(json.dumps(value))
                installed = hooks.read_bytes()
            if mode == "replace-peer":
                value = json.loads(installed)
                value["hooks"]["PostToolUse"].append({"matcher": ".*", "hooks": [
                    {"type": "command", "command": "printf '{}'", "timeout": 3}]})
                hooks.write_text(json.dumps(value))
                installed = hooks.read_bytes()
            # This explicit native switch is restricted to our disposable, vetted fixture.
            if mode not in ("untrusted", "disabled"):
                base.append("--dangerously-bypass-hook-trust")
            output = run(base + ["--", "Run the fixture tool exactly once."], workspace, environment, evidence,
                         expected=1 if mode in ("untrusted", "disabled", "native-rewrite-required",
                                                "replace-peer", "replace-code-mode") else 0)
            assert hooks.read_bytes() == installed
            assert (home / "config.toml").read_text() == native
            assert not (workspace / "AW_MUST_NOT_EXIST").exists()
            if mode in ("untrusted", "disabled", "native-rewrite-required", "replace-peer", "replace-code-mode"):
                assert not model.requests
                assert not state.exists()
                if mode == "untrusted":
                    assert "trust" in output
                elif mode == "disabled":
                    assert "disabled" in output
                elif mode == "native-rewrite-required":
                    assert "cannot share PreToolUse" in output
                else:
                    assert "result replacement requires" in output, output
            else:
                if mode in ("prompt-deny", "offline"):
                    assert not model.requests
                    assert not (workspace / "AW_MUST_NOT_EXIST").exists()
                else:
                    assert "AW_CODEX_DONE" in output
                outputs = json.dumps(model.outputs)
                events = [json.loads(line) for line in (evidence / "events.jsonl").read_text().splitlines()]
                after = [r for r in events if r["operation"] == "record"]
                if mode in ("allow", "after-failure"):
                    assert "AW_NATIVE_OUTPUT" in outputs and "AW_NATIVE_ERROR" in outputs, outputs
                    assert "Process exited with code 7" in outputs, outputs
                    assert len(after) == 1
                if mode in ("replace", "replace-apply", "replace-mcp", "replace-long"):
                    assert model.outputs[-1:] == ["AW_REDACTED_RESULT"], model.outputs
                    sent = [item["output"] for request in model.requests[1:]
                            for item in request["input"] if item.get("type") in ("function_call_output", "custom_tool_call_output")]
                    assert sent[-len(model.outputs):] == model.outputs, sent
                    if mode in ("replace", "replace-apply", "replace-mcp"):
                        assert sent == ["AW_REDACTED_RESULT"], sent
                    if mode == "replace-long":
                        assert "AW_LONG_DONE" not in json.dumps(sent), sent
                    assert len(after) == 1
                if mode == "replace-apply":
                    assert (workspace / "AW_PATCHED").read_text() == "created\n"
                if mode == "replace-mcp":
                    assert after[0]["event"]["tool"]["name"] == "mcp__fixture__echo"
                    assert "AW_MCP_ORIGINAL" not in json.dumps(model.requests)
                if mode == "replace-mcp-large":
                    assert model.outputs == ["AW result withheld"], model.outputs
                    assert "AW_MCP_ORIGINAL" not in json.dumps(model.requests)
                    assert not after, after
                if mode == "replace-media":
                    assert model.outputs == ["AW result withheld"], model.outputs
                    assert after[0]["event"]["tool"].get("result_projection") is None
                if mode == "replace-long":
                    assert model.calls >= 2
                if mode == "replace-parallel":
                    delivered = {item["call_id"]: item["output"] for request in model.requests[1:]
                                 for item in request["input"]
                                 if item.get("type") == "function_call_output"}
                    assert delivered == {"call_1": "AW_REDACTED_1", "call_2": "AW_REDACTED_2"}, delivered
                    assert "AW_SECRET_A" not in json.dumps(delivered)
                    assert "AW_SECRET_B" not in json.dumps(delivered)
                    originals = {event["event"]["tool"]["call_id"]: event["event"]["tool"]["result_projection"]["text"]
                                 for event in after}
                    assert "AW_SECRET_A" in originals["call_1"] and "AW_SECRET_B" in originals["call_2"]
                    assert len(after) == 2
                if mode == "replace-failure":
                    assert model.outputs == ["AW result withheld"], model.outputs
                    assert [item["output"] for request in model.requests[1:]
                            for item in request["input"] if item.get("type") == "function_call_output"] == model.outputs
                    assert len(after) == 1
                if mode == "long":
                    assert "AW_LONG_DONE" in outputs, outputs
                    assert len(after) == 1 and model.calls >= 2
                    assert {r["event"]["tool"]["call_id"] for r in events if "tool" in r["event"]} == {"call_1"}
                if mode in ("deny", "offline", "native-deny", "retry"):
                    assert not after
                if mode == "native-deny":
                    assert "Native user denial" in outputs and "security_denied" not in outputs, outputs
                    assert {r["operation"] for r in events} >= {"scan", "retry"}
                if mode == "native-rewrite":
                    assert (workspace / "AW_REWRITTEN").read_text() == "REWRITTEN"
                    assert {r["operation"] for r in events} >= {"scan", "retry"}
                if mode == "native-rewrite-deny":
                    assert "Blocked by AW policy" in outputs, outputs
                    assert not (workspace / "AW_REWRITTEN").exists()
                if mode == "retry":
                    assert len({r["event"]["tool"]["call_id"] for r in events if "tool" in r["event"]}) == 2
                    assert "Retry with" in outputs and "blocked" in outputs.lower(), outputs
                status = json.loads(run([str(args.aw), "status", "--config", str(config)], workspace, environment, evidence))["result"]
                assert status["bindings"] == 0 and status["events"] == 0, status
                assert not list(state.glob("launch-*"))
        finally:
            try:
                if (state / "aw.sock").exists():
                    run([str(args.aw), "stop", "--config", str(config)], workspace, environment, evidence)
                    deadline = time.monotonic() + 5
                    while (state / "aw.sock").exists() and time.monotonic() < deadline:
                        time.sleep(0.02)
                    assert not (state / "aw.sock").exists()
            finally:
                model.close()
        assert not (home / "sessions").exists(), "ephemeral native test persisted sessions"
    return {"case": mode, "passed": True, "cleaned": not root.exists()}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--expected-version", required=True)
    args = parser.parse_args()
    args.aw = args.aw.resolve()
    args.codex = args.codex.absolute()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True)
    version = subprocess.check_output([args.codex, "--version"], text=True, timeout=5).strip()
    assert version == f"codex-cli {args.expected_version}", version
    results = []
    for mode in ("untrusted", "disabled", "prompt-deny", "allow", "deny", "offline", "retry", "native-deny", "native-rewrite", "native-rewrite-deny", "native-rewrite-required", "after-failure", "long", "replace", "replace-failure", "replace-peer", "replace-code-mode", "replace-apply", "replace-mcp", "replace-mcp-large", "replace-media", "replace-long", "replace-parallel"):
        results.append(case(args, mode))
        print(f"Codex native {mode}: passed", flush=True)
    (args.output / "result.json").write_text(json.dumps({"version": version, "results": results}, indent=2))


def interrupted(_signal, _frame):
    raise InterruptedError("native test cancelled")


if __name__ == "__main__":
    os.umask(0o077)
    signal.signal(signal.SIGTERM, interrupted)
    main()
