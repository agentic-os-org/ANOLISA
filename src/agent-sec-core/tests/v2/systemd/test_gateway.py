"""Secret gateway acceptance on a host with the gateway installed; run explicitly as root.

Everything stays on the host: a local upstream on a non-loopback address plays
api.github.com, github.com (backed by `git http-backend`) and example.com,
clients pin those names to it without DNS, and the gateway unit temporarily
trusts only a throwaway CA for its upstream connections. Tokens are generated
dummies. The tests add an address to `lo`, a runtime drop-in for the gateway
unit, and restart agent-sec-core.service, so they run only when
ASC_GATEWAY_E2E_USER names the non-root user that owns the test Scopes.
The gateway must be disabled with no credential Scopes configured beforehand.
"""

import base64
import contextlib
import http.server
import json
import os
import pwd
import secrets
import select
import shutil
import signal
import socket
import ssl
import string
import subprocess
import tempfile
import threading
import time
from pathlib import Path

import pytest

AGENT = "asc-e2e"
OTHER_AGENT = "asc-e2e-other"
TABLE = "agent_sec_gateway"
RUNTIME = Path("/run/agent-sec-gateway")
UNIT = "agent-sec-gateway.service"
DAEMON_UNIT = "agent-sec-core.service"
# Benchmark-range address: not loopback, so the nft redirect still applies.
UPSTREAM = "198.18.0.1"
HOSTS = ("api.github.com", "github.com", "example.com")
DROP_IN = Path(f"/run/systemd/system/{UNIT}.d/asc-e2e-upstream.conf")
REPOSITORY = "octocat/hello.git"
GIT_RESOLVE = f"-c http.curloptResolve=github.com:443:{UPSTREAM}"


def command(*args, check=True, timeout=90):
    # The domain user may not be able to enter the test's working directory.
    return subprocess.run(
        args,
        capture_output=True,
        text=True,
        check=check,
        timeout=timeout,
        cwd="/",
        env={
            "PATH": os.environ["PATH"],
            "LANG": "C.UTF-8",
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
        },
    )


def cli_json(cli, *args, check=True):
    result = command(cli, *args, check=check)
    return json.loads(result.stdout) if result.returncode == 0 else result


def gateway(cli, *args, check=True):
    return cli_json(cli, "gateway", *args, check=check)


# The root worker matches an isolated executable. Its already running child uses
# a different executable, so discovery admits the root tree rather than overlapping roots.
AGENT_CODE = r"""
import json
import os
import subprocess
import sys
from pathlib import Path

cli, scope_id = sys.argv[1:3]
child = None
if len(sys.argv) == 3:
    child = subprocess.Popen(
        ["/usr/bin/python3", "-u", "-c", os.environ["ASC_E2E_AGENT_CODE"], cli, scope_id, "worker"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
    )
    snapshot = json.loads(child.stdout.readline())
else:
    snapshot = None
print(json.dumps({"pid": os.getpid(), "uid": os.getuid(), "child": snapshot}), flush=True)
try:
    for line in sys.stdin:
        request = json.loads(line)
        if request.get("snapshot"):
            child.stdin.write(json.dumps({**request, "snapshot": False}) + "\n")
            child.stdin.flush()
            print(child.stdout.readline().strip(), flush=True)
            continue
        environment = os.environ.copy()
        if request.get("refresh", True):
            grant = subprocess.run(
                [cli, "gateway", "ca"],
                capture_output=True, text=True, check=False, timeout=30,
            )
            if grant.returncode:
                print(json.dumps({"returncode": grant.returncode,
                                  "stdout": grant.stdout, "stderr": grant.stderr,
                                  "stage": "ca"}), flush=True)
                continue
            ca = json.loads(grant.stdout)
            assert set(ca) == {"caBundlePath"}
            for variable in ("SSL_CERT_FILE", "REQUESTS_CA_BUNDLE",
                             "CURL_CA_BUNDLE", "GIT_SSL_CAINFO"):
                environment[variable] = ca["caBundlePath"]
        if request.get("probe"):
            descendant = subprocess.run(
                ["/usr/bin/python3", "-c",
                 "from pathlib import Path; print(Path('/proc/self/cgroup').read_text().strip())"],
                capture_output=True, text=True, check=True, timeout=10,
            )
            result = {"pid": os.getpid(), "uid": os.getuid(),
                      "cgroup": Path("/proc/self/cgroup").read_text().strip(),
                      "descendantCgroup": descendant.stdout.strip(),
                      "environment": environment}
            print(json.dumps(result), flush=True)
            continue
        # Git's helper belongs to this Agent; no gateway CLI helper or real token
        # is passed in argv. Only get produces output; store/erase are no-ops.
        environment.update({
            "GIT_CONFIG_COUNT": "2",
            "GIT_CONFIG_KEY_0": "credential.helper", "GIT_CONFIG_VALUE_0": "",
            "GIT_CONFIG_KEY_1": "credential.https://github.com.helper",
            "GIT_CONFIG_VALUE_1": '!f() { if [ "$1" = get ]; then '
                'printf "username=x-access-token\\npassword=%s\\n" "$GITHUB_TOKEN"; fi; }; f',
            "GIT_TERMINAL_PROMPT": "0",
        })
        environment.update(request.get("environment", {}))
        result = subprocess.run(request["argv"], env=environment, cwd="/",
                                capture_output=True, text=True, check=False, timeout=60)
        print(json.dumps({"returncode": result.returncode, "stdout": result.stdout,
                          "stderr": result.stderr, "stage": "command"}), flush=True)
finally:
    if child is not None:
        child.stdin.close()
        child.wait(timeout=10)
"""


