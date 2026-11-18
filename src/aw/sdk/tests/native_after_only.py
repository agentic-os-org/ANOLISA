"""Actual native App: immediate prompt, late attach, reviewed output."""

import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import sys
import tempfile
import time

p = argparse.ArgumentParser()
p.add_argument("--bin", type=Path)
p.add_argument("--output", type=Path, required=True)
p.add_argument("--configured", action="store_true")
p.add_argument("--long", action="store_true")
p.add_argument("--boundary", choices=["after", "before", "input"], default="after")
for name in ["repo", "aw", "qwenpaw", "sdk"]:
    p.add_argument("--" + name, type=Path)
a = p.parse_args()
a.repo = a.repo or Path(__file__).resolve().parents[4]
a.aw = a.aw or a.bin / "aw"
a.qwenpaw = a.qwenpaw or Path(sys.executable).with_name("qwenpaw")
package_root = None
if a.sdk is None:
    import zipfile

    package_root = tempfile.TemporaryDirectory(prefix="aw-after-package-")
    build_python = os.environ.get(
        "AW_BUILD_PYTHON",
        (
            str(Path(os.environ["pythonLocation"]) / "bin/python3")
            if "pythonLocation" in os.environ
            else sys.executable
        ),
    )
    import subprocess

    subprocess.run(
        [
            build_python,
            "-m",
            "pip",
            "wheel",
            "--no-build-isolation",
            "--no-deps",
            "--no-index",
            str(a.repo / "src/aw/sdk/python"),
            "--wheel-dir",
            package_root.name,
        ],
        check=True,
        capture_output=True,
        timeout=90,
    )
    (wheel,) = Path(package_root.name).glob("*.whl")
    a.sdk = Path(package_root.name) / "installed"
    with zipfile.ZipFile(wheel) as z:
        z.extractall(a.sdk)
a.output.mkdir(parents=True, exist_ok=True)
sys.path[:0] = [
    str(a.repo / "src/aw/crates/aw-cli/tests/fixtures/sec_core"),
    str(a.repo / "src/aw/crates/aw-cli/tests/fixtures/qwenpaw"),
]
from agents import configure_qwenpaw
from runtime import Processes, await_condition
from native_app_config import ready, request, stage_user_configuration
import subprocess

