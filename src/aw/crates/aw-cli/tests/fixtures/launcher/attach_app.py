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
    print("QwenPaw, version 2.2.2b4")
    raise SystemExit(0)
assert "AW_NATIVE_CONFIG" not in os.environ
root = Path(os.environ["FAKE_ROOT"])
profile = Path(os.environ["QWENPAW_WORKING_DIR"])
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

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))) or b"{}")
        if self.path == "/api/plugins/install":
            assert body["force"] is False
            destination = profile / "plugins/aw-native"
            shutil.copytree(body["source"], destination)
            spec = importlib.util.spec_from_file_location("attach_plugin", destination / "plugin.py")
            plugin = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(plugin)
            plugin.version = lambda name: "2.2.2b4" if name == "qwenpaw" else "2.0.9"
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