class Agent:
    """Bounded stdin/JSON channel to a persistent runuser-owned process tree."""

    def __init__(self, cli, user, scope_id, executable, grant):
        self.cli, self.user, self.scope_id = cli, user, scope_id
        self.executable, self.grant = executable, grant
        self.closed = False
        # Configure fake and ready CA before startup, without operator secrets/proxies.
        environment = {
            "PATH": os.environ["PATH"],
            "HOME": pwd.getpwnam(user).pw_dir,
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
            "ASC_E2E_AGENT_CODE": AGENT_CODE,
            **grant["environment"],
        }
        self.process = subprocess.Popen(
            [
                "runuser",
                "-u",
                user,
                "--",
                str(executable),
                "-u",
                "-c",
                AGENT_CODE,
                cli,
                scope_id,
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            cwd="/",
            env=environment,
            start_new_session=True,
        )
        try:
            self.ready = self.receive()
            assert self.ready["uid"] == pwd.getpwnam(user).pw_uid != 0
            assert self.ready["child"]["uid"] == self.ready["uid"]
        except BaseException:
            self.close()
            raise

    def receive(self):
        readable, _, _ = select.select([self.process.stdout], [], [], 75)
        assert readable, "persistent Agent timed out waiting for a JSON response"
        line = self.process.stdout.readline()
        assert line, "persistent Agent exited before replying"
        return json.loads(line)

    def request(self, **request):
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()
        return self.receive()

    def execute(self, argv, *, refresh=True, snapshot=False, environment=None):
        result = self.request(
            argv=argv, refresh=refresh, snapshot=snapshot, environment=environment or {}
        )
        return subprocess.CompletedProcess(
            argv, result["returncode"], result["stdout"], result["stderr"]
        )

    def close(self):
        if self.closed:
            return
        self.closed = True
        with contextlib.suppress(BrokenPipeError):
            self.process.stdin.close()
        try:
            self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            # Only this harness's new session; never signal an operator process.
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=10)
        finally:
            self.process.stdout.close()
            self.process.stderr.close()


def in_domain(agent, script, *args, refresh=True, snapshot=False):
    """Executes a local script inside the discovered instance's descendant tree."""
    return agent.execute(
        ["sh", "-c", script, "sh", *args], refresh=refresh, snapshot=snapshot
    )


def resolve_flags():
    """curl argv pinning every upstream name locally without DNS."""
    pins = [f"{host}:443:{UPSTREAM}" for host in HOSTS]
    pins.append(f"api.github.com:80:{UPSTREAM}")
    return [argument for pin in pins for argument in ("--resolve", pin)]


def curl(agent, url, *, token=None, method="GET", snapshot=False, refresh=True):
    """Passes credentials as argv data inside the Agent, never shell source."""
    # The short client reads the fake in its own environment. Even an explicit
    # misuse value travels as JSON/env data, not interpolated shell source.
    source = (
        "import os,subprocess,sys; "
        "token=os.environ.get('ASC_E2E_TOKEN', os.environ['GITHUB_TOKEN']); "
        "p=subprocess.run(sys.argv[1:]+['-H','Authorization: Bearer '+token],check=False); "
        "sys.exit(p.returncode)"
    )
    result = agent.execute(
        [
            "python3",
            "-c",
            source,
            "curl",
            "-q",
            "--noproxy",
            "*",
            "-sS",
            "-m",
            "20",
            *resolve_flags(),
            "-X",
            method,
            url,
        ],
        snapshot=snapshot,
        refresh=refresh,
        environment={"ASC_E2E_TOKEN": token} if token is not None else None,
    )
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def fake_of(agent):
    return agent.request(probe=True)["environment"]["GITHUB_TOKEN"]


def wait_status(cli, ready, timeout=30):
    """Polls `gateway status` until `ready(status)`; the daemon may be restarting."""
    deadline = time.monotonic() + timeout
    while True:
        status = gateway(cli, "status", check=False)
        if isinstance(status, dict):
            assert set(status) == {"enabled", "ready", "scopes"}
            assert all(
                set(scope) == {"scopeId", "bindings"} for scope in status["scopes"]
            )
            if ready(status):
                return
        if time.monotonic() > deadline:
            raise AssertionError(f"gateway status not ready: {status}")
        time.sleep(0.2)


def wait_loaded(cli, *, after_generation=None):
    def ready(status):
        if not (status["enabled"] and status["ready"]):
            return False
        if after_generation is None:
            return True
        # Publication identity is internal diagnostics, not a public status field.
        try:
            health = json.loads((RUNTIME / "status" / "health.json").read_text())
        except (OSError, ValueError):
            return False
        generation = health.get("loadedGeneration")
        return isinstance(generation, int) and generation > after_generation

    wait_status(cli, ready)


class _Upstream(http.server.ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address, repositories, scheme, seen):
        super().__init__(address, _Handler)
        self.repositories = repositories
        self.scheme = scheme
        self.seen = seen


class _Handler(http.server.BaseHTTPRequestHandler):
    """Records every request; echoes REST credentials and serves Git over CGI."""

    protocol_version = "HTTP/1.1"

    def do_GET(self):
        self._handle()

    def do_POST(self):
        self._handle()

    def do_DELETE(self):
        self._handle()

    def _handle(self):
        host = self.headers.get("Host", "").partition(":")[0]
        authorization = self.headers.get("Authorization")
        body = self._body()
        self.server.seen.append(
            (self.server.scheme, host, self.command, self.path, authorization)
        )
        if host == "github.com":
            self._git(authorization, body)
            return
        payload = json.dumps({"host": host, "authorization": authorization})
        self._reply(200, {"Content-Type": "application/json"}, payload.encode())

    def _body(self):
        if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
            chunks = []
            while size := int(self.rfile.readline().split(b";")[0], 16):
                chunks.append(self.rfile.read(size))
                self.rfile.readline()
            self.rfile.readline()
            return b"".join(chunks)
        return self.rfile.read(int(self.headers.get("Content-Length", 0)))

    def _git(self, authorization, body):
        if authorization is None:
            # Git consults its credential helper only after a challenge.
            self._reply(401, {"WWW-Authenticate": 'Basic realm="GitHub"'}, b"")
            return
        path, _, query = self.path.partition("?")
        environment = {
            "PATH": os.environ["PATH"],
            "GIT_PROJECT_ROOT": str(self.server.repositories),
            "GIT_HTTP_EXPORT_ALL": "1",
            # A set REMOTE_USER enables receive-pack in git http-backend.
            "REMOTE_USER": "x-access-token",
            "REMOTE_ADDR": self.client_address[0],
            "REQUEST_METHOD": self.command,
            "PATH_INFO": path,
            "QUERY_STRING": query,
            "CONTENT_TYPE": self.headers.get("Content-Type", ""),
            "CONTENT_LENGTH": str(len(body)),
            "HTTP_CONTENT_ENCODING": self.headers.get("Content-Encoding", ""),
            "HTTP_GIT_PROTOCOL": self.headers.get("Git-Protocol", ""),
        }
        output = subprocess.run(
            ["git", "http-backend"],
            input=body,
            capture_output=True,
            env=environment,
            check=True,
        ).stdout
        separator = b"\r\n\r\n" if b"\r\n\r\n" in output else b"\n\n"
        head, _, payload = output.partition(separator)
        status, headers = 200, {}
        for line in head.decode().splitlines():
            name, _, value = line.partition(":")
            if name.lower() == "status":
                status = int(value.split()[0])
            elif name:
                headers[name] = value.strip()
        if authorization.startswith("Basic "):
            # Echo the actual upstream password without altering Git packet framing.
            headers["X-Token-Echo"] = (
                base64.b64decode(authorization[6:]).decode().partition(":")[2]
            )
        self._reply(status, headers, payload)

    def _reply(self, status, headers, payload):
        self.send_response(status)
        for name, value in headers.items():
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args):
        pass


