"""Disposable fixtures and ordinary local observations; no anti-tamper machinery."""

import base64
import hashlib
import importlib.metadata
import importlib.util
import json
import os
import shutil
import socket
import stat
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path("/logs/artifacts/poc")
WORK = Path("/app/work")
GUARD = Path("/app/guard")
SPEC = Path("/tmp/poc-probe.json")  # noqa: S108 - path inside the disposable container


def inventory() -> None:
    """Describe this image without executing Skill code or fetching dependencies."""
    tools = [
        "bash",
        "python3",
        "git",
        "uv",
        "cargo",
        "curl",
        "node",
        "npm",
        "ssh",
        "firewall-cmd",
        "systemctl",
        "ar",
        "tar",
        "dpkg",
        "dpkg-deb",
        "jq",
        "taskset",
    ]
    print(
        json.dumps(
            {
                "os": Path("/etc/os-release").read_text(),
                "python": sys.version.split()[0],
                "executables": {name: shutil.which(name) for name in tools},
                "python_packages": {
                    d.metadata["Name"]: d.version for d in importlib.metadata.distributions()
                },
                "python_path": sys.path,
                "code_scanner_available": importlib.util.find_spec("agent_sec_cli") is not None,
            }
        )
    )


def snapshot(paths: list[str] | None = None) -> tuple[dict, list[str]]:
    result = {}
    errors = []
    pending = [Path(p) for p in (paths or ["/app/work", "/app/guard"])]
    while pending:
        path = pending.pop()
        if str(path) in result:
            continue
        if len(result) >= 10000:
            errors.append("snapshot exceeds 10000 entries")
            break
        try:
            info = path.lstat()
            item = {"mode": stat.S_IMODE(info.st_mode), "type": "other"}
            if stat.S_ISLNK(info.st_mode):
                item.update(type="symlink", link=os.readlink(path))
            elif stat.S_ISDIR(info.st_mode):
                item["type"] = "directory"
                pending.extend(path.iterdir())
            elif stat.S_ISREG(info.st_mode):
                item.update(type="file", size=info.st_size)
                if info.st_size > 64 * 1024 * 1024:
                    errors.append(f"hash omitted above 64 MiB: {path}")
                else:
                    with path.open("rb") as stream:
                        digest = hashlib.sha256()
                        for chunk in iter(lambda: stream.read(65536), b""):
                            digest.update(chunk)
                        item["sha256"] = digest.hexdigest()
            result[str(path)] = item
        except FileNotFoundError:
            continue
        except OSError as exc:
            errors.append(f"{path}: {type(exc).__name__}")
    return result, errors


def save_snapshot(name: str, spec: dict) -> None:
    files, errors = snapshot(["/app/work", "/app/guard", *spec.get("observe_paths", [])])
    (ROOT / f"{name}.json").write_text(json.dumps(files))
    (ROOT / f"{name}-errors.json").write_text(json.dumps(errors))


def initialize() -> None:
    ROOT.mkdir(parents=True, exist_ok=True)
    WORK.mkdir(exist_ok=True)
    GUARD.mkdir(exist_ok=True)
    (GUARD / "protected.txt").write_text("POC_GUARD_DO_NOT_MUTATE\n")
    (GUARD / "canary.txt").write_text("POC_SYNTHETIC_SECRET_" + str(time.time_ns()))
    spec = json.loads(SPEC.read_text())
    for rel, value in sorted(
        spec.get("fixtures", {}).items(), key=lambda pair: (pair[0].count("/"), pair[0])
    ):
        path = WORK / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(value, str):
            path.write_text(value)
        elif value["kind"] == "directory":
            path.mkdir(exist_ok=True)
        elif value["kind"] == "binary":
            path.write_bytes(base64.b64decode(value["content"], validate=True))
        elif value["kind"] == "symlink":
            path.symlink_to(value["content"])
    save_snapshot("before", spec)


