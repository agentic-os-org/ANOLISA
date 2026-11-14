"""Opt-in byte-preserving capture proxy for an authorized Anthropic gateway."""

from http.server import BaseHTTPRequestHandler
import json
import os
from pathlib import Path
import urllib.error
import urllib.request

import native_worker as fixture

SETTINGS = json.loads(Path(os.environ["CLAUDE_HOSTED_SETTINGS"]).read_text())["env"]
BASE = SETTINGS.get("ANTHROPIC_BASE_URL", "https://api.anthropic.com").rstrip("/")
MODEL = SETTINGS["ANTHROPIC_MODEL"].split("[", 1)[0]
RESPONSES = []


class Model(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        path = self.path.removeprefix("/apps/anthropic")
        if path.split("?", 1)[0] != "/v1/messages":
            self.send_error(400, "Unsupported capture route")
            return
        body = self.rfile.read(int(self.headers["Content-Length"]))
        fixture.REQUESTS.append(json.loads(body))
        headers = {name: value for name, value in self.headers.items()
                   if name.lower() in {"content-type", "anthropic-version", "anthropic-beta"}}
        if SETTINGS.get("ANTHROPIC_AUTH_TOKEN"):
            headers["Authorization"] = "Bearer " + SETTINGS["ANTHROPIC_AUTH_TOKEN"]
        else:
            headers["x-api-key"] = SETTINGS["ANTHROPIC_API_KEY"]
        # Forward the exact inspected bytes. Credentials stay in this parent proxy.
        request = urllib.request.Request(BASE + path, data=body, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=90) as response:
                status, content_type = response.status, response.headers.get("Content-Type")
                data = response.read(32 * 1024 * 1024 + 1)
        except urllib.error.HTTPError as error:
            status, content_type = error.code, "application/json"
            detail = error.read(4096).decode(errors="replace")
            for key in ("ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"):
                if SETTINGS.get(key):
                    detail = detail.replace(SETTINGS[key], "[redacted]")
            Path("gateway-error.json").write_text(json.dumps({"status": status, "detail": detail}))
            data = json.dumps({"error": {"type": "gateway_error", "message": f"HTTP {status}"}}).encode()
        assert len(data) <= 32 * 1024 * 1024, "Gateway response limit"
        RESPONSES.append({"status": status, "bytes": len(data)})
        self.send_response(status)
        self.send_header("Content-Type", content_type or "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
