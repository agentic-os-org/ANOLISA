#!/usr/bin/env python3
"""Real Tokenless Provider/CLI adoption through Pi; streams, shell tools and RTK are controlled fixtures."""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument("--aw", type=Path, required=True)
parser.add_argument("--root", type=Path, required=True)
parser.add_argument("--tokenless-provider", type=Path, required=True)
parser.add_argument("--tokenless-cli", type=Path, required=True)
parser.add_argument("--sec-core-provider", type=Path, required=True)
args = parser.parse_args()
root = args.root.resolve()
root.mkdir(parents=True, exist_ok=True)
fixture = Path(__file__).resolve().parent
shutil.copyfile(fixture / "application.mjs", root / "application.mjs")
aw, tokenless, cli, scanner_provider = [str(path.resolve()) for path in
    [args.aw, args.tokenless_provider, args.tokenless_cli, args.sec_core_provider]]
node, python = shutil.which("node"), shutil.which("python3")
assert node and python


def call(command, *, env=None, input=None):
    with subprocess.Popen(command, cwd=root, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True, env=env, start_new_session=True) as process:
        record = {"command": command, "cwd": str(root), "pid": process.pid, "ports": [],
                  "deadline_seconds": 30, "stop": f"kill -TERM -- -{process.pid}"}
        (root / "tokenless-active-command.json").write_text(json.dumps(record))
        try:
            out, err = process.communicate(input, timeout=30)
        finally:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
        return process.returncode, out, err


def provider(argv, config):
    return {"protocol": "aw-provider/v1alpha1", "transport": {"type": "stdio", "location": "agent", "argv": argv},
            "timeout_ms": 4000, "max_output_bytes": 1048576, "config": config}


