import argparse
import fcntl
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import shlex
import signal
import struct
import subprocess
import tempfile
import termios
import threading
import time


def process_info(pid):
    try:
        tail = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
        return {"pid": pid, "state": tail[0], "ppid": int(tail[1]), "start": int(tail[19]), "exe": os.readlink(f"/proc/{pid}/exe"), "cwd": os.readlink(f"/proc/{pid}/cwd")}
    except (OSError, ValueError, IndexError):
        return None


def descendants(parent):
    processes = [info for path in Path("/proc").iterdir() if path.name.isdigit() and (info := process_info(int(path.name)))]
    selected = {parent}
    while True:
        expanded = selected | {info["pid"] for info in processes if info["ppid"] in selected}
        if expanded == selected:
            return [info for info in processes if info["pid"] in selected and info["pid"] != parent]
        selected = expanded


def physical_frame(data, offset, selected="Allow once"):
    bottom = ("╰" + "─" * 38 + "╯").encode()
    action = data.find(f"> [ {selected} ]".encode(), offset)
    bottom_at = data.find(bottom, action) if action >= 0 else -1
    if bottom_at < 0:
        return None
    tail = re.match(rb"(?:\x1b\[0m)?\r\x1b\[0m\x1b\[\?25l\x1b\[([0-9]+);1H", data[bottom_at + len(bottom):])
    if tail is None:
        return None
    end = bottom_at + len(bottom) + tail.end()
    return {"selected": selected, "begin_offset": offset, "bottom_offset": bottom_at, "end_offset": end, "sentinel_row_1based": int(tail[1]), "sha256": hashlib.sha256(data[offset:end]).hexdigest()}


