"""Native 0.166.0 fixed-thread, reviewed-home and credential gateway checks."""

import json
from pathlib import Path
import select
import subprocess
import sys
import time

from test_codex_app_server import Rpc, initialize, start_child, stop_child, turn
from test_codex_native import document, run


def check_redirected_stdio(argv, workspace, environment, output):
    # Echo a string ID larger than the test pipe in the native reply.
    request = {"id": "init-" + "x" * 20000, "method": "initialize",
               "params": {"clientInfo": {"name": "aw_native_test", "version": "0.1"}}}
    source = output / "gateway-input.jsonl"
    transcript = output / "gateway-transcript.jsonl"
    source.write_text(json.dumps(request) + "\n")
    with source.open("rb") as stdin, transcript.open("wb") as stdout:
        subprocess.run(argv, cwd=workspace, env=environment, stdin=stdin, stdout=stdout,
                       check=True, timeout=15)
    replies = [json.loads(line) for line in transcript.read_bytes().splitlines()]
    [expected] = [reply for reply in replies if reply.get("id") == request["id"]]
    assert "userAgent" in expected["result"]

    with source.open("rb") as stdin, subprocess.Popen(
        argv, cwd=workspace, env=environment, stdin=stdin, stdout=subprocess.PIPE, pipesize=4096,
    ) as child:
        try:
            assert select.select([child.stdout], [], [], 10)[0], "no gateway reply"
            time.sleep(0.35)
            stdout, _ = child.communicate(timeout=10)
        finally:
            child.kill()
    assert child.returncode == 0
    slow_replies = [json.loads(line) for line in stdout.splitlines()]
    assert [r for r in slow_replies if r.get("id") == request["id"]] == [expected]

    subprocess.run(argv, cwd=workspace, env=environment, stdin=subprocess.DEVNULL,
                   stdout=subprocess.PIPE, check=True, timeout=10)