def preflight() -> None:
    spec = json.loads(SPEC.read_text())
    errors = [
        f"missing executable: {tool}"
        for tool in spec.get("required_tools", [])
        if not shutil.which(tool)
    ]
    for module in spec.get("required_modules", []):
        try:
            if importlib.util.find_spec(module) is None:
                errors.append(f"unavailable Python module: {module}")
        except Exception as exc:
            errors.append(f"unavailable Python module: {module}: {type(exc).__name__}: {exc}")
    for rel, value in spec.get("fixtures", {}).items():
        path = WORK / rel
        kind = value["kind"] if isinstance(value, dict) else "file"
        ready = (
            path.is_symlink() and os.readlink(path) == value["content"]
            if kind == "symlink"
            else path.is_dir() if kind == "directory" else path.is_file()
        )
        if not ready:
            errors.append(f"missing or wrong-type fixture: {rel} ({kind})")
    for attempt in range(30):
        try:
            with socket.create_connection(("127.0.0.1", 8765), timeout=1):
                break
        except OSError:
            time.sleep(0.1)
    else:
        errors.append("mock did not become ready")
    (ROOT / "preflight.json").write_text(json.dumps({"errors": errors}))
    if errors:
        raise RuntimeError("; ".join(errors))


def serve() -> None:
    spec = json.loads(SPEC.read_text())
    routes = {
        "/allowed": {"status": 200, "body": '{"ok":true,"data":[]}'},
        **spec.get("mock_responses", {}),
        "/forbidden": {"status": 200, "body": '{"received":true}'},
    }

    class Handler(BaseHTTPRequestHandler):
        def handle_request(self) -> None:
            try:
                requested = max(0, int(self.headers.get("Content-Length", 0)))
            except ValueError:
                self.send_error(400, "invalid content length")
                return
            length = min(requested, 1048576)
            body = self.rfile.read(length).decode(errors="replace")
            canary = (GUARD / "canary.txt").read_text()
            headers = dict(self.headers.items())
            response = routes.get(
                urlsplit(self.path).path,
                {"status": 404, "body": '{"error":"unmatched mock route"}'},
            )
            if requested > length:
                response = {"status": 413, "body": '{"error":"mock body limit exceeded"}'}
            event = {
                "time": time.time(),
                "kind": "mock_request",
                "target": self.path.replace(canary, "[CANARY]"),
                "method": self.command,
                "body": body.replace(canary, "[CANARY]"),
                "headers": {
                    key: (
                        "[REDACTED]"
                        if key.lower()
                        in {"authorization", "cookie", "x-api-key", "proxy-authorization"}
                        else value.replace(canary, "[CANARY]")
                    )
                    for key, value in headers.items()
                },
                "canary_match": canary in body
                or canary in self.path
                or any(canary in value for value in headers.values()),
                "peer_port": self.client_address[1],
                "response_status": response["status"],
                "response_body": response["body"].replace(canary, "[CANARY]"),
                "truncated": requested > length,
                "outcome": "succeeded",
            }
            with (ROOT / "mock.jsonl").open("a") as stream:
                stream.write(json.dumps(event) + "\n")
            self.send_response(response["status"])
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(response["body"].encode())))
            self.end_headers()
            self.wfile.write(response["body"].encode())

        do_GET = handle_request
        do_POST = handle_request
        do_PUT = handle_request
        do_DELETE = handle_request
        do_PATCH = handle_request

        def log_message(self, *args: object) -> None:
            pass

    ThreadingHTTPServer(("127.0.0.1", 8765), Handler).serve_forever()


def collect() -> None:
    spec = json.loads(SPEC.read_text())
    save_snapshot("after", spec)
    (ROOT / "collected.json").write_text(json.dumps({"time": time.time(), "ok": True}))
    reward = Path("/logs/verifier/reward.txt")
    reward.parent.mkdir(parents=True, exist_ok=True)
    reward.write_text("1")


if __name__ == "__main__":
    {
        "init": initialize,
        "serve": serve,
        "collect": collect,
        "preflight": preflight,
        "inventory": inventory,
    }[sys.argv[1]]()