results = []
for case in ["json-selected-output", "rtk-retry-rescanned-denied", "rtk-retry-allowed-post-neutral"]:
    state = Path(tempfile.mkdtemp(prefix="t", dir=root))
    assert len(os.fsencode(state / "state/aw.sock")) < 108, "Choose a shorter --root"
    bin_dir = state / "bin"
    bin_dir.mkdir()
    rtk = bin_dir / "rtk"
    rtk.write_text("#!/bin/sh\nif [ \"$1\" = '--version' ]; then echo 'rtk 0.49.0'; elif [ \"$1\" = rewrite ]; then printf 'rtk %s\\n' \"$2\"; else exit 99; fi\n")
    rtk.chmod(0o700)
    scanner = state / "scanner"
    scanner.write_text(f'''#!{python}
import json,sys,pathlib,os
command=sys.argv[sys.argv.index('--code')+1]
with (pathlib.Path(__file__).parent/'scanned.jsonl').open('a') as output: output.write(json.dumps(command)+'\\n')
print(json.dumps({{'ok':True,'verdict':'deny' if os.environ.get('PI_SCAN_RETRY_DENY')=='1' and 'TOKENLESS_AGENT_ID=pi' in command else 'pass'}}))
''')
    scanner.chmod(0o700)
    cli_wrapper = state / "tokenless-cli"
    cli_wrapper.write_text(f'''#!{python}
import json,pathlib,subprocess,sys
raw=sys.stdin.buffer.read()
result=subprocess.run([{cli!r},*sys.argv[1:]],input=raw,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=20)
with (pathlib.Path(__file__).parent/'tokenless-wire.jsonl').open('a') as output:
 output.write(json.dumps({{'request':json.loads(raw),'response':json.loads(result.stdout),'exit_code':result.returncode}})+'\\n')
sys.stdout.buffer.write(result.stdout)
sys.stderr.buffer.write(result.stderr)
raise SystemExit(result.returncode)
''')
    cli_wrapper.chmod(0o700)
    document = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": "pi-tokenless"}, "spec": {
        "daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": str(state / "state")},
        "execution": {"guarantee": "native_hook", "default_event_budget_ms": 10000},
        "audit": {"enabled": True, "payload": "metadata_only"},
        "agents": {"pi": {"adapter": "pi", "argv": [node, str(root / "application.mjs")]}},
        "providers": {
            "scan": provider([scanner_provider, "--cli", str(scanner), "--socket", str(state / "unused-scanner.sock")],
                             {"version": 1, "mode": "block", "tools": {"bash": {"language": "bash", "input_pointer": "/command"}}}),
            "tokenless": provider([tokenless, "--cli", str(cli_wrapper), "--data-dir", str(state / "tokenless")], {"version": 1}),
            "record": {"transport": {"type": "stdio", "location": "agent", "argv": [python, str(fixture / "policy.py"), str(state), "normal"]},
                       "timeout_ms": 2000, "max_output_bytes": 32},
        },
        "events": {
            "tool.before": {"enabled": True, "required": True, "steps": [
                {"id": "scan", "provider": "scan", "operation": "scan_code", "effects": ["observe", "block"], "on_error": "block"}]},
            "tool.after": {"enabled": True, "required": True, "steps": [
                {"id": "record", "provider": "record", "effects": ["observe"], "on_error": "report"},
                {"id": "compress", "provider": "tokenless", "operation": "post_tool", "effects": ["replace_result"], "on_error": "report"}]},
        }}}
    if case != "json-selected-output":
        document["spec"]["events"]["tool.before"]["steps"].append(
            {"id": "rewrite", "provider": "tokenless", "operation": "pre_tool", "effects": ["block"], "on_error": "report"})
    config = state / "aw.json"
    config.write_text(json.dumps(document))
    env = {**os.environ, "PI_TEST_ROOT": str(state), "PI_METADATA": "1", "TOKENLESS_STATS_ENABLED": "0", "TOKENLESS_SLS_ENABLED": "0",
           "PATH": os.pathsep.join([str(bin_dir), "/usr/bin", "/bin"])}
    if case == "json-selected-output":
        env["PI_PRETTY_JSON"] = "1"
    else:
        env.update(PI_TOOL_NAME="bash", PI_COMMAND="git status", PI_TOKENLESS_RETRY="1")
        if case == "rtk-retry-rescanned-denied":
            env["PI_SCAN_RETRY_DENY"] = "1"
        else:
            env["PI_PRETTY_JSON"] = "1"
    try:
        code, out, err = call([aw, "run", "--config", str(config), "--agent", "pi"], env=env)
        (state / "run.stdout").write_text(out)
        (state / "run.stderr").write_text(err)
        assert code == 0, (case, code, err)
        result = json.loads(out)
        tool_results = [message for message in result["messages"] if message["role"] == "toolResult"]
        if case == "json-selected-output":
            output = tool_results[0]["content"][0]["text"]
            assert len(output) < len(result["prettyJson"]), (len(output), len(result["prettyJson"]), err)
            wires = [json.loads(line) for line in (state / "tokenless-wire.jsonl").read_text().splitlines()]
            selected = [wire for wire in wires if wire["request"]["operation"] == "post_tool"]
            assert len(selected) == 1 and selected[0]["exit_code"] == 0
            assert selected[0]["request"]["input"]["content"] == result["prettyJson"]
            assert selected[0]["response"]["result"]["disposition"] == "applied"
            assert output == selected[0]["response"]["result"]["output"]
            assert result["modelToolResults"][-1]["content"][0]["text"] == output
            assert tool_results[0]["isError"] is False
            assert tool_results[0]["details"] == {"native": True, "id": "call-0"}
            assert tool_results[0]["usage"]["totalTokens"] == 0
        else:
            assert len(tool_results) == 2
            assert tool_results[0]["isError"] is True
            guidance = tool_results[0]["content"][0]["text"]
            prefix = "[tokenless:rewrite] Retry this tool with arguments: "
            assert guidance.startswith(prefix), (case, guidance, err)
            retry = json.loads(guidance[len(prefix):])["command"]
            assert str(rtk) in retry and "TOKENLESS_AGENT_ID=pi" in retry
            scanned = [json.loads(line) for line in (state / "scanned.jsonl").read_text().splitlines()]
            assert scanned == ["git status", retry], scanned
            if case == "rtk-retry-rescanned-denied":
                assert not result["sideEffects"] and not result["executedCommands"]
                assert tool_results[1]["isError"] is True
            else:
                assert result["executedCommands"] == [retry]
                assert tool_results[1]["isError"] is False
                assert tool_results[1]["content"][0]["text"] == result["prettyJson"]
                assert result["modelToolResults"][-1]["content"][0]["text"] == result["prettyJson"]
                wires = [json.loads(line) for line in (state / "tokenless-wire.jsonl").read_text().splitlines()]
                selected = [wire for wire in wires if wire["request"]["operation"] == "post_tool"]
                assert len(selected) == 1 and selected[0]["exit_code"] == 0
                assert selected[0]["request"]["input"]["output_optimization"] == "rtk"
                assert selected[0]["request"]["input"]["content"] == result["prettyJson"]
                assert selected[0]["response"]["result"]["disposition"] == "passthrough"
                assert selected[0]["response"]["result"]["output"] == result["prettyJson"]
                assert result["afterCalls"] == 1
        code, out, err = call([aw, "status", "--config", str(config)])
        assert code == 0, err
        status = json.loads(out)["result"]
        assert status["bindings"] == 0 and status["events"] == 0
        audits = []
        for chain in (state / "state/journal").glob("*.jsonl"):
            code, out, err = call([aw, "request", "--socket", str(state / "state/aw.sock")],
                                  input=json.dumps({"method": "audit", "key": chain.stem}))
            assert code == 0, err
            audit = json.loads(out)["result"]
            assert audit["terminal"] is True
            audits.append(audit)
        assert audits
        (state / "verified-audits.json").write_text(json.dumps(audits, indent=2))
        results.append({"case": case, "passed": True, "artifacts": str(state)})
    finally:
        code, out, err = call([aw, "stop", "--config", str(config)])
        assert code == 0, err
        deadline = time.monotonic() + 5
        while (state / "state/aw.sock").exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        assert not (state / "state/aw.sock").exists()
(root / "tokenless-smoke-results.json").write_text(json.dumps(results, indent=2))
print(json.dumps(results, indent=2))