def _issue_certificates(directory):
    """Creates a throwaway CA and one server certificate for every upstream host."""
    ec = ("-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes")
    command(
        "openssl", "req", "-x509", *ec, "-days", "1",
        "-subj", "/CN=asc-e2e-upstream-ca",
        "-keyout", f"{directory}/ca.key", "-out", f"{directory}/ca.pem",
        "-addext", "basicConstraints=critical,CA:TRUE",
        "-addext", "keyUsage=critical,keyCertSign",
    )  # fmt: skip
    command(
        "openssl", "req", *ec, "-subj", "/CN=api.github.com",
        "-keyout", f"{directory}/server.key", "-out", f"{directory}/server.csr",
    )  # fmt: skip
    extensions = Path(directory) / "server.ext"
    names = ",".join(f"DNS:{host}" for host in HOSTS)
    extensions.write_text(
        f"subjectAltName={names}\n"
        "extendedKeyUsage=serverAuth\n"
        "authorityKeyIdentifier=keyid\n"
    )
    command(
        "openssl", "x509", "-req", "-days", "1", "-in", f"{directory}/server.csr",
        "-CA", f"{directory}/ca.pem", "-CAkey", f"{directory}/ca.key",
        "-CAcreateserial", "-extfile", str(extensions),
        "-out", f"{directory}/server.pem",
    )  # fmt: skip


def _create_repository(root):
    """Creates the bare repository behind https://github.com/octocat/hello.git."""
    work = root / "seed"
    identity = ("-c", "user.name=seed", "-c", "user.email=seed@example.invalid")
    command("git", "init", "-q", "-b", "main", str(work))
    (work / "README").write_text("seed\n")
    command("git", "-C", str(work), "add", "README")
    command("git", *identity, "-C", str(work), "commit", "-q", "-m", "seed")
    bare = root / "repositories" / REPOSITORY
    bare.parent.mkdir(parents=True)
    command("git", "clone", "-q", "--bare", str(work), str(bare))
    return bare


def _unit_exec_start():
    """Returns the installed unit's ExecStart command as one line."""
    path = command("systemctl", "show", "-P", "FragmentPath", UNIT).stdout.strip()
    lines = Path(path).read_text().splitlines()
    start = next(i for i, line in enumerate(lines) if line.startswith("ExecStart="))
    parts = []
    for line in lines[start:]:
        parts.append(line.rstrip("\\").strip())
        if not line.endswith("\\"):
            break
    return " ".join(parts).removeprefix("ExecStart=")


@pytest.fixture
def host():
    user = os.environ.get("ASC_GATEWAY_E2E_USER")
    if not user:
        pytest.skip("set ASC_GATEWAY_E2E_USER to run the installed gateway acceptance")
    if os.geteuid() != 0 or Path("/proc/1/comm").read_text().strip() != "systemd":
        pytest.fail("requires root and PID 1 systemd")
    cli = shutil.which("agent-sec-cli")
    assert cli is not None, "agent-sec-cli must be installed or built and on PATH"
    status = gateway(cli, "status")
    # Global import/retirement must never modify operator Scopes or an active key.
    assert not status["enabled"] and not status["scopes"], "requires an unused gateway"
    assert gateway(cli, "key", "list") == {
        "pairids": []
    }, "requires a host without active managed credentials"
    assert pwd.getpwnam(user).pw_uid != 0
    return cli, user


@pytest.fixture
def upstream(host):
    """Serves the upstream hosts on UPSTREAM ports 443 and 80; yields its state.

    While active, the gateway unit trusts only the throwaway CA upstream, so its
    certificate verification stays on.
    """
    directory = Path(tempfile.mkdtemp(prefix="asc-gw-upstream-", dir="/run"))
    directory.chmod(0o755)
    _issue_certificates(directory)
    (directory / "ca.pem").chmod(0o644)
    bare = _create_repository(directory)
    command("ip", "addr", "add", f"{UPSTREAM}/32", "dev", "lo")
    seen = []
    servers = []
    try:
        repositories = directory / "repositories"
        tls = _Upstream((UPSTREAM, 443), repositories, "https", seen)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(directory / "server.pem", directory / "server.key")
        tls.socket = context.wrap_socket(tls.socket, server_side=True)
        servers.append(tls)
        servers.append(_Upstream((UPSTREAM, 80), repositories, "http", seen))
        for server in servers:
            threading.Thread(target=server.serve_forever, daemon=True).start()
        DROP_IN.parent.mkdir(parents=True, exist_ok=True)
        DROP_IN.write_text(
            "[Service]\nExecStart=\nExecStart="
            f"{_unit_exec_start()} --set ssl_verify_upstream_trusted_ca={directory}/ca.pem\n"
        )
        command("systemctl", "daemon-reload")
        yield {"seen": seen, "ca": directory / "ca.pem", "bare": bare}
    finally:
        DROP_IN.unlink(missing_ok=True)
        command("systemctl", "daemon-reload", check=False)
        for server in servers:
            server.shutdown()
            server.server_close()
        command("ip", "addr", "del", f"{UPSTREAM}/32", "dev", "lo", check=False)
        shutil.rmtree(directory, ignore_errors=True)


def public_ca(cli, user, check=True):
    result = command("runuser", "-u", user, "--", cli, "gateway", "ca", check=check)
    return json.loads(result.stdout) if result.returncode == 0 else result


def agent_environment(ca, fake):
    """Only harness-generated fake values enter the Agent; CA has no credential data."""
    assert set(ca) == {"caBundlePath"}
    assert Path(ca["caBundlePath"]).is_file()
    return {
        "environment": {
            "GH_TOKEN": fake,
            "GITHUB_TOKEN": fake,
            **{
                variable: ca["caBundlePath"]
                for variable in (
                    "SSL_CERT_FILE",
                    "REQUESTS_CA_BUNDLE",
                    "CURL_CA_BUNDLE",
                    "GIT_SSL_CAINFO",
                )
            },
        }
    }