def exercise_gateway(args, root, workspace, environment, pins, model):
    home = root / "gateway-home"
    home.mkdir(mode=0o700)
    environment = {**environment, "CODEX_HOME": str(home), "LOCAL_CODEX_GATEWAY_KEY": "fixture-key"}
    fixture = Path(__file__).resolve().parent
    config = root / "gateway.json"
    policy = document(args.aw, args.codex, root / "gateway-state", "replace-gateway", args.output)
    policy["spec"]["agents"]["codex"]["argv"] = [str(args.codex), "app-server", "--listen", "stdio://",
                                                 "-c", 'model_provider="gateway"']
    config.write_text(json.dumps(policy))
    gateway = subprocess.Popen(["node", fixture / "codex_gateway.mjs",
                                f"http://127.0.0.1:{model.server.server_port}/v1/responses",
                                home, json.dumps(pins)], stdout=subprocess.PIPE)
    try:
        assert select.select([gateway.stdout], [], [], 5)[0], "gateway did not start"
        setup = json.loads(gateway.stdout.readline())
        native = ('model="gpt-5.4"\napproval_policy="never"\nsandbox_mode="danger-full-access"\n'
                  '[shell_environment_policy]\ninherit="none"\n'
                  '[features]\nshell_tool=true\nunified_exec=true\ncode_mode=false\n[mcp_servers.fixture]\n'
                  f'command={json.dumps(sys.executable)}\nargs=[{json.dumps(str(fixture / "codex_mcp.py"))}]\n')
        (home / "config.toml").write_text(native + setup["trust"])
        run([args.aw, "connect", "--config", config, "--agent", "codex"], workspace, environment, args.output)
        argv = [args.aw, "run", "--config", config, "--agent", "codex", "--", *setup["argv"]]
        incomplete = run(argv[:-2], workspace, environment, args.output, expected=1)
        assert "gateway overrides" in incomplete
        child = start_child(argv, workspace, environment, args.output / "gateway-stderr.log")
        try:
            rpc = Rpc(child)
            initialize(rpc)
            check_redirected_stdio(argv, workspace, environment, args.output)
            direct = start_child([args.codex, "app-server"], workspace, environment,
                                 args.output / "unbound-stderr.log")
            try:
                initialize(Rpc(direct))
                socket = root / "gateway-state" / "aw.sock"
                assert json.loads(run([args.aw, "instance", "list", "--socket", socket],
                                      workspace, environment, args.output))["applications"] == []
                run([args.aw, "connect", "--socket", socket, "--sdk-pid", str(direct.pid),
                     "--timeout-ms", "250"], workspace, environment, args.output, expected=1)
                assert direct.poll() is None
            finally:
                stop_child(direct)
            assert "supported SDK adapter" in run(
                [args.aw, "instance", "prepare", "--config", config, "--agent", "codex"],
                workspace, environment, args.output, expected=1)
            for i, (method, params) in enumerate([
                ("thread/start", {"config": {"features": {"code_mode": True}}}),
                ("thread/start", {"config": {"hooks": {"PostToolUse": []}}}),
                ("thread/start", {"cwd": str(home)}), ("thread/resume", {"threadId": "old"}),
                ("thread/start", {"dynamicTools": []}), ("thread/start", {"modelProvider": "other"}),
                ("thread/fork", {}), ("config/value/write", {}), ("turn/start", {"threadId": "old", "input": []}),
            ], 20):
                rpc.send({"id": i, "method": method, "params": params})
                assert rpc.receive(lambda frame: frame.get("id") == i)["error"]["code"] == -32602
            for i, (mode, original) in enumerate([("replace-gateway", "sample-credential sample-pii"),
                                                  ("replace-mcp", "AW_MCP_ORIGINAL")]):
                model.mode, model.calls, model.outputs = mode, 0, []
                thread = turn(rpc, 100 + 2 * i, "Run the fixture tool exactly once.",
                              cwd=str(workspace), model="gpt-5.4", modelProvider="gateway",
                              config={"model_reasoning_effort": "low", "model_context_window": 32000})
                rpc.send({"id": 70 + i, "method": "turn/start", "params": {
                    "threadId": thread, "input": [{"type": "text", "text": "unused"}], "cwd": str(home),
                }})
                assert rpc.receive(lambda frame: frame.get("id") == 70 + i)["error"]["code"] == -32602
                assert len(model.outputs) == 1 and "[pii]" in model.outputs[0], model.outputs
                assert original not in model.outputs[0], model.outputs
                items = [frame["params"]["item"] for frame in rpc.pending if frame.get("method") == "item/completed"
                         and frame["params"].get("threadId") == thread]
                if i == 0:
                    assert all(token not in json.dumps(model.requests[-1]) for token in ("sample-credential", "sample-pii"))
                    assert "left [credential] [pii] right" in model.outputs[0], model.outputs
                    assert any(original in (item.get("aggregatedOutput") or "") for item in items), items
                else:
                    assert any(original in json.dumps(item.get("result")) for item in items), items
            model.mode, model.calls, model.outputs = "gateway-provider-failure", 0, []
            turn(rpc, 110, "Run the fixture tool exactly once.")
            assert model.outputs == ["AW result withheld"], model.outputs
        finally:
            try:
                stop_child(child)
            finally:
                run([args.aw, "service", "stop", "--config", config], workspace, environment, args.output)
        settings = home / "config.toml"
        for changed, expected in [
            ((native + setup["trust"]).replace(pins["post_tool_use"], "sha256:" + "0" * 64), "trust review"),
            ((native + setup["trust"]).replace("code_mode=false", "code_mode=true"), "code mode disabled"),
        ]:
            settings.write_text(changed)
            assert expected in run(argv, workspace, environment, args.output, expected=1)
        settings.write_text(native + setup["trust"])
        installed = home / "hooks.json"
        peers = json.loads(installed.read_text())
        peers["hooks"]["PostToolUse"].append({"matcher": "^Bash$", "hooks": [{
            "type": "mcp_tool", "server": "fixture", "tool": "credential_hook",
            "input": {"output": "${tool_response}"}, "timeout": 90,
        }]})
        installed.write_text(json.dumps(peers))
        assert "no other PostToolUse" in run(argv, workspace, environment, args.output, expected=1)
    finally:
        gateway.terminate()
        try:
            gateway.wait(timeout=3)
        except subprocess.TimeoutExpired:
            gateway.kill()
            gateway.wait(timeout=3)
        gateway.stdout.close()