def geometry(pid):
    try:
        descriptor = os.open(f"/proc/{pid}/fd/0", os.O_RDONLY | os.O_NOCTTY)
    except OSError:
        return None
    try:
        rows, cols, unused_x, unused_y = struct.unpack("HHHH", fcntl.ioctl(descriptor, termios.TIOCGWINSZ, bytes(8)))
        return {"rows": rows, "cols": cols}
    except OSError:
        return None
    finally:
        os.close(descriptor)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--production", action="store_true")
    parser.add_argument("--scenario", choices=["resize", "kill-start", "kill-frame"], required=True)
    parser.add_argument("--out", type=Path, default=Path("/tmp/cosh-ui-canonical-evidence"))
    args = parser.parse_args()
    profile = args.profile.resolve()
    binaries = {name: profile / name for name in ["cosh-shell", "cosh-core", "cosh-ui-worker"]}
    hashes = {}
    for name, path in binaries.items():
        data = path.read_bytes()
        if data[:4] != b"\x7fELF":
            raise RuntimeError(f"actual Linux ELF required: {path}")
        hashes[name] = hashlib.sha256(data).hexdigest()
    output = args.out / f"{args.scenario}-{os.getpid()}"
    output.mkdir(parents=True, exist_ok=False)
    requests = []
    owned = {}
    worker_seen = False
    fault = []
    bash = None
    bash_drift = False
    stop_deadline = None
    outer_termios = termios.tcgetattr(0)
    geometries = []

    with tempfile.TemporaryDirectory(prefix="cosh-ui-canonical-") as temporary:
        home = Path(temporary)
        effect = home / "effect.txt"

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *unused):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                index = len(requests)
                requests.append(body)
                if index == 0:
                    arguments = json.dumps({"command": f"printf '%s\\n' ui_canonical_effect >> '{effect}'"})
                    chunks = [{"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "canonical-tool-1", "type": "function", "function": {"name": "shell", "arguments": arguments}}]}}]}, {"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}]
                else:
                    chunks = [{"choices": [{"index": 0, "delta": {"content": "UI_CANONICAL_MODEL_DONE"}}]}, {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}]
                payload = ("".join("data: " + json.dumps(chunk) + "\n\n" for chunk in chunks) + "data: [DONE]\n\n").encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(payload)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(payload)
                self.wfile.flush()

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        server.daemon_threads = True
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        environment = os.environ.copy()
        for key in ["DASHSCOPE_API_KEY", "ALIBABA_CLOUD_ACCESS_KEY_ID", "ALIBABA_CLOUD_ACCESS_KEY_SECRET", "ALIBABA_CLOUD_SECURITY_TOKEN", "GIT_DIR", "GIT_WORK_TREE", "COSH_SHELL_STATUS_SYMBOLS"]:
            environment.pop(key, None)
        environment.update({"HOME": str(home), "TMPDIR": str(home), "TERM": "xterm-256color", "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "COSH_LOG": "debug", "COSH_SHELL_LANG": "en-US", "COSH_SHELL_WIDTH": "40", "COSH_SHELL_ISOLATED": "1", "COSH_SHELL_INTEGRATION": "enhanced", "COSH_SHELL_BOOTSTRAP_PATH": "0", "COSH_SHELL_HEALTH_SCAN": "disabled", "COSH_RECOMMENDATIONS_ENABLED": "0", "COSH_SHELL_RAW_SHELL": "bash", "COSH_SHELL_DEFAULT_SHELL": "bash", "COSH_SHELL_APPROVAL_MODE": "auto", "COSH_APPROVAL_MODE": "auto", "COSH_CORE_PATH": str(binaries["cosh-core"]), "COSH_AI_PROVIDER": "ui-approval-canonical", "COSH_MODEL": "ui-approval-canonical-model", "OPENAI_BASE_URL": f"http://127.0.0.1:{server.server_port}/v1", "OPENAI_API_KEY": "canonical-test-only", "COSH_UI_APPROVAL_SLICE_TEST": "1", "COSH_UI_WORKER_PATH": str(binaries["cosh-ui-worker"])})
        for key, value in {"MAX_PAYLOAD_BYTES": 1048576, "MAX_DRAW_OPS": 4096, "MAX_TEXT_BYTES": 16384, "MAX_SURFACE_ROWS": 24, "HANDSHAKE_TIMEOUT_MS": 5000, "RENDER_TIMEOUT_MS": 5000, "HEARTBEAT_INTERVAL_MS": 100, "HEARTBEAT_TIMEOUT_MS": 3000, "BACKPRESSURE_TIMEOUT_MS": 5000, "SHUTDOWN_TIMEOUT_MS": 2000, "FAILURE_LIMIT": 3}.items():
            environment["COSH_UI_TEST_" + key] = str(value)
        if args.production:
            for key in list(environment):
                if key.startswith("COSH_UI_"):
                    environment.pop(key)
        command = [str(binaries["cosh-shell"]), "raw", "cosh-core"]
        physical_log = output / "script-output.bin"
        if args.scenario == "kill-frame":
            command = ["/usr/bin/script", "-q", "-e", "-f", "-E", "never", "-O", str(physical_log), "-c", "exec " + shlex.join(command)]
        shell = subprocess.Popen(command, cwd=home, env=environment)
        owned[shell.pid] = process_info(shell.pid)
        actual_shell = shell.pid if args.scenario != "kill-frame" else None
        host_binding = {"pid": actual_shell, "source": "direct-popen"} if actual_shell else None
        frame_offset = 0
        focus_roundtrip = []
        focus_worker = None

        def terminate(unused_signal, unused_frame):
            nonlocal stop_deadline
            stop_deadline = time.monotonic() + 0.75
            if shell.poll() is None:
                shell.terminate()

        signal.signal(signal.SIGTERM, terminate)
        signal.signal(signal.SIGHUP, terminate)
        try:
            while shell.poll() is None:
                if stop_deadline is not None and time.monotonic() >= stop_deadline:
                    break
                processes = descendants(shell.pid)
                for info in processes:
                    owned[info["pid"]] = info
                    if Path(info["exe"]).name == "cosh-ui-worker":
                        worker_seen = True
                        if actual_shell is None:
                            parent = process_info(info["ppid"])
                            if parent and Path(parent["exe"]).resolve() == binaries["cosh-shell"].resolve():
                                actual_shell = parent["pid"]
                                host_binding = {"pid": actual_shell, "start": parent["start"], "source": "worker-parent"}
                        frame = None
                        if args.scenario == "kill-frame" and physical_log.exists():
                            data = physical_log.read_bytes()
                            selected = ["Allow once", "Deny", "Allow once"][min(len(focus_roundtrip), 2)]
                            frame = physical_frame(data, frame_offset, selected)
                            if frame and not fault:
                                identity = (info["pid"], info["start"])
                                if focus_worker is None:
                                    focus_worker = identity
                                if identity != focus_worker:
                                    frame = None
                                else:
                                    focus_roundtrip.append({"worker_pid": info["pid"], "worker_start": info["start"], **frame})
                                    frame_offset = frame["end_offset"]
                                    if len(focus_roundtrip) < 3:
                                        frame = None
                        ready = args.scenario == "kill-start" or frame is not None
                        if ready and len(fault) < 3 and not any(item["pid"] == info["pid"] and item["start"] == info["start"] for item in fault):
                            current = process_info(info["pid"])
                            if current and current["start"] == info["start"] and current["exe"] == info["exe"]:
                                os.kill(info["pid"], signal.SIGKILL)
                                fault.append({"pid": info["pid"], "start": info["start"], "monotonic_ns": time.monotonic_ns(), "phase": "focus-roundtrip-consumed; complete physical frame; no internal generation claim" if frame else "worker-start-detected; publication not asserted", "frame": frame})
                                if frame:
                                    frame_offset = len(data)
                if actual_shell:
                    observed = geometry(actual_shell)
                    outer = geometry(os.getpid())
                    if observed and (not geometries or (geometries[-1]["inner"], geometries[-1]["outer"]) != (observed, outer)):
                        geometries.append({"monotonic_ns": time.monotonic_ns(), "host_pid": actual_shell, "inner": observed, "outer": outer})
                for info in processes:
                    if worker_seen and info["ppid"] == actual_shell and Path(info["exe"]).name == "bash":
                        if bash is None:
                            bash = info
                        elif (info["pid"], info["start"], info["cwd"]) != (bash["pid"], bash["start"], bash["cwd"]):
                            bash_drift = True
                time.sleep(0.01)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)
            if shell.poll() is None:
                shell.kill()
            shell.wait(timeout=10)
            for pid, info in owned.items():
                current = process_info(pid)
                if info and current and current["start"] == info["start"] and current["state"] != "Z":
                    os.kill(pid, signal.SIGKILL)
            cleanup_deadline = time.monotonic() + 0.5
            while time.monotonic() < cleanup_deadline and any(info and (current := process_info(pid)) and current["start"] == info["start"] for pid, info in owned.items()):
                time.sleep(0.01)
        effects = effect.read_text().splitlines() if effect.exists() else []
        remaining = [pid for pid, info in owned.items() if info and (current := process_info(pid)) and current["start"] == info["start"]]
        tool_result = len(requests) >= 2 and any(message.get("role") == "tool" for message in requests[1].get("messages", []))
        termios_restored = termios.tcgetattr(0) == outer_termios
        resize_proven = any(item["inner"] == item["outer"] == {"rows": 20, "cols": 47} for item in geometries)
        result = {"execution": "scripted-pty", "real_provider": False, "model_fixture": "loopback SSE only", "scenario": args.scenario, "production": args.production, "ui_test_environment": {key: value for key, value in environment.items() if key.startswith("COSH_UI_")}, "binary_sha256": hashes, "shell_exit": shell.returncode, "actual_shell_pid": actual_shell, "host_binding": host_binding, "geometry_observations": geometries, "outer_termios_restored": termios_restored, "request_count": len(requests), "host_tool_result": tool_result, "effect_count": len(effects), "worker_seen": worker_seen, "focus_roundtrip": focus_roundtrip, "fault": fault, "bash_baseline": bash, "bash_drift": bash_drift, "remaining_pids": remaining}
        (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        (output / "requests.json").write_text(json.dumps(requests, indent=2) + "\n")
        passed = shell.returncode == 0 and requests and requests[0].get("tools") and len(requests) == 2 and tool_result and effects == ["ui_canonical_effect"] and worker_seen and bash and not bash_drift and not remaining and termios_restored and resize_proven and (args.scenario == "resize" or len(fault) == 3)
        print(f"UI_CANONICAL_RESULT effect={len(effects)} requests={len(requests)} cleanup={len(remaining)} worker_seen={int(worker_seen)} status={'PASS' if passed else 'FAIL'}", flush=True)
        raise SystemExit(0 if passed else 1)


if __name__ == "__main__":
    main()
