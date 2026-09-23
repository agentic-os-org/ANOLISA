#!/usr/bin/env python3

# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""MCP server wrapping claw-eval mock services.

Reads a task.yaml, starts declared mock services, exposes their APIs as MCP tools.
Also exposes HTTP /audit and /reset endpoints for claw-eval grading compatibility.

Usage:
    python mcp_mock_services.py --task-yaml /path/to/task.yaml [--http-port PORT]
"""

from __future__ import annotations

import argparse
import inspect
import json
import os
import subprocess
import sys
import time
import threading
from pathlib import Path
from typing import Any
from http.server import HTTPServer, BaseHTTPRequestHandler

import re

import httpx

try:
    import yaml
except ImportError:
    print("pyyaml required: pip install pyyaml", file=sys.stderr)
    sys.exit(1)


def _shift_url(url: str, offset: int) -> str:
    """Replace localhost:<port> with localhost:<port+offset>."""
    if offset == 0:
        return url
    return re.sub(
        r"localhost:(\d+)",
        lambda m: f"localhost:{int(m.group(1)) + offset}",
        url,
    )


class MockServiceManager:
    """Manages claw-eval mock service processes + exposes them via HTTP and MCP."""

    def __init__(self, task_yaml_path: str | Path, port_offset: int = 0) -> None:
        self.task_yaml_path = Path(task_yaml_path).resolve()
        self.task_dir = self.task_yaml_path.parent
        # task YAML is at: claw-eval/tasks/<ID>/task.yaml
        # project root (claw-eval/) is parent.parent.parent
        self.project_root = self.task_yaml_path.parent.parent.parent
        self.port_offset = port_offset

        with open(self.task_yaml_path) as f:
            self.task = yaml.safe_load(f)

        self.services = self.task.get("services", [])
        self.processes: list[subprocess.Popen] = []
        self._audit_log: list[dict] = []

        # Build tool -> endpoint map (apply port offset to URLs)
        tools = self.task.get("tools", [])
        endpoints = {ep["tool_name"]: ep for ep in self.task.get("tool_endpoints", [])}
        self.tool_endpoints = []
        for tool in tools:
            name = tool.get("name", "")
            ep = endpoints.get(name)
            entry = dict(tool)
            if ep:
                entry["endpoint_url"] = _shift_url(ep.get("url", ""), port_offset)
                entry["endpoint_method"] = ep.get("method", "POST")
            self.tool_endpoints.append(entry)

    # ---- Service lifecycle ----

    def start_all(self) -> None:
        for svc in self.services:
            self._start_service(svc)

    def _start_service(self, svc: dict) -> None:
        name = svc["name"]
        port = svc["port"] + self.port_offset
        health_check = _shift_url(svc.get("health_check", ""), self.port_offset)
        health_method = svc.get("health_check_method", "POST")

        if self._is_healthy(health_check, health_method):
            print(f"[mcp-mock] '{name}' already running on :{port}", file=sys.stderr)
            return

        cmd = svc["command"].split()
        env = os.environ.copy()
        env["no_proxy"] = "localhost,127.0.0.1"
        env["NO_PROXY"] = "localhost,127.0.0.1"
        env["PORT"] = str(port)
        for k, v in svc.get("env", {}).items():
            if v.startswith("tasks/"):
                v = str(self.project_root / v)
            env[k] = v

        print(f"[mcp-mock] Starting '{name}': {' '.join(cmd)} (port {port})", file=sys.stderr)
        proc = subprocess.Popen(
            cmd, env=env, cwd=str(self.project_root),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        self.processes.append(proc)

        # Give it a moment to start
        time.sleep(1)

        timeout = svc.get("ready_timeout", 15)
        if self._wait_health(health_check, health_method, timeout):
            print(f"[mcp-mock] '{name}' ready on :{port}", file=sys.stderr)
        else:
            # Check if process is still alive
            if proc.poll() is not None:
                print(f"[mcp-mock] ERROR: '{name}' exited with code {proc.returncode}", file=sys.stderr)
            else:
                print(f"[mcp-mock] WARN: '{name}' health check timed out (but process alive)", file=sys.stderr)

    def stop_all(self) -> None:
        for proc in reversed(self.processes):
            proc.terminate()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        self.processes.clear()

    def reset_all(self) -> None:
        """Reset all mock services and clear audit log."""
        self._audit_log.clear()
        for svc in self.services:
            ep = svc.get("reset_endpoint")
            if ep:
                try:
                    httpx.post(_shift_url(ep, self.port_offset), timeout=5)
                except Exception:
                    pass

    def get_audit_data(self) -> dict[str, Any]:
        """Build audit data in claw-eval format for grading.

        Returns dict keyed by service name, each containing:
        - calls: list of tool calls with endpoint, request_body, response_body, timestamp
        """
        # Group audit entries by service
        service_calls: dict[str, list] = {}
        for entry in self._audit_log:
            # Extract service name from URL (e.g., http://localhost:9100/rss/feeds -> rss)
            url = entry["url"]
            # Find which service this URL belongs to
            for svc in self.services:
                port = svc.get("port")
                if port:
                    actual_port = port + self.port_offset
                    if f":{actual_port}/" in url:
                        svc_name = svc["name"]
                        if svc_name not in service_calls:
                            service_calls[svc_name] = []
                        # Convert to claw-eval audit format
                        service_calls[svc_name].append({
                            "endpoint": url.split(f":{actual_port}")[-1],  # e.g., /rss/feeds
                            "request_body": entry.get("request", {}),
                            "response_body": entry.get("response"),
                            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S+00:00", time.gmtime(entry["timestamp"])),
                        })
                        break

        return service_calls

    def _is_healthy(self, url: str, method: str) -> bool:
        if not url:
            return True
        try:
            resp = httpx.post(url, json={}, timeout=3) if method == "POST" else httpx.get(url, timeout=3)
            return resp.status_code == 200
        except Exception:
            return False

    def _wait_health(self, url: str, method: str, timeout: int) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self._is_healthy(url, method):
                return True
            time.sleep(0.5)
        return False

    # ---- Service calls ----

    async def call_tool(self, tool_name: str, **kwargs) -> str:
        """Call a mock service endpoint and return JSON response."""
        ep_map = {t["name"]: t for t in self.tool_endpoints}
        ep = ep_map.get(tool_name)
        if not ep:
            return json.dumps({"error": f"Unknown tool: {tool_name}"})

        url = ep["endpoint_url"]
        method = ep.get("endpoint_method", "POST")

        # MCP wraps all parameters in a single 'kwargs' field.
        # Unwrap it so the mock service receives the expected flat dict.
        if len(kwargs) == 1 and "kwargs" in kwargs:
            inner = kwargs["kwargs"]
            if isinstance(inner, str):
                # kwargs might be a JSON string
                try:
                    inner = json.loads(inner)
                except (json.JSONDecodeError, TypeError):
                    inner = {}
            kwargs = inner if isinstance(inner, dict) else {}

        try:
            async with httpx.AsyncClient(timeout=30) as client:
                if method == "GET":
                    resp = await client.get(url, params=kwargs)
                else:
                    resp = await client.post(url, json=kwargs)
                result = resp.json()
                self._audit_log.append({
                    "tool": tool_name, "url": url, "method": method,
                    "request": kwargs, "response": result, "status": resp.status_code,
                    "timestamp": time.time(),
                })
                return json.dumps(result, ensure_ascii=False, indent=2)
        except Exception as e:
            return json.dumps({"error": f"Call failed: {e}"})

    @property
    def audit_log(self) -> list[dict]:
        return list(self._audit_log)


class AuditHTTPServer:
    """HTTP server exposing /audit and /reset endpoints for claw-eval grading."""

    def __init__(self, manager: MockServiceManager, port: int = 17090):
        self.manager = manager
        self.port = port
        self.server = None
        self.thread = None

    def _create_handler(self):
        """Create handler class with closure over manager."""
        manager = self.manager

        class AuditHandler(BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path == "/audit" or self.path.startswith("/audit?"):
                    audit_data = manager.get_audit_data()
                    # Return all services combined (claw-eval expects per-service keys)
                    self._send_json(200, audit_data)
                elif self.path == "/health":
                    self._send_json(200, {"status": "ok"})
                else:
                    # Try to match /<service>/audit pattern
                    parts = self.path.strip("/").split("/")
                    if len(parts) == 2 and parts[1] == "audit":
                        svc_name = parts[0]
                        audit_data = manager.get_audit_data()
                        self._send_json(200, audit_data.get(svc_name, {"calls": []}))
                    else:
                        self._send_json(404, {"error": "not found"})

            def do_POST(self):
                if self.path == "/reset":
                    manager.reset_all()
                    self._send_json(200, {"status": "reset"})
                else:
                    # Try to match /<service>/reset pattern
                    parts = self.path.strip("/").split("/")
                    if len(parts) == 2 and parts[1] == "reset":
                        svc_name = parts[0]
                        # Reset specific service
                        for svc in manager.services:
                            if svc["name"] == svc_name and svc.get("reset_endpoint"):
                                try:
                                    httpx.post(svc["reset_endpoint"], timeout=5)
                                except Exception:
                                    pass
                        manager._audit_log.clear()
                        self._send_json(200, {"status": "reset"})
                    else:
                        self._send_json(404, {"error": "not found"})

            def _send_json(self, status_code: int, data: dict):
                self.send_response(status_code)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(json.dumps(data).encode())

            def log_message(self, format, *args):
                # Suppress default logging
                pass

        return AuditHandler

    def start(self):
        """Start HTTP server in background thread."""
        handler = self._create_handler()
        self.server = HTTPServer(("127.0.0.1", self.port), handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        print(f"[mcp-mock] Audit HTTP server started on http://127.0.0.1:{self.port}", file=sys.stderr)

    def stop(self):
        """Stop HTTP server."""
        if self.server:
            self.server.shutdown()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--task-yaml", required=True)
    parser.add_argument("--http-port", type=int, default=17090,
                        help="Port for audit HTTP server (default: 17090)")
    parser.add_argument("--mcp-only", action="store_true",
                        help="Run MCP server only (no audit HTTP server)")
    parser.add_argument("--no-start-services", action="store_true",
                        help="Don't start mock services (assume they're already running)")
    parser.add_argument("--port-offset", type=int, default=0,
                        help="Offset for all service ports (enables parallel runs)")
    args = parser.parse_args()

    manager = MockServiceManager(args.task_yaml, port_offset=args.port_offset)
    task_id = manager.task.get("task_id", "?")
    print(f"[mcp-mock] Task: {task_id}", file=sys.stderr)
    print(f"[mcp-mock] Services: {len(manager.services)}, Tools: {len(manager.tool_endpoints)}", file=sys.stderr)

    # Start mock services BEFORE MCP server (unless disabled)
    if not args.no_start_services:
        manager.start_all()
    else:
        print("[mcp-mock] Skipping service start (assuming already running)", file=sys.stderr)

    # Start audit HTTP server (for claw-eval grading) unless --mcp-only
    audit_server = None
    if not args.mcp_only:
        audit_server = AuditHTTPServer(manager, port=args.http_port)
        audit_server.start()

    # Now create and run the MCP server
    from mcp.server.fastmcp import FastMCP

    mcp = FastMCP(
        name=f"claw-eval-{task_id}",
        instructions=(
            f"You are working on claw-eval task: {task_id}. "
            f"Use the tools below to interact with mock services. "
            f"Each tool calls the corresponding HTTP endpoint automatically."
        ),
    )

    # JSON schema type → Python type mapping for FastMCP schema inference.
    # FastMCP derives the MCP tool schema from function parameter annotations,
    # so we must provide correct types to preserve array/object/number semantics
    # declared in task.yaml (bug-fix P1: schema type loss).
    import typing as _typing
    _JSON_TYPE_MAP = {
        "string": str,
        "integer": int,
        "number": float,
        "boolean": bool,
        "object": dict,
        "array": list,  # fallback when items type is unknown
    }

    def _python_type_for(prop_info: dict):
        """Map a JSON schema property definition to a Python type annotation."""
        json_type = prop_info.get("type", "string")
        if json_type == "array":
            items = prop_info.get("items", {})
            items_type = items.get("type")
            if items_type and items_type in _JSON_TYPE_MAP:
                return _typing.List[_JSON_TYPE_MAP[items_type]]
            return list
        return _JSON_TYPE_MAP.get(json_type, str)

    for tool_def in manager.tool_endpoints:
        name = tool_def["name"]
        desc = tool_def.get("description", f"Call {name}")
        url = tool_def.get("endpoint_url", "?")
        method = tool_def.get("endpoint_method", "POST")
        schema = tool_def.get("input_schema", {})

        # Build properties dict for the tool decorator
        properties = schema.get("properties", {})
        required = schema.get("required", [])

        # Create a handler with explicit parameters matching the tool schema.
        # Uses inspect.Signature with type annotations so FastMCP correctly
        # infers the MCP tool schema (preserving array/object types from
        # task.yaml instead of collapsing everything to string).
        def make_handler(tname: str, props: dict, req_fields: list):
            parameters = []
            for k, pinfo in props.items():
                py_type = _python_type_for(pinfo)
                if k in req_fields:
                    parameters.append(
                        inspect.Parameter(k, inspect.Parameter.KEYWORD_ONLY,
                                          annotation=py_type)
                    )
                else:
                    default = pinfo.get("default", "")
                    parameters.append(
                        inspect.Parameter(k, inspect.Parameter.KEYWORD_ONLY,
                                          default=default, annotation=py_type)
                    )

            sig = inspect.Signature(parameters)

            async def handler(**kwargs):
                return await manager.call_tool(tname, **kwargs)

            handler.__signature__ = sig
            return handler

        handler = make_handler(name, properties, required)
        handler.__doc__ = f"{desc}\n\nEndpoint: {method} {url}"

        # Register the tool
        registered = mcp.tool(
            name=name,
            description=f"{desc} ({method} {url})",
        )(handler)

    print(f"[mcp-mock] MCP server running with {len(manager.tool_endpoints)} tools", file=sys.stderr)

    # Run MCP server (stdio transport)
    try:
        mcp.run()
    finally:
        # Cleanup on exit
        if audit_server:
            audit_server.stop()
        manager.stop_all()


if __name__ == "__main__":
    main()
