#!/usr/bin/env python3
"""Optional real-Gateway regression using a local scripted model and the built adapter."""

import argparse
import csv
import hashlib
import io
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

COMPONENT = Path(__file__).resolve().parents[1]


def scripted_response(body: dict[str, Any]) -> bytes:
    # Some hosts append runtime context after the tool-result message.
    results = tool_results(body)
    if results:
        block = {"type": "text", "text": "GATEWAY_PROBE_COMPLETE"}
        reason = "end_turn"
        initial = {"type": "text", "text": ""}
        delta = {"type": "text_delta", "text": block["text"]}
    else:
        block = {
            "type": "tool_use",
            "id": "toolu_gateway_probe",
            "name": "exec",
            "input": {"command": "python3 /workspace/export.py"},
        }
        reason = "tool_use"
        initial = {**block, "input": {}}
        delta = {"type": "input_json_delta", "partial_json": json.dumps(block["input"])}
    events = [
        (
            "message_start",
            {
                "type": "message_start",
                "message": {
                    "id": "msg_gateway_probe",
                    "type": "message",
                    "role": "assistant",
                    "model": "gateway-probe",
                    "content": [],
                    "stop_reason": None,
                    "stop_sequence": None,
                    "usage": {"input_tokens": 1, "output_tokens": 0},
                },
            },
        ),
        (
            "content_block_start",
            {"type": "content_block_start", "index": 0, "content_block": initial},
        ),
        ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": delta}),
        ("content_block_stop", {"type": "content_block_stop", "index": 0}),
        (
            "message_delta",
            {
                "type": "message_delta",
                "delta": {"stop_reason": reason},
                "usage": {"output_tokens": 1},
            },
        ),
        ("message_stop", {"type": "message_stop"}),
    ]
    return "".join(
        f"event: {name}\ndata: {json.dumps(event)}\n\n" for name, event in events
    ).encode()


def tool_results(body: dict[str, Any]) -> list[str]:
    return [
        text_content(block["content"])
        for message in body["messages"]
        if isinstance(message.get("content"), list)
        for block in message["content"]
        if block.get("type") == "tool_result"
    ]


def text_content(content: Any) -> str:
    return (
        content
        if isinstance(content, str)
        else "\n".join(block["text"] for block in content if block.get("type") == "text")
    )


