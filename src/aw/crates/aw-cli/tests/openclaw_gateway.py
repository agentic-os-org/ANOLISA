#!/usr/bin/env python3
"""Run an owned official Gateway with synthetic hooks or local-model Agent turns.

Native-start mode additionally verifies policy updates between completed turns.
No cloud credentials, existing Gateway or messaging consumer is used.
"""
import argparse
import json
import math
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import threading
import uuid


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--node", type=Path, default=Path(shutil.which("node") or "node"))
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--model-turn", action="store_true")
    parser.add_argument("--compile-cache", action="store_true")
    parser.add_argument("--native-start", action="store_true")
    parser.add_argument("--final-gate-order-probe", action="store_true")
    parser.add_argument("--final-gate-enforce", action="store_true")
    parser.add_argument("--final-gate-provider-error", action="store_true")
    parser.add_argument("--sec-core-provider", type=Path)
    parser.add_argument("--benchmark-turns", type=int, default=0)
    args = parser.parse_args()
    if (args.native_start or args.final_gate_order_probe) and not args.model_turn:
        parser.error("--native-start and --final-gate-order-probe require --model-turn")
    if args.final_gate_enforce and not args.final_gate_order_probe:
        parser.error("--final-gate-enforce requires --final-gate-order-probe")
    if args.final_gate_provider_error and not args.final_gate_enforce:
        parser.error("--final-gate-provider-error requires --final-gate-enforce")
    if args.sec_core_provider and (not args.final_gate_enforce or args.final_gate_provider_error):
        parser.error("--sec-core-provider requires --final-gate-enforce without Provider error")
    if not 0 <= args.benchmark_turns <= 24:
        parser.error("--benchmark-turns must be 0..24")
    args.output.mkdir(parents=True, exist_ok=True)
    runtime = Path(tempfile.mkdtemp(prefix="aw-oc-gateway-"))
    records = []
    children = []
    result = {"status": "started", "runtime": str(runtime), "model_calls": 0}
    ports = []
    model_server = None
    model_thread = None

    def write(path: Path, value: object) -> None:
        path.write_text(json.dumps(value, indent=2) + "\n")

    def start(name: str, command: list[object], environment: dict[str, str]) -> subprocess.Popen[bytes]:
        log = args.output / f"{name}.log"
        record = {"name": name, "command": list(map(str, command)), "cwd": str(runtime),
                  "ports": ports, "log": str(log), "timeout_seconds": 90, "pid": None,
                  "stop": None}
        records.append(record)
        write(args.output / "processes.json", records)
        with log.open("wb") as output:
            child = subprocess.Popen(record["command"], cwd=runtime, env=environment,
                                     stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT,
                                     start_new_session=True)
        record.update(pid=child.pid, stop=f"kill -TERM -- -{child.pid}")
        children.append((child, record))
        write(args.output / "processes.json", records)
        return child

    def stop(child: subprocess.Popen[bytes]) -> None:
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGTERM)
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=5)

    def interrupted(_signal: int, _frame: object) -> None:
        raise TimeoutError("official Gateway fixture deadline or interruption")

    for value in (signal.SIGALRM, signal.SIGTERM, signal.SIGINT):
        signal.signal(value, interrupted)
    signal.alarm(90)
    try:
        profile, workspace, state = runtime / "profile", runtime / "workspace", runtime / "aw-state"
        existing = runtime / "existing-plugin"
        for path in (profile, workspace, state, existing, runtime / "home"):
            path.mkdir(mode=0o700)
        (profile / "credential-marker").write_text("fixture-persistent-state")
        (workspace / "AGENTS.md").write_text("Isolated no-model AW acceptance fixture.\n")
        package = json.loads((args.package / "package.json").read_text())
        assert package["version"] == "2026.9.6"
        runner_file = next(path for path in (args.package / "dist").glob("hook-runner-global-*.mjs")
                           if "getGlobalHookRunner as t" in path.read_text())
        write(existing / "package.json", {"name": "aw-existing-fixture", "version": "0.0.1",
              "private": True, "type": "module", "openclaw": {"extensions": ["./index.mjs"]}})
        manifest = {"id": "aw-existing-fixture", "activation": {"onStartup": True},
                    "configSchema": {"type": "object", "additionalProperties": False, "properties": {}}}
        if args.final_gate_order_probe:
            manifest["contracts"] = {"agentToolResultMiddleware": ["openclaw"]}
        write(existing / "openclaw.plugin.json", manifest)
        (existing / "index.mjs").write_text("""import { appendFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { t as getRunner } from RUNNER;
const root = ROOT;
export default { id:'aw-existing-fixture', register(api) {
  const trace = row => appendFileSync(join(root,'existing.jsonl'),JSON.stringify(row)+'\\n');
  for (const hook of ['before_tool_call','after_tool_call']) api.on(hook,(event)=>{trace({hook,call:event.toolCallId,run:event.runId});},{priority:200});
  api.on('gateway_start',async()=>{
    try {
      await new Promise(resolve=>setTimeout(resolve,150));
      const rows=[]; const runner=getRunner();
      for(const command of ['ALLOW','DENY']) {
        const event={toolName:'exec',params:{command},runId:command,toolCallId:'same-call'};
        const context={sessionId:'fixture-session',runId:command,toolCallId:'same-call'};
        const before=await runner.runBeforeToolCall(event,context);
        if(!before?.block) {
          writeFileSync(join(root,'tool-'+command),'fixture invocation adopted');
          await runner.runAfterToolCall({...event,result:{content:['fixture result']}},context);
        }
        rows.push({command,blocked:before?.block===true});
      }
      writeFileSync(join(root,'proof.json'),JSON.stringify({status:'passed',rows}));
    } catch(error) { writeFileSync(join(root,'proof.json'),JSON.stringify({status:'failed',error:String(error)})); }
  });
}};
""".replace("RUNNER", json.dumps(runner_file.as_uri())).replace("ROOT", json.dumps(str(runtime))))
        if args.model_turn:
            source = """import { appendFileSync } from 'node:fs';
const root=ROOT;
export default { id:'aw-existing-fixture',register(api){
  for(const hook of ['gateway_start','before_tool_call','after_tool_call']) api.on(hook,event=>{
    appendFileSync(root+'/existing.jsonl',JSON.stringify({hook,call:event.toolCallId,run:event.runId})+'\\n');
  },{priority:200});
  MIDDLEWARE
}};
"""
            middleware = """api.registerAgentToolResultMiddleware(async (event,context)=>{
    appendFileSync(root+'/middleware.jsonl',JSON.stringify({stage:'early',tool:event.toolName,call:event.toolCallId,context,result:event.result})+'\\n');
    return undefined;
  },{runtimes:['openclaw']});
  api.registerAgentToolResultMiddleware(async event=>{
    appendFileSync(root+'/middleware.jsonl',JSON.stringify({stage:'late',tool:event.toolName,result:event.result})+'\\n');
    if (!event.args?.command?.includes('AW_ALLOW')) return undefined;
    return {result:{...event.result,content:[...event.result.content,
      {type:'text',text:'AW_FINAL_GATE_SECRET_MARKER'}]}};
  },{runtimes:['openclaw']});""" if args.final_gate_order_probe else ""
            (existing / "index.mjs").write_text(source.replace("ROOT",json.dumps(str(runtime)))
                                               .replace("MIDDLEWARE",middleware))
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        ports.append(port)
        native = {"logging": {"file": str(args.output / "runtime.log"), "level": "info"},
                  "agents": {"defaults": {"workspace": str(workspace), "skipBootstrap": True}},
                  "gateway": {"mode": "local", "bind": "loopback", "port": port,
                              "auth": {"mode": "token", "token": uuid.uuid4().hex}, "controlUi": {"enabled": False}},
                  "skills": {"load": {"watch": False}}, "cron": {"enabled": False},
                  "plugins": {"allow": ["aw-existing-fixture"], "load": {"paths": [str(existing)]},
                              "entries": {"aw-existing-fixture": {"enabled": True}}}}
        if args.model_turn:
            class ModelFixture(BaseHTTPRequestHandler):
                def log_message(self, _format: str, *unused: object) -> None:
                    pass

                def do_POST(self) -> None:
                    request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                    result["model_calls"] += 1
                    messages = request["messages"]
                    if args.final_gate_order_probe:
                        with (runtime / "model-results.jsonl").open("a") as stream:
                            stream.write(json.dumps([message for message in messages
                                                     if message.get("role") == "tool"]) + "\n")
                    has_result = any(message.get("role") == "tool" for message in messages)
                    user_text = json.dumps([message for message in messages if message.get("role") == "user"])
                    tag = "DENY" if "DENY" in user_text else "SAFE" if "SAFE" in user_text else "ALLOW"
                    marker = workspace / ("denied.marker" if tag == "DENY" else
                                          "safe.marker" if tag == "SAFE" else "allowed.marker")
                    command = "printf AW_" + tag + " > " + str(marker)
                    delta = {"role":"assistant","content":"Fixture turn completed."} if has_result else {
                        "role":"assistant","tool_calls":[{"index":0,"id":"fixture-call","type":"function",
                        "function":{"name":"exec","arguments":json.dumps({"command":command})}}]}
                    finish = "stop" if has_result else "tool_calls"
                    self.send_response(200)
                    if request.get("stream"):
                        self.send_header("Content-Type","text/event-stream")
                        self.end_headers()
                        for content, reason in [(delta,None),({},finish)]:
                            chunk = {"id":"fixture-completion","object":"chat.completion.chunk","created":1,"model":"fixture-model",
                                     "choices":[{"index":0,"delta":content,"finish_reason":reason}]}
                            self.wfile.write(("data: "+json.dumps(chunk)+"\n\n").encode())
                        self.wfile.write(b"data: [DONE]\n\n")
                    else:
                        self.send_header("Content-Type","application/json")
                        self.end_headers()
                        self.wfile.write(json.dumps({"id":"fixture-completion","object":"chat.completion","created":1,"model":"fixture-model",
                            "choices":[{"index":0,"message":delta,"finish_reason":finish}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}).encode())

            model_server = ThreadingHTTPServer(("127.0.0.1",0),ModelFixture)
            model_thread = threading.Thread(target=model_server.serve_forever,daemon=True)
            model_thread.start()
            ports.append(model_server.server_port)
            native["models"] = {"mode":"merge","providers":{"fixture":{"baseUrl":f"http://127.0.0.1:{model_server.server_port}/v1",
                "apiKey":"local-fixture-only","api":"openai-completions","models":[{"id":"fixture-model","name":"fixture-model",
                "reasoning":False,"input":["text"],"contextWindow":131072,"maxTokens":512}]}}}
            native["agents"]["defaults"]["model"] = {"primary":"fixture/fixture-model"}
            native["tools"] = {"allow":["exec"],"exec":{"mode":"full"}}
        native_file = runtime / "native.json"
        write(native_file, native)
        action = Path(__file__).resolve().parent / "fixtures/openclaw/action.py"
        config = {"apiVersion": "aw/v1alpha1", "kind": "AWConfiguration", "metadata": {"name": "openclaw-fixture"},
                  "spec": {"daemon": {"startup": "external", "endpoint": "auto", "state_dir": str(state)},
                           "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
                           "audit": {"enabled": True, "payload": "metadata_only"},
                           "agents": {"openclaw": {"adapter": "openclaw", "argv": [str(args.node),str(args.package / "openclaw.mjs"),"gateway","run","--bind","loopback","--port",str(port),"--tailscale","off"]}},
                           "providers": {}, "events": {}}}
        for event, label, structured, behavior in [("tool.before","policy",True,"observe"),
                ("tool.before","before-one",False,"observe"),("tool.before","before-two",False,"observe"),
                ("tool.after","after-one",False,"barrier"),("tool.after","after-two",False,"barrier")]:
            config["spec"]["providers"][label] = {"protocol": "aw-provider/v1alpha1" if structured else "native-hook/v1alpha1",
                "transport": {"type": "stdio", "location": "agent", "argv": ["/usr/bin/python3",str(action),"--root",str(runtime),"--label",label,"--behavior",behavior]},
                "timeout_ms": 4500, "max_output_bytes": 65536, "config": {}}
            step = {"id": label,"provider": label,"on_error": "block" if event == "tool.before" else "report"}
            step.update({"operation": "check", "effects": ["observe","block"]} if structured else {"native": {}})
            config["spec"]["events"].setdefault(event,{"enabled":True,"steps":[]})["steps"].append(step)
        if args.final_gate_enforce:
            label = "final-check"
            if args.sec_core_provider:
                scanner = runtime / "scanner"
                scanner.write_text(f"""#!{sys.executable}
import json, pathlib, sys
text = sys.stdin.read()
with (pathlib.Path(__file__).parent / 'pii-scanned.jsonl').open('a') as stream:
    stream.write(json.dumps(text) + '\\n')
print(json.dumps({{'ok': True,
    'verdict': 'deny' if 'AW_FINAL_GATE_SECRET_MARKER' in text else 'pass',
    'summary': {{'source': 'tool_output', 'truncated': False,
                'bytes_scanned': len(text.encode())}}}}))
""")
                scanner.chmod(0o700)
                provider_command = [str(args.sec_core_provider), "--cli", str(scanner),
                                    "--socket", str(runtime / "scanner.sock")]
                provider_config = {"version": 1, "mode": "block",
                                   "pii": {"input_mode": "block", "output_mode": "observe"}}
                operation = "check_final_tool_output"
            else:
                provider_command = ["/usr/bin/python3", str(action), "--root", str(runtime),
                                    "--label", label, "--behavior",
                                    "final-error" if args.final_gate_provider_error else "observe"]
                provider_config = {}
                operation = "check-final"
            config["spec"]["providers"][label] = {"protocol": "aw-provider/v1alpha1",
                "transport": {"type": "stdio", "location": "agent", "argv": provider_command},
                "timeout_ms": 4500, "max_output_bytes": 65536, "config": provider_config}
            config["spec"]["events"]["security.violation"] = {"enabled": True, "steps": [
                {"id": label, "provider": label, "on_error": "block", "operation": operation,
                 "effects": ["observe", "block"]}]}
        config_file = runtime / "aw.json"
        write(config_file,config)
        environment = {"PATH": str(args.node.parent)+":/usr/bin:/bin", "HOME": str(runtime / "home"),
                       "OPENCLAW_HOME": str(runtime / "home"), "OPENCLAW_STATE_DIR": str(profile),
                       "OPENCLAW_CONFIG_PATH": str(native_file), "OPENCLAW_SKIP_CHANNELS": "1"}
        if args.final_gate_order_probe:
            environment["AW_FINAL_GATE_ORDER_PROBE"] = "1"
        if args.compile_cache:
            environment["NODE_COMPILE_CACHE"] = str(runtime / "compile-cache")
        daemon = start("daemon",[args.aw,"serve","--config",config_file,"--state-dir",state],environment)
        deadline = time.monotonic()+10
        while not (state / "aw.sock").exists():
            assert daemon.poll() is None and time.monotonic()<deadline,"fixture daemon did not become ready"
            time.sleep(0.05)
        if args.native_start:
            prepared = subprocess.run([str(args.aw), "instance", "setup", "--config", str(config_file),
                "--agent", "openclaw", "--directory", str(runtime / "startup"),
                "--native-settings", str(native_file), "--native-state-dir", str(profile)],
                cwd=runtime, env=environment, capture_output=True, timeout=15, check=True)
            startup_environment = {**environment, **json.loads(prepared.stdout)["environment"]}
            gateway = start("gateway", config["spec"]["agents"]["openclaw"]["argv"], startup_environment)
        else:
            gateway = start("gateway",[args.aw,"run","--config",config_file,"--agent","openclaw",
                             "--native-settings",native_file,"--native-state-dir",profile],environment)
        deadline = time.monotonic()+50
        def ready() -> bool:
            if args.native_start:
                return any(state.glob("native-*/ready.json"))
            return "AW openclaw native hooks ready" in (args.output / "gateway.log").read_text()
        while not ready() or (not args.model_turn and not (runtime / "proof.json").exists()):
            assert gateway.poll() is None and time.monotonic()<deadline,"fixture Gateway did not become ready and produce a proof"
            time.sleep(0.1)
        if args.model_turn:
            turn_ms = []
            labels = (("ALLOW", "SAFE", "DENY") if args.final_gate_enforce else ("ALLOW", "DENY"))
            labels += ("SAFE",) * args.benchmark_turns
            for label in labels:
                started = time.monotonic()
                client = start("agent-"+label.lower(),[args.node,args.package / "openclaw.mjs","agent","--session-id",str(uuid.uuid4()),
                    "--thinking","off","--timeout","25","--json","--message",label],environment)
                assert client.wait(timeout=35)==0,"local fixture Agent turn failed"
                turn_ms.append(round((time.monotonic() - started) * 1000, 3))
                if args.native_start:
                    config["spec"]["providers"]["policy"]["timeout_ms"] -= 10
                    write(config_file, config)
                    update = subprocess.run([str(args.aw), "instance", "update", "--config", str(config_file),
                        "--agent", "openclaw"], cwd=runtime, env=environment,
                        capture_output=True, timeout=15, check=False)
                    assert update.returncode == 0, update.stderr.decode()
            deadline = time.monotonic()+8
            while not (runtime / "barrier-after-two").exists() and time.monotonic()<deadline:
                time.sleep(0.05)
            provider_inputs = [json.loads(path.read_text()) for path in runtime.glob("provider-*.json")]
            invocations = [item for item in provider_inputs if isinstance(item,dict) and item.get("method")=="invoke"]
            commands = [item["event"]["tool"]["input"].get("command") for item in invocations]
            assert any("printf AW_ALLOW" in command for command in commands) and any("printf AW_DENY" in command for command in commands),commands
            assert (workspace / "allowed.marker").read_text()=="AW_ALLOW"
            if args.final_gate_enforce:
                assert (workspace / "safe.marker").read_text()=="AW_SAFE"
            assert not (workspace / "denied.marker").exists()
            native_rows = [json.loads(line) for line in (runtime / "raw-calls.jsonl").read_text().splitlines()]
            before_count = (4 if args.final_gate_enforce else 2) + 2 * args.benchmark_turns
            after_count = (6 if args.final_gate_enforce else 4) + 2 * args.benchmark_turns
            assert len([row for row in native_rows if row["hook"]=="before_tool_call"])==before_count,native_rows
            assert len([row for row in native_rows if row["hook"]=="after_tool_call"])==after_count,native_rows
            proof = {"status":"passed","kind":"official Agent turns with a local model fixture","commands":commands,
                     "cloud_model_calls":0,"native_after_calls":after_count,"native_before_calls":before_count,"idle_policy_updates":2 if args.native_start else 0,
                     "allow_marker_present":True,"deny_marker_absent":True}
            if args.benchmark_turns:
                samples = sorted(turn_ms[-args.benchmark_turns:])
                percentile = lambda p: samples[math.ceil(len(samples) * p) - 1]
                write(args.output / "performance.json", {"scope":"separate local Agent client turn",
                    "gate": args.final_gate_enforce, "samples":len(samples),
                    "p50_ms":percentile(.50),"p95_ms":percentile(.95),"p99_ms":percentile(.99),
                    "all_ms":samples})
            if args.final_gate_order_probe:
                marker = "AW_FINAL_GATE_SECRET_MARKER"
                post = [row["result"] for row in native_rows if row["hook"] == "after_tool_call"]
                middleware = [json.loads(line) for line in (runtime / "middleware.jsonl").read_text().splitlines()]
                received = [json.loads(line) for line in (runtime / "model-results.jsonl").read_text().splitlines()]
                early = [row for row in middleware if row["stage"] == "early"]
                late = [row for row in middleware if row["stage"] == "late"]
                assert early and len(early) == len(late)
                assert all(marker not in json.dumps(row["result"]) for row in early + late)
                assert marker in json.dumps(post), "Post Hook did not observe the middleware result"
                if args.sec_core_provider:
                    final_rows = [json.loads(line) for line in
                                  (runtime / "pii-scanned.jsonl").read_text().splitlines()]
                    model_checked = final_rows
                else:
                    final_rows = [item["event"]["tool"]["result"] for item in invocations
                                  if item["event"]["name"] == "security.violation"]
                    model_checked = [item["event"]["tool"]["result"] for item in invocations
                                     if item["event"]["name"] == "security.violation" and
                                     item["event"]["native"]["event"].get("phase") == "model"]
                if args.final_gate_enforce:
                    assert marker in json.dumps(final_rows), "final gate did not inspect the mutated result"
                    assert marker not in json.dumps(received), "mutated result reached the model"
                    assert len(model_checked) >= 2, "model projection was not checked"
                    assert marker in json.dumps(model_checked), "model projection lost the injected result"
                    if args.final_gate_provider_error:
                        assert all("AW withheld" in json.dumps(row) for row in received if row), \
                            "Provider failure released a tool result"
                    else:
                        assert any(message.get("content") == "(no output)" for row in received
                                   for message in row), "safe tool result was withheld"
                else:
                    assert marker in json.dumps(received), "official Agent did not adopt the later middleware mutation"
                status = "final_gate_verified" if args.final_gate_enforce else "baseline_red"
                final_probe = {"status": status, "agent": "openclaw", "version": package["version"],
                               "raw": "AW_ALLOW", "early_middleware": early, "post_hook": post,
                               "native_final": marker,
                               "checked": final_rows if args.final_gate_enforce else None,
                               "model_checked": model_checked if args.final_gate_enforce else None,
                               "model_received": received,
                               "finding": "a middleware check can be followed by another middleware rewrite"}
                write(runtime / "final-gate-baseline.json", final_probe)
                proof["final_gate_order_probe"] = status
                proof["final_gate_provider_error"] = args.final_gate_provider_error
                proof["final_gate_sec_core"] = bool(args.sec_core_provider)
            write(runtime / "proof.json",proof)
        else:
            proof = json.loads((runtime / "proof.json").read_text())
            assert proof["status"] == "passed",proof
            assert proof["rows"] == [{"command":"ALLOW","blocked":False},{"command":"DENY","blocked":True}],proof
            assert (runtime / "tool-ALLOW").exists() and not (runtime / "tool-DENY").exists()
        assert native == json.loads(native_file.read_text())
        assert (profile / "credential-marker").read_text() == "fixture-persistent-state"
        assert (runtime / "barrier-after-one").exists() and (runtime / "barrier-after-two").exists()
        stop(gateway)
        status = start("status",[args.aw,"status","--config",config_file],environment)
        assert status.wait(timeout=8)==0
        state_report = json.loads((args.output / "status.log").read_text())["result"]
        assert state_report["bindings"] == 0 and state_report["events"] == 0,state_report
        assert not list(state.glob("launch-*"))
        stop(daemon)
        assert not (state / "aw.sock").exists()
        with socket.socket() as probe:
            assert probe.connect_ex(("127.0.0.1",port))!=0,"owned Gateway port remains open"
        for name in ("proof.json","existing.jsonl","raw-calls.jsonl","middleware.jsonl",
                     "model-results.jsonl","final-gate-baseline.json", "pii-scanned.jsonl"):
            if not (runtime / name).exists():
                continue
            shutil.copyfile(runtime / name,args.output / name)
        result.update(status="passed",proof=proof,native_start=args.native_start,original_config_unchanged=True,
                      persistent_profile_preserved=True,bindings_after_exit=0,port_closed=True)
    finally:
        for child, record in reversed(children):
            stop(child)
            record.update(exit_code=child.poll(),exited=not Path(f"/proc/{child.pid}").exists())
        if model_server:
            model_server.shutdown()
            model_server.server_close()
            model_thread.join(timeout=3)
        write(args.output / "processes.json",records)
        for name in ("proof.json", "raw-calls.jsonl", "model-results.jsonl", "middleware.jsonl",
                     "pii-scanned.jsonl"):
            if (runtime / name).exists():
                shutil.copyfile(runtime / name, args.output / name)
        if (runtime / "aw-state/journal").exists():
            shutil.copytree(runtime / "aw-state/journal", args.output / "journal", dirs_exist_ok=True)
        for path in runtime.glob("provider-*.json"):
            shutil.copyfile(path,args.output / path.name)
        shutil.rmtree(runtime)
        result["runtime_removed"] = not runtime.exists()
        write(args.output / "result.json",result)
        signal.alarm(0)
    print(json.dumps(result))


if __name__ == "__main__":
    main()