def render_policy(template, real_file, fake_file):
    """Render raw Cedar with protected daemon-host absolute token-file paths."""
    source = template.read_text()
    for annotation, original, path in (
        ("real_token_file", "/secure/github-real", real_file),
        ("fake_token_file", "/secure/github-fake", fake_file),
    ):
        assert path.is_absolute() and path == path.resolve(strict=True)
        metadata = path.stat()
        assert metadata.st_uid == 0 and metadata.st_mode & 0o777 == 0o600
        assert metadata.st_nlink == 1 and path.is_file()
        marker = f"@{annotation}({json.dumps(original)})"
        assert marker in source
        source = source.replace(marker, f"@{annotation}({json.dumps(str(path))})")
    return source


def pair_id(policy):
    bindings = policy["credentialBindings"]
    assert bindings and all(set(item) == {"ruleId", "pairId"} for item in bindings)
    identifiers = {item["pairId"] for item in bindings}
    assert len(identifiers) == 1
    return identifiers.pop()


def published_snapshot():
    """Inspect the actual Rust-to-addon contract, not a copied test plan."""
    manifest = json.loads((RUNTIME / "active.json").read_text())
    path = RUNTIME / manifest["secrets"]
    snapshot = json.loads(path.read_text())
    assert path.stat().st_uid == 0 and path.stat().st_mode & 0o777 == 0o640
    assert snapshot["version"] == 2
    assert snapshot["generation"] == manifest["generation"]
    assert all(
        set(entry) == {"fake", "credential", "scopeIds", "real"}
        for entry in snapshot["entries"]
    )
    assert all(set(entry) == {"fake", "credential"} for entry in snapshot["retired"])
    for listener in manifest["listeners"]:
        plan = json.loads((RUNTIME / listener["plan"]).read_text())
        assert plan["format"] == "agent-sec-gateway-plan/v2"
        assert plan["scopeId"] == listener["scopeId"]
        assert all(
            rule["header"] == "Authorization"
            and rule["format"] in ("auto", "basic")
            and rule["responseRewrite"] is True
            for rule in plan["rules"]
        )
        encoded = json.dumps(plan)
        assert all(
            entry["fake"] not in encoded and entry["real"] not in encoded
            for entry in snapshot["entries"]
        )
    return path, snapshot


def wait_bindings(cli, scope_id, pids, timeout=45):
    """Waits for real discovery/reconciliation, not a manually attached cgroup."""
    deadline = time.monotonic() + timeout
    while True:
        listing = cli_json(cli, "binding", "list", "--limit", "1000")
        bindings = [
            item
            for item in listing["items"]
            if item["spec"]["scope"]["scopeId"] == scope_id
        ]
        ready = {
            item["spec"]["scope"]["process"]["pid"]
            for item in bindings
            if item["status"]["phase"] == "READY"
        }
        if ready == set(pids) and len(bindings) == len(pids):
            return bindings
        assert (
            time.monotonic() < deadline
        ), f"Scope discovery/reconciliation stalled: {bindings}"
        time.sleep(0.2)


def delete_scope(cli, scope_id):
    deadline = time.monotonic() + 45
    while True:
        result = cli_json(cli, "scope", "delete", "--scope-id", scope_id)
        if result["completed"]:
            break
        assert time.monotonic() < deadline, f"Scope cleanup stalled: {scope_id}"
        time.sleep(0.2)
    wait_status(
        cli,
        lambda status: scope_id not in {entry["scopeId"] for entry in status["scopes"]},
    )


@contextlib.contextmanager
def protected_token(cli, user, agents=(AGENT,), foreign_user=None, shared_pair=False):
    """Register private test files through root Policy admission before discovery."""
    template = Path("/usr/share/anolisa/sec-core/gateway/github.cedar")
    assert template.is_file(), f"installed GitHub policy asset is missing: {template}"
    alphabet = string.ascii_letters + string.digits
    real = "ghp_" + "".join(secrets.choice(alphabet) for _ in range(36))
    domains, scopes, policies = {}, {}, []
    with tempfile.TemporaryDirectory(prefix="asc-gw-workers-", dir="/run") as directory:
        root = Path(directory).resolve(strict=True)
        root.chmod(0o755)
        secret_file = root / "github-real"
        secret_file.write_text(real + "\n")
        secret_file.chmod(0o600)
        try:
            names = (*agents, OTHER_AGENT) if foreign_user else agents
            preparations = {}
            for name in names:
                owner = foreign_user if foreign_user and name == OTHER_AGENT else user
                if shared_pair and policies:
                    policy, fake_file, envelope = preparations[names[0]][2:]
                else:
                    fake_file = root / f"{name}-fake"
                    generated = command(
                        cli,
                        "key",
                        "generate-fake",
                        "--real-token-file",
                        str(secret_file),
                        "--output",
                        str(fake_file),
                    )
                    fake = fake_file.read_text().rstrip("\n")
                    assert real not in generated.stdout + generated.stderr
                    assert fake not in generated.stdout + generated.stderr
                    assert len(fake) == len(real) and fake.startswith("ghp_")
                    assert sum(a != b for a, b in zip(real, fake)) * 5 >= len(real) * 4
                    envelope = root / f"{name}-policy.cedar"
                    envelope.write_text(render_policy(template, secret_file, fake_file))
                    envelope.chmod(0o644)
                    policy = cli_json(
                        cli,
                        "policy",
                        "create",
                        "--name",
                        f"asc-gateway-e2e-{name}",
                        "--file",
                        str(envelope),
                        "--schema-revision",
                        "2",
                    )
                    policies.append(policy)
                    encoded = json.dumps(policy)
                    assert real not in encoded and fake not in encoded
                    assert pair_id(policy) in gateway(cli, "key", "list")["pairids"]
                executable = root / name
                shutil.copyfile(Path(shutil.which("python3")).resolve(), executable)
                executable.chmod(0o755)
                scope = cli_json(
                    cli,
                    "scope",
                    "create",
                    "--executable",
                    str(executable),
                    "--owner-uid",
                    str(pwd.getpwnam(owner).pw_uid),
                    "--policy-id",
                    policy["policyId"],
                    "--policy-revision",
                    str(policy["revision"]),
                )
                scopes[name] = scope["scopeId"]
                preparations[name] = (owner, executable, policy, fake_file, envelope)
                assert scope["policySnapshots"] == [policy]
                denied = public_ca(cli, owner, check=False)
                assert denied.returncode != 0
                assert "unavailable" in (denied.stdout + denied.stderr)
                wait_bindings(cli, scope["scopeId"], [])
            assert gateway(cli, "enable") == {}
            wait_loaded(cli)
            ca = gateway(cli, "ca")
            for name, preparation in preparations.items():
                owner, executable, policy, fake_file, envelope = preparation
                scope_id = scopes[name]
                assert public_ca(cli, owner) == ca
                fake = fake_file.read_text().rstrip("\n")
                grant = agent_environment(ca, fake)
                wait_bindings(cli, scope_id, [])
                domain = Agent(cli, owner, scope_id, executable, grant)
                domain.policy, domain.envelope = policy, envelope
                domain.real_file, domain.fake_file = secret_file, fake_file
                domains[name] = domain
                wait_bindings(cli, scope_id, [domain.ready["pid"]])
                for snapshot in (False, True):
                    probe = domain.request(probe=True, refresh=False, snapshot=snapshot)
                    assert "/agentsec.slice/" in probe["cgroup"]
                    assert probe["descendantCgroup"] == probe["cgroup"]
            _, snapshot = published_snapshot()
            expected = {}
            for domain in domains.values():
                identity = pair_id(domain.policy)
                entry = expected.setdefault(
                    identity,
                    {
                        "credential": identity,
                        "real": real,
                        "fake": domain.fake_file.read_text().rstrip("\n"),
                        "scopeIds": [],
                    },
                )
                entry["scopeIds"].append(domain.scope_id)
            for entry in expected.values():
                entry["scopeIds"].sort()
            assert sorted(snapshot["entries"], key=lambda item: item["credential"]) == [
                expected[identity] for identity in sorted(expected)
            ]
            yield real, domains
        finally:
            # Cleanup every resource even if an earlier cleanup fails.
            with contextlib.ExitStack() as cleanup:
                cleanup.callback(gateway, cli, "disable")
                for policy in policies:
                    cleanup.callback(gateway, cli, "key", "remove", pair_id(policy))
                    cleanup.callback(
                        cli_json,
                        cli,
                        "policy",
                        "delete",
                        "--policy-id",
                        policy["policyId"],
                        "--revision",
                        str(policy["revision"]),
                    )
                for scope_id in scopes.values():
                    cleanup.callback(delete_scope, cli, scope_id)
                for domain in domains.values():
                    cleanup.callback(domain.close)
    assert gateway(cli, "status")["scopes"] == []
    assert sorted(path.name for path in RUNTIME.iterdir()) == ["ca-bundle.pem"]
    assert (RUNTIME / "ca-bundle.pem").is_file()
    assert command("nft", "list", "table", "inet", TABLE, check=False).returncode != 0
    logs = command("journalctl", "--no-pager", "-u", UNIT, "-u", DAEMON_UNIT).stdout
    assert real not in logs


