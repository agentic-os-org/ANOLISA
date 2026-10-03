#!/usr/bin/env python3
"""Bound an official Gateway registration and synthetic native dispatch probe.

No model, cloud credentials, existing Gateway or messaging consumer is used.
The native runner receives fixture tool events; this is not a model-turn test.
"""
import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import threading
import uuid


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--node", type=Path, required=True)
    parser.add_argument("--aw", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--model-turn", action="store_true")
    args = parser.parse_args()
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
        write(existing / "openclaw.plugin.json", {"id": "aw-existing-fixture", "activation": {"onStartup": True},
              "configSchema": {"type": "object", "additionalProperties": False, "properties": {}}})
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
            (existing / "index.mjs").write_text("""import { appendFileSync } from 'node:fs';
const root=ROOT;
export default { id:'aw-existing-fixture',register(api){
  for(const hook of ['gateway_start','before_tool_call','after_tool_call']) api.on(hook,event=>{
    appendFileSync(root+'/existing.jsonl',JSON.stringify({hook,call:event.toolCallId,run:event.runId})+'\\n');
  },{priority:200});
}};
""".replace("ROOT",json.dumps(str(runtime))))
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
                    has_result = any(message.get("role") == "tool" for message in messages)
                    denied = "DENY" in json.dumps([message for message in messages if message.get("role") == "user"])
                    marker = workspace / ("denied.marker" if denied else "allowed.marker")
                    command = ("printf AW_DENY" if denied else "printf AW_ALLOW") + " > " + str(marker)
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
        config_file = runtime / "aw.json"
        write(config_file,config)
        environment = {"PATH": str(args.node.parent)+":/usr/bin:/bin", "HOME": str(runtime / "home"),
                       "OPENCLAW_HOME": str(runtime / "home"), "OPENCLAW_STATE_DIR": str(profile),
                       "OPENCLAW_CONFIG_PATH": str(native_file), "OPENCLAW_SKIP_CHANNELS": "1"}
        daemon = start("daemon",[args.aw,"serve","--config",config_file,"--state-dir",state],environment)
        deadline = time.monotonic()+10
        while not (state / "aw.sock").exists():
            assert daemon.poll() is None and time.monotonic()<deadline,"fixture daemon did not become ready"
            time.sleep(0.05)
        gateway = start("gateway",[args.aw,"run","--config",config_file,"--agent","openclaw",
                         "--native-settings",native_file,"--native-state-dir",profile],environment)
        deadline = time.monotonic()+50
        while not (runtime / "proof.json").exists() and not (args.model_turn and "AW openclaw native hooks ready" in (args.output / "gateway.log").read_text()):
            assert gateway.poll() is None and time.monotonic()<deadline,"fixture Gateway did not produce a proof"
            time.sleep(0.1)
        if args.model_turn:
            for label in ("ALLOW","DENY"):
                client = start("agent-"+label.lower(),[args.node,args.package / "openclaw.mjs","agent","--session-id",str(uuid.uuid4()),
                    "--thinking","off","--timeout","25","--json","--message",label],environment)
                assert client.wait(timeout=35)==0,"local fixture Agent turn failed"
            deadline = time.monotonic()+8
            while not (runtime / "barrier-after-two").exists() and time.monotonic()<deadline:
                time.sleep(0.05)
            provider_inputs = [json.loads(path.read_text()) for path in runtime.glob("provider-*.json")]
            invocations = [item for item in provider_inputs if isinstance(item,dict) and item.get("method")=="invoke"]
            commands = [item["event"]["tool"]["input"].get("command") for item in invocations]
            assert any("printf AW_ALLOW" in command for command in commands) and any("printf AW_DENY" in command for command in commands),commands
            assert (workspace / "allowed.marker").read_text()=="AW_ALLOW"
            assert not (workspace / "denied.marker").exists()
            native_rows = [json.loads(line) for line in (runtime / "raw-calls.jsonl").read_text().splitlines()]
            assert len([row for row in native_rows if row["hook"]=="before_tool_call"])==2,native_rows
            assert len([row for row in native_rows if row["hook"]=="after_tool_call"])==4,native_rows
            proof = {"status":"passed","kind":"official Agent turns with a local model fixture","commands":commands,
                     "cloud_model_calls":0,"native_after_calls":4,"native_before_calls":2,
                     "allow_marker_present":True,"deny_marker_absent":True}
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
        for name in ("proof.json","existing.jsonl","raw-calls.jsonl"):
            shutil.copyfile(runtime / name,args.output / name)
        result.update(status="passed",proof=proof,original_config_unchanged=True,
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
        for name in ("proof.json", "raw-calls.jsonl"):
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
