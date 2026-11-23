import argparse
import json
import os
from pathlib import Path
import signal
import ssl
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class TrialService(BaseHTTPRequestHandler):
    def log_message(self, *_args: object) -> None:
        with self.server.count_lock:
            with self.server.trace.open("a") as trace:
                trace.write(f"{self.command} {self.path}\n")

    def do_PUT(self) -> None:
        self.reply("fixture-imds-token")

    def do_GET(self) -> None:
        if self.path == "/latest/meta-data/instance-id":
            self.reply("i-trial-fixture")
        elif self.path == "/latest/meta-data/zone-id":
            self.reply("cn-hangzhou-b")
        elif self.path.endswith("/document"):
            self.reply({"instance-id": "i-trial-fixture"})
        elif "/pkcs7?" in self.path:
            self.reply("Zm9v")
        else:
            self.send_error(404)

    def do_POST(self) -> None:
        self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if self.path.endswith("/challenge"):
            challenge = "A" * 43
            data = {
                "challengeId": challenge,
                "audience": f"sysom:ecs-model:prod:cn-hangzhou:v1:{challenge}",
                "serverTime": "2026-01-01T00:00:00Z",
                "expiresAt": "2026-01-01T00:02:00Z",
            }
            self.reply({"code": "Success", "success": True, "data": data})
        elif self.path.endswith("/credentials"):
            data = {
                "accessKeyId": "STS.fixture",
                "accessKeySecret": "fixture-secret",
                "securityToken": "fixture-token",
                "serverTime": "2026-01-01T00:00:00Z",
                "expiration": "2026-01-01T00:15:00Z",
            }
            self.reply({"code": "Success", "success": True, "data": data})
        elif self.path == "/api/v1/copilot/generate_copilot_stream_response":
            with self.server.count_lock:
                self.server.model_requests += 1
                (self.server.fixture_root / "requests").write_text(
                    str(self.server.model_requests)
                )
            self.reply({"code": "Sysom.ServiceNotActivated", "success": False})
        else:
            self.send_error(404)

    def reply(self, value: object) -> None:
        body = (value if isinstance(value, str) else json.dumps(value)).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def run(profile: Path, language: str) -> int:
    if sys.platform != "linux" or os.geteuid() != 0:
        raise RuntimeError("trial fixture requires root inside an isolated Linux container")
    if os.environ.get("COSH_E2E_ISOLATED_CONTAINER") != "1":
        raise RuntimeError("explicit isolated container acknowledgement is required")
    signal.signal(signal.SIGTERM, lambda *_args: sys.exit(143))
    servers = []
    with tempfile.TemporaryDirectory(prefix="cosh-trial-") as directory:
        root = Path(directory)
        try:
            route = root / "cloud-route.so"
            subprocess.run(
                [
                    "cc", "-shared", "-fPIC", "-Wall", "-Werror",
                    str(Path(__file__).with_name("trial_cloud_route.c")),
                    "-ldl", "-o", str(route),
                ], check=True,
            )
            cert, key = root / "cert.pem", root / "key.pem"
            subprocess.run(
                [
                    "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                    "-keyout", str(key), "-out", str(cert), "-days", "1",
                    "-subj", "/CN=sysom.vpc-proxy.aliyuncs.com",
                    "-addext", "basicConstraints=critical,CA:FALSE",
                    "-addext",
                    "subjectAltName=DNS:sysom.vpc-proxy.aliyuncs.com,DNS:sysom.cn-hangzhou.aliyuncs.com",
                ], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            for address, port in [("127.0.0.1", 80), ("127.0.0.1", 443)]:
                server = ThreadingHTTPServer((address, port), TrialService)
                servers.append(server)
                server.fixture_root = root
                server.trace = profile / "trial-fixture-requests.log"
                server.count_lock = threading.Lock()
                server.model_requests = 0
                if port == 443:
                    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                    context.load_cert_chain(cert, key)
                    server.socket = context.wrap_socket(server.socket, server_side=True)
                threading.Thread(
                    target=lambda current=server: current.serve_forever(poll_interval=0.05),
                    daemon=True,
                ).start()
            home = root / "home"
            home.mkdir(mode=0o700)
            env = os.environ.copy()
            for name in list(env):
                if name.startswith(("COSH_", "OPENAI_", "DASHSCOPE_", "ALIBABA_CLOUD_")):
                    env.pop(name)
                elif name.lower().endswith("_proxy"):
                    env.pop(name)
            env.update(
                HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"),
                PATH=f"{profile}:{env['PATH']}", SSL_CERT_FILE=str(cert),
                LD_PRELOAD=str(route),
                TRIAL_FIXTURE_ROOT=str(root), TERM="xterm-256color",
                LANG="C.UTF-8", LC_ALL="C.UTF-8", COSH_SHELL_LANG=language,
                COSH_SHELL_WIDTH="96", COSH_SHELL_ISOLATED="1",
                COSH_SHELL_BOOTSTRAP_PATH="0", COSH_SHELL_HEALTH_SCAN="disabled",
                COSH_RECOMMENDATIONS_ENABLED="0", COSH_SHELL_STARTUP_BANNER="0",
                COSH_SHELL_RAW_SHELL="bash", COSH_SHELL_DEFAULT_SHELL="bash",
                COSH_CORE_PATH=str(profile / "cosh-core"),
            )
            child = subprocess.Popen([str(profile / "cosh-shell"), "raw", "cosh-core"], env=env)
            try:
                status = child.wait(timeout=180)
            finally:
                if child.poll() is None:
                    child.send_signal(signal.SIGTERM)
                    try:
                        child.wait(timeout=0.5)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait(timeout=0.5)
            preference = home / ".copilot-shell/cosh/preferences.toml"
            if preference.exists():
                raise AssertionError("session cancellation persisted trial preferences")
            print(f"TRIAL_FIXTURE_RESULT requests={servers[-1].model_requests} preferences=unchanged", flush=True)
            return status
        finally:
            for server in servers:
                server.shutdown()
                server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--language", choices=["en-US", "zh-CN"], required=True)
    args = parser.parse_args()
    raise SystemExit(run(args.profile.resolve(), args.language))