def test_agent_sees_only_a_fake_inside_its_domain(host, upstream):
    cli, user = host
    nobody = pwd.getpwnam("nobody")
    assert nobody.pw_uid not in (0, pwd.getpwnam(user).pw_uid)
    with protected_token(cli, user, foreign_user=nobody.pw_name) as (real, domains):
        agent, foreign = domains[AGENT], domains[OTHER_AGENT]
        ca = gateway(cli, "ca")
        assert set(ca) == {"caBundlePath"}
        assert public_ca(cli, nobody.pw_name) == ca
        assert real not in json.dumps(ca) and fake_of(agent) not in json.dumps(ca)
        for snapshot in (False, True):
            probe = agent.request(probe=True, refresh=False, snapshot=snapshot)
            assert "/agentsec.slice/" in probe["cgroup"]
            assert probe["descendantCgroup"] == probe["cgroup"]
            environment = probe["environment"]
            fake = environment["GITHUB_TOKEN"]
            assert (
                fake
                == environment["GH_TOKEN"]
                == agent.fake_file.read_text().rstrip("\n")
            )
            assert fake != real and fake.startswith("ghp_") and len(fake) == len(real)
            assert real not in json.dumps(environment)
            for variable in (
                "SSL_CERT_FILE",
                "REQUESTS_CA_BUNDLE",
                "CURL_CA_BUNDLE",
                "GIT_SSL_CAINFO",
            ):
                assert environment[variable] == ca["caBundlePath"]
                assert Path(environment[variable]).is_file()
        before = agent.request(probe=True, refresh=False)["cgroup"]
        own = agent.execute([cli, "gateway", "ca"], refresh=False)
        assert own.returncode == 0, own.stderr
        assert json.loads(own.stdout) == ca
        assert agent.request(probe=True, refresh=False)["cgroup"] == before
        # Public CA queries neither require Scope ownership nor move the caller.
        outside = command(
            "runuser",
            "-u",
            user,
            "--",
            "python3",
            "-c",
            "import subprocess,sys; from pathlib import Path; "
            "before=Path('/proc/self/cgroup').read_text(); "
            "p=subprocess.run(sys.argv[1:],check=False); "
            "assert before==Path('/proc/self/cgroup').read_text(); sys.exit(p.returncode)",
            cli,
            "gateway",
            "ca",
        )
        assert json.loads(outside.stdout) == ca
        rpc_source = (
            "import json,socket,sys; "
            "s=socket.socket(socket.AF_UNIX); s.settimeout(10); "
            "s.connect('/run/agent-sec-core/daemon.sock'); "
            "s.sendall((sys.argv[1]+'\\n').encode()); "
            "print(s.makefile().readline()); s.close()"
        )
        for method, params, code in (
            ("gateway.credentials", {"scopeId": agent.scope_id}, "unknown_method"),
            ("gateway.key.add", {"secretFile": str(agent.real_file)}, "unknown_method"),
            ("gateway.ca", {"scopeId": agent.scope_id}, "invalid_argument"),
        ):
            request = json.dumps({"method": method, "params": params})
            response = agent.execute(
                ["python3", "-c", rpc_source, request], refresh=False
            )
            assert response.returncode == 0, response.stderr
            assert json.loads(response.stdout)["error"]["code"] == code
        snapshot_path, _ = published_snapshot()
        for path in (agent.real_file, agent.fake_file, snapshot_path):
            denied = agent.execute(
                [
                    "python3",
                    "-c",
                    "from pathlib import Path; import sys; Path(sys.argv[1]).read_bytes()",
                    str(path),
                ],
                refresh=False,
            )
            assert denied.returncode != 0 and "PermissionError" in denied.stderr
        for args in (
            ["gateway", "enable"],
            ["gateway", "disable"],
            ["gateway", "key", "list"],
            ["gateway", "key", "remove", pair_id(agent.policy)],
            [
                "policy",
                "create",
                "--name",
                "non-root-credential",
                "--file",
                str(agent.envelope),
                "--schema-revision",
                "2",
            ],
            ["scope", "delete", "--scope-id", agent.scope_id],
        ):
            denied = agent.execute([cli, *args], refresh=False)
            assert denied.returncode != 0, args
            assert "permission_denied" in (denied.stdout + denied.stderr), args
        for caller, target in ((agent, foreign), (foreign, agent)):
            public = caller.execute([cli, "gateway", "ca"], refresh=False)
            assert public.returncode == 0 and json.loads(public.stdout) == ca
            denied = caller.execute(
                [cli, "scope", "delete", "--scope-id", target.scope_id], refresh=False
            )
            assert denied.returncode != 0
            assert "permission_denied" in (denied.stdout + denied.stderr)
            status = caller.execute([cli, "gateway", "status"], refresh=False)
            assert status.returncode == 0, status.stderr
            assert {
                entry["scopeId"] for entry in json.loads(status.stdout)["scopes"]
            } == {caller.scope_id}
        assert {entry["scopeId"] for entry in gateway(cli, "status")["scopes"]} == {
            agent.scope_id,
            foreign.scope_id,
        }


