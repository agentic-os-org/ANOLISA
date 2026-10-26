#!/usr/bin/env python3
"""Native plugin API seam for credential-free CLI/RPC regression; not SDK adoption."""
import asyncio
import http.server
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import time
from types import ModuleType, SimpleNamespace

if sys.argv[1:] == ["--version"]:
    print("QwenPaw, version 2.2.2.post1")
    raise SystemExit(0)
assert "AW_NATIVE_CONFIG" not in os.environ
root = Path(os.environ["FAKE_ROOT"])
explicit_profile = os.environ.get("QWENPAW_WORKING_DIR")
legacy_profile = Path("~/.copaw").expanduser()
profile = (Path(explicit_profile).expanduser() if explicit_profile else
           legacy_profile if legacy_profile.exists() else Path("~/.qwenpaw").expanduser()).resolve()
for name, values in {
    "agentscope.message": {"TextBlock": object, "ToolResultState": SimpleNamespace(DENIED="denied")},
    "agentscope.middleware": {"MiddlewareBase": object},
    "agentscope.tool": {"ToolResponse": object, "ToolChunk": type("ToolChunk", (), {})},
}.items():
    module = ModuleType(name)
    module.__dict__.update(values)
    sys.modules[name] = module
factories = []


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def respond(self, value):
        payload = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        assert self.path == "/api/plugins/aw-native/status"
        if (root / "fail-status").exists():
            self.send_error(503)
        elif factories:
            self.respond({"id": "aw-native", "loaded": True})
        else:
            self.send_error(404)

    def do_POST(self):
        (root / "post-seen").write_text(self.path)
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))) or b"{}")
        if self.path == "/api/plugins/install":
            assert body["force"] is False
            if (root / "fail-install").exists():
                self.send_error(503)
                return
            destination = profile / "plugins/aw-native"
            shutil.copytree(body["source"], destination)
            spec = importlib.util.spec_from_file_location("attach_plugin", destination / "plugin.py")
            plugin = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(plugin)
            plugin.version = lambda name: "2.2.2.post1" if name == "qwenpaw" else "2.0.10"
            startup = []
            plugin.plugin.register(SimpleNamespace(config={}, register_middleware=lambda f, **kw: factories.append(f),
                register_startup_hook=lambda name, callback: startup.append(callback)))
            for callback in startup:
                asyncio.run(callback())
            self.respond({"id": "aw-native", "loaded": True})
        elif self.path in ("/begin", "/end"):
            phase = "request_start" if self.path == "/begin" else "request_end"
            asyncio.run(factories[0](None, None).lifecycle(phase, "request"))
            self.respond({"phase": phase})
        else:
            self.send_error(404)

    def do_DELETE(self):
        assert self.path == "/api/plugins/aw-native"
        factories.clear()
        shutil.rmtree(profile / "plugins/aw-native")
        self.respond({"uninstalled": True})


port = int(sys.argv[sys.argv.index("--port") + 1])
with http.server.HTTPServer(("127.0.0.1", port), Handler) as server:
    temporary = root / "native-pid.tmp"
    temporary.write_text(json.dumps(os.getpid()))
    temporary.replace(root / "native-pid.json")
    server.timeout = .1
    deadline = time.monotonic() + 40
    while time.monotonic() < deadline:
        server.handle_request()
        if (root / "close-listener").exists():
            break
    server.server_close()
    if (root / "close-listener").exists():
        (root / "listener-closed").write_text("closed")
        while time.monotonic() < deadline:
            time.sleep(.05)