def persisted_results(database: Path, image: str) -> list[str]:
    with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
        columns = {row[1] for row in connection.execute("PRAGMA table_info(transcript_events)")}
        packed_column = "event_zstd" if "event_zstd" in columns else "NULL"
        rows = list(
            connection.execute(f"SELECT event_json, {packed_column} FROM transcript_events")
        )
    packed = [blob.hex() for raw, blob in rows if raw is None]
    # Reuse the tested host's Node zstd support instead of requiring Python 3.14.
    decoded = (
        iter(
            json.loads(
                subprocess.check_output(
                    [
                        "docker",
                        "run",
                        "--rm",
                        "-i",
                        "--network",
                        "none",
                        "--cap-drop",
                        "ALL",
                        "--entrypoint",
                        "node",
                        image,
                        "-e",
                        "const fs=require('node:fs'),z=require('node:zlib');"
                        "process.stdout.write(JSON.stringify(JSON.parse(fs.readFileSync(0,'utf8'))"
                        ".map(x=>z.zstdDecompressSync(Buffer.from(x,'hex')).toString('utf8'))));",
                    ],
                    input=json.dumps(packed),
                    text=True,
                    timeout=30,
                )
            )
        )
        if packed
        else iter([])
    )
    messages = [
        json.loads(raw if raw is not None else next(decoded)).get("message", {}) for raw, _ in rows
    ]
    return [
        text_content(message["content"])
        for message in messages
        if message.get("role") == "toolResult"
    ]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="tokenless-test-agent-openclaw:2026.8.2")
    parser.add_argument(
        "--host-output-policy",
        choices=["legacy", "normalized"],
        required=True,
        help="Expected native middleware behavior; select from source/build provenance, not semver",
    )
    parser.add_argument("--output", type=Path)
    parser.add_argument("--binary", type=Path, default=COMPONENT / "target/release/tokenless")
    args = parser.parse_args()
    output = args.output or Path(tempfile.mkdtemp(prefix="tokenless-openclaw-gateway."))
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    image_id = subprocess.check_output(
        ["docker", "image", "inspect", args.image, "--format", "{{.Id}}"], text=True
    ).strip()
    runtime = output / "runtime"
    runtime.mkdir()
    shutil.copytree(
        COMPONENT / "adapters/tokenless/openclaw",
        runtime / "plugin",
        ignore=shutil.ignore_patterns("node_modules", "*.ts", "package-lock.json"),
    )
    shutil.copy2(args.binary, runtime / "core")
    (runtime / "bin").mkdir()
    shim = runtime / "bin/tokenless"
    shim.write_text(
        "#!/usr/bin/python3\n"
        "import json,os,subprocess,sys\nfrom pathlib import Path\n"
        "data=sys.stdin.buffer.read()\n"
        "if os.environ.get('PROBE_CORE_FAIL')=='1':\n"
        " p=subprocess.CompletedProcess([],1,b'',b'injected Core failure')\n"
        "else:\n p=subprocess.run(['/runtime/core',*sys.argv[1:]],input=data,capture_output=True)\n"
        "with Path('/results/core.jsonl').open('a') as log:\n"
        " log.write(json.dumps({'request':json.loads(data),'stdout':p.stdout.decode(),'exit_code':p.returncode})+'\\n')\n"
        "sys.stdout.buffer.write(p.stdout)\nsys.stderr.buffer.write(p.stderr)\nsys.exit(p.returncode)\n"
    )
    shim.chmod(0o755)
    workspace = output / "workspace"
    workspace.mkdir()
    stream = io.StringIO(newline="")
    writer = csv.writer(stream, quoting=csv.QUOTE_ALL, lineterminator="\r\n")
    writer.writerow(["id", "dependency", "version"])
    writer.writerows((str(i), f"repeated_dependency_name_{i:03}", "1.2.3") for i in range(100))
    source = stream.getvalue()
    plain = "BEGIN " + "x" * (150000 - len("BEGIN TAIL_SENTINEL")) + "TAIL_SENTINEL"
    document = {
        "items": [{"id": i, "value": f"item_{i:03}"} for i in range(100)],
        "tail": "TAIL_SENTINEL",
    }
    compact = json.dumps(document)
    # Internal whitespace keeps the JSON recognizable and puts its tail beyond 100k.
    tail_offset = compact.index('"tail"')
    large_json = compact[:tail_offset] + " " * (150000 - len(compact)) + compact[tail_offset:]
    cases = {
        "off": source,
        "on": source,
        "failure": source,
        "large-off": plain,
        "large-passthrough": plain,
        "large-failure": plain,
        "large-json": large_json,
    }
    (workspace / "export.py").write_text(
        "from pathlib import Path\nimport sys\n"
        "sys.stdout.buffer.write(Path('/workspace/data.txt').read_bytes())\n"
    )
    requests: dict[str, list[dict[str, Any]]] = {}

    class ModelHandler(BaseHTTPRequestHandler):
        def log_message(self, format: str, *arguments: object) -> None:
            pass

        def do_POST(self) -> None:
            case = self.path.split("/")[1]
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.setdefault(case, []).append(body)
            if len(requests[case]) > 3:
                self.send_error(500, "Unexpected extra model request")
                return
            response = scripted_response(body)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(response)))
            self.end_headers()
            self.wfile.write(response)

    server = ThreadingHTTPServer(("127.0.0.1", 0), ModelHandler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    model_port = server.server_address[1]
    driver = output / "driver.py"
    driver.write_text(
        "import json,socket,subprocess,time\nfrom pathlib import Path\n"
        "root=Path('/results')\nsock=socket.socket(); sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]; sock.close()\n"
        "config=json.loads((root/'config.json').read_text()); config['gateway']['port']=port\n"
        "(root/'config.json').write_text(json.dumps(config))\n"
        "with (root/'gateway.log').open('w') as log:\n"
        " gateway=subprocess.Popen(['openclaw','gateway','run'],stdout=log,stderr=subprocess.STDOUT)\n"
        " try:\n"
        "  for _ in range(30):\n"
        "   health=subprocess.run(['openclaw','gateway','health','--json'],capture_output=True,timeout=12)\n"
        "   if health.returncode==0: break\n"
        "   if gateway.poll() is not None: raise RuntimeError('Gateway exited')\n"
        "   time.sleep(1)\n"
        "  else: raise RuntimeError('Gateway not healthy')\n"
        "  for phase,prompt in [('first','Export the fixture with exec.'),('followup','Answer again using the existing result, without tools.')]:\n"
        "   p=subprocess.run(['openclaw','agent','--session-key','agent:main:gateway-probe','--message',prompt,'--thinking','off','--json'],capture_output=True,timeout=60)\n"
        "   (root/(phase+'.stdout')).write_bytes(p.stdout); (root/(phase+'.stderr')).write_bytes(p.stderr)\n"
        "   if p.returncode: raise RuntimeError('Agent failed')\n"
        " finally:\n"
        "  gateway.terminate()\n"
        "  try: gateway.wait(timeout=8)\n"
        "  except subprocess.TimeoutExpired: gateway.kill(); gateway.wait()\n"
    )
    report: dict[str, Any] = {
        "image": args.image,
        "image_id": image_id,
        "cloud_requests": 0,
        "host_output_policy": args.host_output_policy,
        "pass": False,
        "cases": [],
    }
    try:
        baseline = None
        for case, source in cases.items():
            (workspace / "data.txt").write_bytes(source.encode())
            disabled = case in {"off", "large-off"}
            failing = case in {"failure", "large-failure"}
            directory = output / case
            directory.mkdir()
            config = {
                "gateway": {
                    "mode": "local",
                    "bind": "loopback",
                    "controlUi": {"enabled": False},
                    "auth": {"mode": "token", "token": "gateway-probe-local"},
                },
                "models": {
                    "providers": {
                        "probe": {
                            "baseUrl": f"http://127.0.0.1:{model_port}/{case}",
                            "apiKey": "unused-local-placeholder",
                            "api": "anthropic-messages",
                            "models": [
                                {
                                    "id": "gateway-probe",
                                    "name": "gateway-probe",
                                    "reasoning": False,
                                    "input": ["text"],
                                    "contextWindow": 1000000,
                                    "maxTokens": 1024,
                                }
                            ],
                        }
                    }
                },
                "agents": {
                    "defaults": {
                        "workspace": "/workspace",
                        "skipBootstrap": True,
                        "model": {"primary": "probe/gateway-probe"},
                        "heartbeat": {"every": "0m"},
                    }
                },
                "tools": {
                    "allow": ["exec"],
                    "exec": {"host": "gateway", "security": "full", "ask": "off"},
                },
                "plugins": {
                    "allow": ["tokenless"],
                    "load": {"paths": ["/runtime/plugin"]},
                    "entries": {
                        "tokenless": {
                            "enabled": True,
                            "config": {
                                "rtk_enabled": False,
                                "post_tool_enabled": not disabled,
                                "tool_ready_enabled": False,
                            },
                        }
                    },
                },
                "cron": {"enabled": False},
            }
            (directory / "config.json").write_text(json.dumps(config))
            name = f"{output.name}-{case}".lower()
            command = [
                "docker",
                "run",
                "--rm",
                "--name",
                name,
                "--network",
                "host",
                "--cap-drop",
                "ALL",
                "--mount",
                f"type=bind,src={runtime},dst=/runtime,readonly",
                "--mount",
                f"type=bind,src={workspace},dst=/workspace,readonly",
                "--mount",
                f"type=bind,src={driver},dst=/driver.py,readonly",
                "--mount",
                f"type=bind,src={directory},dst=/results",
                "--env",
                "PATH=/runtime/bin:/usr/local/bin:/usr/bin:/bin",
                "--env",
                "OPENCLAW_STATE_DIR=/results/state",
                "--env",
                "OPENCLAW_CONFIG_PATH=/results/config.json",
                "--env",
                "TOKENLESS_DATA_DIR=/results/tokenless",
                "--env",
                "TOKENLESS_STATS_ENABLED=1",
                "--env",
                "TOKENLESS_SLS_ENABLED=0",
                "--env",
                f"PROBE_CORE_FAIL={int(failing)}",
                "--entrypoint",
                "python3",
                args.image,
                "/driver.py",
            ]
            try:
                with (directory / "container.log").open("w") as log:
                    completed = subprocess.run(
                        command, stdout=log, stderr=subprocess.STDOUT, timeout=240
                    )
            finally:
                subprocess.run(
                    ["docker", "stop", "--time", "3", name], capture_output=True, check=False
                )
            assert completed.returncode == 0, f"{case}: container failed, see {directory}"
            bodies = requests[case]
            (directory / "requests.json").write_text(
                json.dumps(bodies, ensure_ascii=False, indent=2)
            )
            assert len(bodies) == 3, f"{case}: unexpected model request count"
            first, replay = tool_results(bodies[1]), tool_results(bodies[2])
            assert len(first) == 1 and replay == first, f"{case}: replay differs"
            text = first[0]
            database = directory / "state/agents/main/agent/openclaw-agent.sqlite"
            assert persisted_results(database, args.image) == first, f"{case}: persistence differs"
            records = (
                [json.loads(line) for line in (directory / "core.jsonl").read_text().splitlines()]
                if (directory / "core.jsonl").exists()
                else []
            )
            assert len(records) == (0 if disabled else 1), f"{case}: Core call count"
            received = source.strip()  # Native exec trims stdout before middleware.
            if case.startswith("large-") and args.host_output_policy == "legacy":
                received = received[:100000]
            core = None
            if records:
                assert records[0]["request"]["input"]["content"] == received
                assert records[0]["exit_code"] == (1 if failing else 0)
                if not failing:
                    core = json.loads(records[0]["stdout"])["result"]
            applied = case == "on" or (
                case == "large-json" and args.host_output_policy == "normalized"
            )
            if applied:
                assert core["disposition"] == "applied"
                assert text == core["output"] and text != received
                if case == "on":
                    assert list(csv.reader(io.StringIO(text))) == list(
                        csv.reader(io.StringIO(source))
                    )
                else:
                    decoded = subprocess.check_output(
                        [str(runtime / "core"), "decompress-toon"],
                        input=text,
                        text=True,
                        env={
                            **os.environ,
                            "TOKENLESS_SLS_ENABLED": "0",
                            "TOKENLESS_STATS_ENABLED": "0",
                            "TOKENLESS_DATA_DIR": str(output / "decode-data"),
                        },
                    )
                    assert json.loads(decoded) == document, "JSON candidate lost data"
            elif not case.startswith("large-"):
                assert text == received, f"{case}: passthrough changed"
            elif case == "large-off":
                baseline = text
                assert len(text) < len(source), "Expected downstream context budget"
            elif case in {"large-passthrough", "large-failure"}:
                if args.host_output_policy == "normalized":
                    assert text == baseline, f"{case}: differs from disabled baseline"
                else:
                    # Legacy preprocessing changes the downstream truncation notice too.
                    assert text != baseline and len(text) < len(received)
                    assert "TAIL_SENTINEL" not in text
                if core:
                    assert core["disposition"] == "passthrough"
            else:
                assert core["disposition"] != "applied", "Clipped JSON must not be compressed"
                assert len(text) < len(received) and "TAIL_SENTINEL" not in text
            stats = directory / "tokenless/stats.db"
            count = 0
            if stats.exists():
                with sqlite3.connect(f"file:{stats}?mode=ro", uri=True) as connection:
                    count = connection.execute("SELECT count(*) FROM stats").fetchone()[0]
            assert count == int(applied), f"{case}: unexpected statistics count"
            report["cases"].append(
                {
                    "case": case,
                    "pass": True,
                    "model_requests": len(bodies),
                    "core_calls": len(records),
                    "source_chars": len(source),
                    "core_input_chars": len(received) if records else None,
                    "disposition": core["disposition"] if core else None,
                    "replay_and_persistence_match": True,
                    "stats_rows": count,
                    "expected_host_truncation": bool(records)
                    and len(received) < len(source.strip()),
                    "first_consumption_chars": len(text),
                }
            )
            print(f"{case}: PASS", flush=True)
        report["pass"] = True
    finally:
        server.shutdown()
        for case in cases:
            shutil.rmtree(output / case / "state/cache/control-ui-assets", ignore_errors=True)
        report["binary_sha256"] = hashlib.sha256((runtime / "core").read_bytes()).hexdigest()
        report["adapter_sha256"] = hashlib.sha256(
            (runtime / "plugin/dist/index.js").read_bytes()
        ).hexdigest()
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(f"Evidence: {output}", flush=True)


if __name__ == "__main__":
    main()