def test_rest_requests_carry_the_real_token_upstream(host, upstream):
    cli, user = host
    seen = upstream["seen"]
    with protected_token(cli, user) as (real, domains):
        agent = domains[AGENT]
        fake = fake_of(agent)
        assert pwd.getpwnam("nobody").pw_uid != pwd.getpwnam(user).pw_uid
        # An authenticated non-root peer needs no credential Scope to read public CA.
        assert public_ca(cli, "nobody") == gateway(cli, "ca")
        for snapshot in (False, True):
            echoed = curl(agent, "https://api.github.com/user", snapshot=snapshot)
            assert seen[-1] == (
                "https",
                "api.github.com",
                "GET",
                "/user",
                f"Bearer {real}",
            )
            # The existing descendant snapshot inherits the discovered root's grant.
            assert echoed == {
                "host": "api.github.com",
                "authorization": f"Bearer {fake}",
            }

        anonymous = agent.execute(
            [
                "curl",
                "-q",
                "--noproxy",
                "*",
                "-sS",
                "-m",
                "20",
                *resolve_flags(),
                "https://api.github.com/zen",
            ]
        )
        assert anonymous.returncode == 0, anonymous.stderr
        assert json.loads(anonymous.stdout)["authorization"] is None
        assert seen[-1] == ("https", "api.github.com", "GET", "/zen", None)
    assert not any(fake in str(record) for record in seen)


def test_git_clone_fetch_and_push_use_the_real_token_upstream(host, upstream):
    cli, user = host
    seen = upstream["seen"]
    with protected_token(cli, user) as (real, domains):
        agent = domains[AGENT]
        fake = fake_of(agent)
        script = (
            'set -e; work=$(mktemp -d); trap \'rm -rf "$work"\' EXIT; cd "$work";'
            f" git {GIT_RESOLVE} clone -q https://github.com/{REPOSITORY} repo; cd repo;"
            f" git {GIT_RESOLVE} fetch -q origin main;"
            ' test "$(git show FETCH_HEAD:README)" = seed;'
            " echo pushed > CHANGE; git add CHANGE;"
            " git -c user.name=agent -c user.email=agent@example.invalid"
            " commit -q -m 'pushed through the gateway';"
            f" git {GIT_RESOLVE} push -q origin main"
        )
        result = in_domain(agent, script)
        assert result.returncode == 0, result.stderr
        refs = in_domain(
            agent,
            'exec curl -q --noproxy "*" -sS -m 20 '
            f"--resolve github.com:443:{UPSTREAM} "
            '--user "x-access-token:$GITHUB_TOKEN" --dump-header - --output /dev/null '
            f'"https://github.com/{REPOSITORY}/info/refs?service=git-upload-pack"',
        )
        assert refs.returncode == 0, refs.stderr
        headers = dict(
            (name.lower(), value.strip())
            for name, _, value in (
                line.partition(":") for line in refs.stdout.splitlines()
            )
            if value
        )
        assert headers["x-token-echo"] == fake
        assert real not in refs.stdout
    subject = command(
        "git", "--git-dir", str(upstream["bare"]), "log", "-1", "--format=%s", "main"
    ).stdout.strip()
    assert subject == "pushed through the gateway"
    basic = "Basic " + base64.b64encode(f"x-access-token:{real}".encode()).decode()
    github = [record for record in seen if record[1] == "github.com"]
    authenticated = {
        (method, path.partition("?")[0].rsplit("/", 1)[-1])
        for _, _, method, path, authorization in github
        if authorization == basic
    }
    assert {
        ("GET", "refs"),
        ("POST", "git-upload-pack"),
        ("POST", "git-receive-pack"),
    } <= authenticated
    # Git's unauthenticated probes aside, every request carried the real token.
    assert {authorization for *_, authorization in github} == {None, basic}
    assert not any(fake in str(record) for record in seen)


def test_misused_credentials_are_rejected_before_the_upstream(host, upstream):
    cli, user = host
    seen = upstream["seen"]
    with protected_token(cli, user, agents=(AGENT, OTHER_AGENT)) as (real, domains):
        agent, other = domains[AGENT], domains[OTHER_AGENT]
        fake = fake_of(agent)
        api = "https://api.github.com/user"
        cases = [
            (curl(agent, "https://example.com/"), "credential_not_allowed"),
            (
                curl(agent, f"https://github.com/{REPOSITORY}", method="DELETE"),
                "credential_not_allowed",
            ),
            (curl(agent, "http://api.github.com/user"), "plaintext_credential"),
            (curl(agent, api, token=real), "real_credential_exposed"),
            (curl(other, api, token=fake), "subject_mismatch"),
        ]
        # Distinct explicit pairs may share a real, but not each other's Scope authority.
        delete_scope(cli, agent.scope_id)
        cases.append((curl(other, api, token=fake), "retired_credential"))
        public = agent.request(argv=[cli, "gateway", "ca"])
        assert public["returncode"] == 0
        assert json.loads(public["stdout"]) == gateway(cli, "ca")
        for snapshot in (False, True):
            probe = agent.request(probe=True, refresh=False, snapshot=snapshot)
            expected = agent.ready["child"]["pid"] if snapshot else agent.ready["pid"]
            assert (
                probe["pid"] == expected
            )  # Deleting a Scope must not kill the worker.
        for body, code in cases:
            assert body == {"error": code, "source": "agent-sec-gateway"}
        assert seen == []  # Every rejected request stopped before the upstream.
        assert fake_of(other) != fake
        echoed = curl(other, api)
        assert seen[-1][4] == f"Bearer {real}"
        assert echoed["authorization"] == f"Bearer {fake_of(other)}"
        assert gateway(cli, "key", "list") == {"pairids": [pair_id(other.policy)]}