with tempfile.TemporaryDirectory(prefix="aw-after-app-") as directory:
    root = Path(directory)
    for name in ["home", "work", "state"]:
        (root / name).mkdir(mode=0o700)
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(("AW_", "QWENPAW_", "QODER_", "AGENT_SEC_"))
    }
    env["PYTHONPATH"] = str(a.sdk)
    processes = Processes(root, a.output, env)
    try:
        argv, ports = configure_qwenpaw(root, a.qwenpaw, env, processes)
        preserved = stage_user_configuration(root)
        plugin = root / "home/plugins/aw-after"
        plugin.mkdir()
        (plugin / "plugin.json").write_text(
            json.dumps(
                {
                    "id": "aw-after",
                    "name": "After Only",
                    "version": "0.1.1",
                    "type": "general",
                    "entry": {"backend": "plugin.py"},
                    "dependencies": [],
                }
            )
        )
        if a.configured or a.boundary != "after":
            code = (
                "from aw_sdk.qwenpaw import QwenPawPlugin\nplugin=QwenPawPlugin.from_config(%r,target='app',executable=%r,attach_timeout_ms=5000)\n"
                % (str(root / "aw.json"), str(a.aw))
            )
        else:
            code = (
                "from aw_sdk.qwenpaw import AfterOnlyPlugin\nplugin=AfterOnlyPlugin(endpoint=%r,target='app',attach_timeout_ms=5000)\n"
                % str(root / "state/aw.sock")
            )
        (plugin / "plugin.py").write_text(code)
        doc = json.loads((root / "home/config.json").read_text())
        doc["plugins"]["aw-after"] = {"enabled": True}
        (root / "home/config.json").write_text(json.dumps(doc))
        provider = root / "provider.py"
        provider.write_text("""import json,sys
x=json.load(sys.stdin)
r={'api_version':'aw-provider/v1alpha1','request_id':x['request_id'],'status':'ok'}
if x['method']=='describe':r['operations']=[{'name':'scan','events':['input.submit','tool.before','tool.after'],'effects':['observe','block','replace_result']}]
elif x['method']=='invoke':
 if __import__('pathlib').Path(sys.argv[1]).exists():raise SystemExit(1)
 r['input_digest']=x['input_digest'];r['effects']=[{'type':'replace_result','text':'REVIEWED_SAFE_BODY'}] if 'SYNTHETIC_RAW_SECRET' in json.dumps(x['event']) else []
print(json.dumps(r))
""")
        config = root / "aw.json"
        config.write_text(
            json.dumps(
                {
                    "apiVersion": "aw/v1alpha1",
                    "kind": "AWConfiguration",
                    "metadata": {"name": "late-after"},
                    "spec": {
                        "daemon": {
                            "startup": "on_demand",
                            "endpoint": "auto",
                            "state_dir": str(root / "state"),
                        },
                        "execution": {
                            "guarantee": "native_hook",
                            "default_event_budget_ms": 3000,
                        },
                        "audit": {"enabled": True, "payload": "metadata_only"},
                        "agents": {"app": {"adapter": "qwenpaw", "argv": argv}},
                        "providers": {
                            "scan": {
                                "protocol": "aw-provider/v1alpha1",
                                "config": {},
                                "transport": {
                                    "type": "stdio",
                                    "location": "agent",
                                    "argv": [
                                        sys.executable,
                                        str(provider),
                                        str(root / "fail"),
                                    ],
                                },
                                "timeout_ms": 2000,
                                "max_output_bytes": 65536,
                            }
                        },
                        "events": {
                            "tool.after": {
                                "enabled": True,
                                "required": True,
                                "steps": [
                                    {
                                        "id": "scan",
                                        "provider": "scan",
                                        "operation": "scan",
                                        "effects": ["observe", "replace_result"],
                                        "on_error": "withhold_result",
                                    }
                                ],
                            }
                        },
                    },
                }
            )
        )
        if a.boundary != "after":
            document = json.loads(config.read_text())
            event = "input.submit" if a.boundary == "input" else "tool.before"
            document["spec"]["providers"]["check"] = {
                "protocol": "aw-provider/v1alpha1",
                "config": {"deny_file": str(root / "deny")},
                "transport": {
                    "type": "stdio",
                    "location": "agent",
                    "argv": [sys.executable, str(a.repo / "src/aw/sdk/tests/fixtures/sdk-deny.py")],
                },
                "timeout_ms": 2000,
                "max_output_bytes": 65536,
            }
            document["spec"]["events"] = {
                event: {
                    "enabled": True,
                    "required": True,
                    "steps": [
                        {
                            "id": "check",
                            "provider": "check",
                            "operation": "check",
                            "effects": ["observe", "block"],
                            "on_error": "block",
                        }
                    ],
                },
            }
            config.write_text(json.dumps(document))
        config.chmod(0o600)
        parsed = subprocess.run(
            [str(a.aw), "config", "show", "--config", str(config)],
            capture_output=True,
            text=True,
            env=env,
            timeout=5,
        )
        (a.output / "config.stderr").write_text(parsed.stderr)
        assert parsed.returncode == 0, parsed.stderr
        app = processes.start("app", argv, ports[:1])
        await_condition(lambda: ready(ports[0]), 45, "HTTP ready")
        source = root / "work/raw.txt"
        source.write_text("SYNTHETIC_RAW_SECRET")
        (root / "tool-call.json").write_text(
            json.dumps({"name": "read_file", "arguments": {"file_path": str(source)}})
        )
        (root / "tool-call-id.txt").write_text("late-after")
        if a.long and a.boundary == "input":
            (root / "model-delay-seconds").write_text("34")
        if a.long and a.boundary == "after":
            (root / "tool-call.json").write_text(
                json.dumps(
                    {
                        "name": "execute_shell_command",
                        "arguments": {"command": f"sleep 34; cat {source}", "timeout": 60},
                    }
                )
            )
        started_reply = time.monotonic()
        # A fast model reaches the installed boundary before the service exists.
        payload = {
            "session_id": "late-after",
            "user_id": "test",
            "input": [
                {
                    "role": "user",
                    "content": [{"type": "text", "text": "Read the file once."}],
                }
            ],
        }
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(request, ports[0], "console/chat", payload)
            if a.boundary == "input":
                # Give the immediate prompt time to reach the installed input gate.
                time.sleep(0.3)
                assert not (root / "model-requests.jsonl").exists()
                assert not future.done()
            else:
                await_condition(
                    lambda: (root / "model-requests.jsonl").exists(),
                    20,
                    "model accepted prompt before AW startup",
                )
            assert not (root / "state/aw.sock").exists()
            result = subprocess.run(
                [
                    str(a.aw),
                    "connect",
                    "--config",
                    str(config),
                    "--sdk-pid",
                    str(app.pid),
                    "--timeout-ms",
                    "5000",
                ],
                capture_output=True,
                text=True,
                timeout=10,
                env=env,
            )
            (a.output / "attach.json").write_text(result.stdout)
            (a.output / "attach.stderr").write_text(result.stderr)
            assert result.returncode == 0, (result.stdout, result.stderr)
            data = future.result(timeout=60)
        (a.output / "reply.sse").write_bytes(data)
        (a.output / "model-requests.jsonl").write_bytes(
            (root / "model-requests.jsonl").read_bytes()
        )
        if a.boundary != "after":
            from native_boundaries import verify

            verify(a, root, ports, payload, data, preserved, time.monotonic() - started_reply)
            raise SystemExit(0)
        assert b"REVIEWED_SAFE_BODY" in data, data.decode()
        assert b"SYNTHETIC_RAW_SECRET" not in data, data.decode()
        model = (root / "model-requests.jsonl").read_text()
        assert "REVIEWED_SAFE_BODY" in model and "SYNTHETIC_RAW_SECRET" not in model
        cases = [{"name": "late-attach", "passed": True}]
        if a.long:
            assert time.monotonic() - started_reply >= 34
            cases.append({"name": "long-tool", "passed": True})
            (root / "tool-call.json").write_text(
                json.dumps(
                    {
                        "name": "read_file",
                        "arguments": {"file_path": str(source)},
                    }
                )
            )
        # Verify ordinary output through the same reviewed event and model exits.
        source.write_text("ORDINARY_NATIVE_BODY")
        (root / "tool-call-id.txt").write_text("ordinary")
        payload["session_id"] = "ordinary"
        ordinary = request(ports[0], "console/chat", payload)
        (a.output / "ordinary.sse").write_bytes(ordinary)
        assert b"ORDINARY_NATIVE_BODY" in ordinary
        cases.append({"name": "ordinary", "passed": True})
        # Provider failure must suppress the original result while HTTP remains alive.
        (root / "fail").touch()
        source.write_text("SYNTHETIC_RAW_SECRET")
        (root / "tool-call-id.txt").write_text("provider-failure")
        payload["session_id"] = "provider-failure"
        failed = request(ports[0], "console/chat", payload)
        (a.output / "provider-failure.sse").write_bytes(failed)
        assert b"SYNTHETIC_RAW_SECRET" not in failed and ready(ports[0])
        cases.append({"name": "provider-failure", "passed": True})
        detached = subprocess.run(
            [
                str(a.aw),
                "disconnect",
                "--config",
                str(config),
                "--sdk-instance",
                json.loads(result.stdout)["application_id"],
            ],
            capture_output=True,
            text=True,
            timeout=10,
            env=env,
        )
        assert detached.returncode == 0, (detached.stdout, detached.stderr)
        # The model can respond without tools while AW is detached.
        (root / "plain-reply.txt").write_text("NATIVE_TEXT_WITHOUT_AW")
        payload["session_id"] = "plain"
        plain = request(ports[0], "console/chat", payload)
        assert b"NATIVE_TEXT_WITHOUT_AW" in plain
        (root / "plain-reply.txt").unlink()
        cases.append({"name": "plain-detached", "passed": True})
        # No attachment: reject the first tool after a bounded wait; no side effect.
        marker = root / "work/must-not-exist"
        (root / "tool-call.json").write_text(
            json.dumps(
                {
                    "name": "execute_shell_command",
                    "arguments": {"command": f"touch {marker}", "timeout": 5},
                }
            )
        )
        (root / "tool-call-id.txt").write_text("not-ready")
        payload["session_id"] = "not-ready"
        started = time.monotonic()
        missing = request(ports[0], "console/chat", payload)
        assert not marker.exists() and time.monotonic() - started < 10 and ready(ports[0])
        (a.output / "not-ready.sse").write_bytes(missing)
        cases.append({"name": "not-ready", "passed": True})
        assert all(path.read_bytes() == contents for path, contents in preserved.items())
        (a.output / "model-requests.jsonl").write_bytes(
            (root / "model-requests.jsonl").read_bytes()
        )
        (a.output / "results.json").write_text(
            json.dumps(
                {
                    "passed": True,
                    "cases": cases,
                    "prompt_before_aw": True,
                    "safe_events": True,
                    "safe_model": True,
                },
                indent=2,
            )
        )
    finally:
        processes.close()
        if (root / "state/aw.sock").exists():
            subprocess.run(
                [str(a.aw), "service", "stop", "--config", str(root / "aw.json")],
                capture_output=True,
                timeout=10,
                env=env,
            )

if package_root is not None:
    package_root.cleanup()