def test_traffic_outside_the_domain_bypasses_the_gateway(host, upstream):
    cli, user = host
    seen = upstream["seen"]
    with protected_token(cli, user) as (real, domains):
        agent = domains[AGENT]
        fake = fake_of(agent)
        direct = command(
            "runuser", "-u", user, "--",
            "curl", "-q", "--noproxy", "*", "-sS", "-m", "20", "--cacert", str(upstream["ca"]),
            *resolve_flags(),
            "-H", f"Authorization: Bearer {fake}", "https://api.github.com/user",
        )  # fmt: skip
        # Unrouted, the fake reaches the upstream untouched and is useless there.
        assert json.loads(direct.stdout)["authorization"] == f"Bearer {fake}"
        assert seen[-1][4] == f"Bearer {fake}"

        send = f"exec bash -c 'echo quic > /dev/udp/{UPSTREAM}/443'"
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
            receiver.bind((UPSTREAM, 443))
            receiver.settimeout(1)
            in_domain(agent, send)
            # UDP/443 is deliberately outside the MVP's interception boundary.
            assert receiver.recv(16) == b"quic\n"
            command("runuser", "-u", user, "--", "sh", "-c", send)
            assert receiver.recv(16) == b"quic\n"
    assert real not in str(seen)


def test_conflicting_import_and_global_retirement_apply_to_every_scope(host, upstream):
    cli, user = host
    seen = upstream["seen"]
    with protected_token(cli, user, agents=(AGENT, OTHER_AGENT), shared_pair=True) as (
        real,
        domains,
    ):
        agent = domains[AGENT]
        fake = fake_of(agent)
        identity = pair_id(agent.policy)
        assert {fake_of(domain) for domain in domains.values()} == {fake}
        policies = []
        try:
            # Pair identity follows values, not authoring file paths.
            root = agent.envelope.parent
            copied_real, copied_fake = root / "copied-real", root / "copied-fake"
            for source, target in (
                (agent.real_file, copied_real),
                (agent.fake_file, copied_fake),
            ):
                shutil.copyfile(source, target)
                target.chmod(0o600)
            duplicate_envelope = root / "duplicate-policy.cedar"
            template = Path("/usr/share/anolisa/sec-core/gateway/github.cedar")
            duplicate_envelope.write_text(
                render_policy(template, copied_real, copied_fake)
            )
            duplicate = cli_json(
                cli,
                "policy",
                "create",
                "--name",
                "same-pair",
                "--file",
                str(duplicate_envelope),
                "--schema-revision",
                "2",
            )
            policies.append(duplicate)
            assert pair_id(duplicate) == identity
            assert gateway(cli, "key", "list") == {"pairids": [identity]}
            replacement = "ghp_" + "".join(
                "A" if character != "A" else "B" for character in fake[4:]
            )
            assert replacement != real
            agent.real_file.write_text(replacement + "\n")
            # Exact source replay consumes its saved binding, not mutated authoring files.
            replay = cli_json(
                cli,
                "policy",
                "create",
                "--name",
                "same-source-replay",
                "--file",
                str(agent.envelope),
                "--schema-revision",
                "2",
            )
            policies.append(replay)
            assert replay["credentialBindings"] == agent.policy["credentialBindings"]
            assert pair_id(replay) == identity
            assert gateway(cli, "key", "list") == {"pairids": [identity]}
            # A new source forces file import; the same fake cannot bind a new real.
            conflicting_real = root / "conflicting-real"
            conflicting_real.write_text(replacement + "\n")
            conflicting_real.chmod(0o600)
            conflict_source = root / "conflicting-policy.cedar"
            conflict_source.write_text(
                render_policy(template, conflicting_real, agent.fake_file)
            )
            conflict = cli_json(
                cli,
                "policy",
                "create",
                "--name",
                "conflicting-pair",
                "--file",
                str(conflict_source),
                "--schema-revision",
                "2",
                check=False,
            )
            assert conflict.returncode != 0 and conflict.stdout == ""
            assert gateway(cli, "key", "list") == {"pairids": [identity]}
            assert real not in conflict.stderr and replacement not in conflict.stderr
            # File mutation cannot alter execution or restart recovery.
            for unit in (None, DAEMON_UNIT, UNIT):
                if unit is not None:
                    command("systemctl", "restart", unit)
                    wait_loaded(cli)
                for domain in domains.values():
                    wait_bindings(cli, domain.scope_id, [domain.ready["pid"]])
                    assert fake_of(domain) == fake
                    echoed = curl(domain, "https://api.github.com/user")
                    assert seen[-1][4] == f"Bearer {real}"
                    assert echoed["authorization"] == f"Bearer {fake}"
            delete_scope(cli, agent.scope_id)
            survivor = domains[OTHER_AGENT]
            assert gateway(cli, "key", "list") == {"pairids": [identity]}
            assert (
                curl(survivor, "https://api.github.com/user")["authorization"]
                == f"Bearer {fake}"
            )
            assert seen[-1][4] == f"Bearer {real}"
            assert gateway(cli, "key", "remove", identity) == {"pairid": identity}
            assert gateway(cli, "key", "list") == {"pairids": []}
            for domain in domains.values():
                # Retirement withdraws routing; retained fakes never become real values.
                before = len(seen)
                direct = domain.execute(
                    [
                        "curl",
                        "-q",
                        "--noproxy",
                        "*",
                        "-sS",
                        "-m",
                        "20",
                        "--cacert",
                        str(upstream["ca"]),
                        *resolve_flags(),
                        "-H",
                        f"Authorization: Bearer {fake}",
                        "https://api.github.com/user",
                    ],
                    refresh=False,
                )
                assert direct.returncode == 0, direct.stderr
                assert seen[before:] == [
                    ("https", "api.github.com", "GET", "/user", f"Bearer {fake}")
                ]
                assert json.loads(direct.stdout)["authorization"] == f"Bearer {fake}"
            agent.real_file.write_text(real + "\n")
            retired = cli_json(
                cli,
                "policy",
                "create",
                "--name",
                "cannot-resurrect",
                "--file",
                str(agent.envelope),
                "--schema-revision",
                "2",
                check=False,
            )
            assert retired.returncode != 0 and retired.stdout == ""
            assert gateway(cli, "key", "list") == {"pairids": []}
        finally:
            with contextlib.ExitStack() as cleanup:
                for policy in policies:
                    cleanup.callback(
                        cli_json,
                        cli,
                        "policy",
                        "delete",
                        "--policy-id",
                        policy["policyId"],
                        "--revision",
                        str(policy["revision"]),
                    )
        logs = command("journalctl", "--no-pager", "-u", UNIT, "-u", DAEMON_UNIT).stdout
        assert replacement not in logs


def test_multiple_instances_and_restarts_retain_the_scope_fake(host, upstream):
    cli, user = host
    with protected_token(cli, user) as (real, domains):
        first = domains[AGENT]
        fake = fake_of(first)
        second = Agent(
            cli,
            user,
            first.scope_id,
            first.executable,
            agent_environment(public_ca(cli, user), fake),
        )
        try:
            wait_bindings(
                cli, first.scope_id, [first.ready["pid"], second.ready["pid"]]
            )
            for agent in (first, second):
                assert fake_of(agent) == fake
                assert (
                    curl(agent, "https://api.github.com/user")["authorization"]
                    == f"Bearer {fake}"
                )
                assert upstream["seen"][-1][4] == f"Bearer {real}"
            old_pid = first.ready["pid"]
            first.close()
            wait_bindings(cli, first.scope_id, [second.ready["pid"]])
            assert fake_of(second) == fake
            assert (
                curl(second, "https://api.github.com/user")["authorization"]
                == f"Bearer {fake}"
            )
            restarted = Agent(
                cli,
                user,
                first.scope_id,
                first.executable,
                agent_environment(public_ca(cli, user), fake),
            )
            domains[AGENT] = restarted
            assert restarted.ready["pid"] != old_pid
            wait_bindings(
                cli, first.scope_id, [second.ready["pid"], restarted.ready["pid"]]
            )
            assert fake_of(restarted) == fake
            assert (
                curl(restarted, "https://api.github.com/user")["authorization"]
                == f"Bearer {fake}"
            )
            assert upstream["seen"][-1][4] == f"Bearer {real}"
        finally:
            second.close()


def test_empty_scope_deletion_retires_the_last_shared_pair(host, upstream):
    cli, user = host
    with protected_token(cli, user, agents=(AGENT, OTHER_AGENT)) as (real, domains):
        agent, other = domains[AGENT], domains[OTHER_AGENT]
        policy = agent.policy
        fake, identity = fake_of(agent), pair_id(policy)
        with tempfile.TemporaryDirectory(
            prefix="asc-gw-empty-", dir="/run"
        ) as directory:
            empty = cli_json(
                cli,
                "scope",
                "create",
                "--executable",
                str(Path(directory) / "never-started"),
                "--owner-uid",
                str(pwd.getpwnam(user).pw_uid),
                "--policy-id",
                policy["policyId"],
                "--policy-revision",
                str(policy["revision"]),
            )
            try:
                scope_id = empty["scopeId"]
                wait_bindings(cli, scope_id, [])
                assert empty["policySnapshots"] == [policy]
                assert public_ca(cli, user) == gateway(cli, "ca")
                agent.close()
                wait_bindings(cli, agent.scope_id, [])
                # Zero instances do not revoke; the final Active Scope owns retirement.
                assert identity in gateway(cli, "key", "list")["pairids"]
                delete_scope(cli, agent.scope_id)
                assert identity in gateway(cli, "key", "list")["pairids"]
                before = len(upstream["seen"])
                assert curl(other, "https://api.github.com/user", token=fake) == {
                    "error": "subject_mismatch",
                    "source": "agent-sec-gateway",
                }
                assert len(upstream["seen"]) == before
                delete_scope(cli, scope_id)
                assert gateway(cli, "key", "list") == {
                    "pairids": [pair_id(other.policy)]
                }
                for unit in (None, DAEMON_UNIT, UNIT):
                    if unit is not None:
                        command("systemctl", "restart", unit)
                        wait_loaded(cli)
                        wait_bindings(cli, other.scope_id, [other.ready["pid"]])
                    _, snapshot = published_snapshot()
                    assert {"fake": fake, "credential": identity} in snapshot["retired"]
                    assert all(
                        entry["credential"] != identity for entry in snapshot["entries"]
                    )
                    before = len(upstream["seen"])
                    assert curl(other, "https://api.github.com/user", token=fake) == {
                        "error": "retired_credential",
                        "source": "agent-sec-gateway",
                    }
                    assert len(upstream["seen"]) == before
                    assert (
                        curl(other, "https://api.github.com/user")["authorization"]
                        == f"Bearer {fake_of(other)}"
                    )
                    assert upstream["seen"][-1][4] == f"Bearer {real}"
                rejected = cli_json(
                    cli,
                    "policy",
                    "create",
                    "--name",
                    "last-scope-retired",
                    "--file",
                    str(agent.envelope),
                    "--schema-revision",
                    "2",
                    check=False,
                )
                assert rejected.returncode != 0 and rejected.stdout == ""
                assert identity not in gateway(cli, "key", "list")["pairids"]
            finally:
                delete_scope(cli, empty["scopeId"])


def test_credential_survives_daemon_and_gateway_restarts(host, upstream):
    cli, user = host
    seen = upstream["seen"]
    with protected_token(cli, user) as (real, domains):
        agent = domains[AGENT]
        fake = fake_of(agent)
        ca = gateway(cli, "ca")
        try:
            command("systemctl", "stop", UNIT)
            wait_status(cli, lambda status: not status["ready"])
            # An existing public bundle is not proof of live unit/generation readiness.
            assert Path(ca["caBundlePath"]).is_file()
            for denied in (
                gateway(cli, "ca", check=False),
                public_ca(cli, user, check=False),
            ):
                assert denied.returncode != 0
                assert "unavailable" in denied.stdout + denied.stderr
            denied = agent.request(argv=["true"])
            assert denied["returncode"] != 0 and denied["stage"] == "ca"
        finally:
            command("systemctl", "start", UNIT)
            wait_loaded(cli)
        # Recovery consumes registered pair snapshots, never rereads authoring files.
        agent.real_file.unlink()
        agent.fake_file.unlink()
        for unit in (DAEMON_UNIT, UNIT):
            previous = json.loads((RUNTIME / "status" / "health.json").read_text())[
                "loadedGeneration"
            ]
            command("systemctl", "restart", unit)
            # Daemon recovery must publish anew, not merely expose the proxy's old health.
            wait_loaded(cli, after_generation=previous if unit == DAEMON_UNIT else None)
            wait_bindings(cli, agent.scope_id, [agent.ready["pid"]])
            assert public_ca(cli, user) == ca
            assert fake_of(agent) == fake
            echoed = curl(agent, "https://api.github.com/user")
            assert seen[-1][4] == f"Bearer {real}", unit
            assert echoed["authorization"] == f"Bearer {fake}"
