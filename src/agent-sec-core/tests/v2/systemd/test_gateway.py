"""Secret gateway acceptance on a host with the gateway installed; run explicitly as root.

Everything stays on the host: a local upstream on a non-loopback address plays
api.github.com, github.com (backed by `git http-backend`) and example.com,
clients pin those names to it without DNS, and the gateway unit temporarily
trusts only a throwaway CA for its upstream connections. Tokens are generated
dummies. The tests add an address to `lo`, a runtime drop-in for the gateway
unit, and restart agent-sec-core.service, so they run only when
ASC_GATEWAY_E2E_USER names the non-root user that owns the test Agent domains.
The gateway must be disabled with no Agent domains configured beforehand.
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


def gateway(cli, *args, check=True):
    result = command(cli, "gateway", *args, check=check)
    return json.loads(result.stdout) if result.returncode == 0 else result


# Both persistent workers exist before root registers the parent's PID; only
# their own children execute commands.
AGENT_CODE = r"""
import json
import os
import subprocess
import sys
from pathlib import Path

cli, agent = sys.argv[1:3]
child = None
if len(sys.argv) == 3:
    child = subprocess.Popen(
        [sys.executable, "-u", "-c", os.environ["ASC_E2E_AGENT_CODE"], cli, agent, "worker"],
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
                [cli, "gateway", "credentials", "--agent", agent],
                capture_output=True, text=True, check=False, timeout=30,
            )
            if grant.returncode:
                print(json.dumps({"returncode": grant.returncode,
                                  "stdout": grant.stdout, "stderr": grant.stderr,
                                  "stage": "credentials"}), flush=True)
                continue
            environment.update(json.loads(grant.stdout)["environment"])
        if request.get("probe"):
            descendant = subprocess.run(
                [sys.executable, "-c",
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

    def __init__(self, cli, user, name):
        self.cli, self.name = cli, name
        # Do not inherit operator credentials, proxies, or Git configuration.
        environment = {
            "PATH": os.environ["PATH"],
            "HOME": pwd.getpwnam(user).pw_dir,
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
            "ASC_E2E_AGENT_CODE": AGENT_CODE,
        }
        self.process = subprocess.Popen(
            ["runuser", "-u", user, "--", "python3", "-u", "-c", AGENT_CODE, cli, name],
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
    """Executes a generated local script in a descendant of the registered Agent."""
    return agent.execute(
        ["sh", "-c", script, "sh", *args], refresh=refresh, snapshot=snapshot
    )


def resolve_flags():
    """curl argv pinning every upstream name locally without DNS."""
    pins = [f"{host}:443:{UPSTREAM}" for host in HOSTS]
    pins.append(f"api.github.com:80:{UPSTREAM}")
    return [argument for pin in pins for argument in ("--resolve", pin)]


def curl(agent, url, *, token=None, method="GET", snapshot=False):
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
        if isinstance(status, dict) and ready(status):
            return
        if time.monotonic() > deadline:
            raise AssertionError(f"gateway status not ready: {status}")
        time.sleep(0.2)


def wait_loaded(cli):
    wait_status(
        cli,
        lambda status: status["enabled"]
        and status["loadedGeneration"] == status["generation"],
    )


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
    # A shared preset token would be rotated for existing domains by these tests.
    assert (
        not status["enabled"] and not status["agents"]
    ), "run on a host without configured gateway Agent domains"
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


@contextlib.contextmanager
def protected_token(cli, user, agents=(AGENT,), foreign_user=None):
    """Starts independent Agents, registers before enable, and unregisters on exit."""
    alphabet = string.ascii_letters + string.digits
    real = "ghp_" + "".join(secrets.choice(alphabet) for _ in range(36))
    domains = {}
    registered = []
    with tempfile.TemporaryDirectory() as directory:
        secret_file = Path(directory) / "github.token"
        secret_file.write_text(real + "\n")
        secret_file.chmod(0o600)
        try:
            # key add must not create a domain implicitly.
            assert (
                gateway(
                    cli,
                    "key",
                    "add",
                    "github",
                    "--agent",
                    AGENT,
                    "--secret-file",
                    str(secret_file),
                    check=False,
                ).returncode
                != 0
            )
            names = (*agents, OTHER_AGENT) if foreign_user else agents
            for name in names:
                owner = foreign_user if foreign_user and name == OTHER_AGENT else user
                domain = Agent(cli, owner, name)
                domains[name] = domain
                gateway(
                    cli,
                    "register",
                    "--agent",
                    name,
                    "--uid",
                    str(domain.ready["uid"]),
                    "--pid",
                    str(domain.ready["pid"]),
                )
                registered.append(name)
                assert not gateway(cli, "status")["enabled"]
                scope = f"/agentsec.slice/agent-sec-agent-{name}.scope"
                for snapshot in (False, True):
                    probe = domain.request(probe=True, refresh=False, snapshot=snapshot)
                    assert probe["cgroup"].endswith(scope)
                    assert probe["descendantCgroup"] == probe["cgroup"]
                    expected_pid = (
                        domain.ready["child"]["pid"]
                        if snapshot
                        else domain.ready["pid"]
                    )
                    assert probe["pid"] == expected_pid
                gateway(
                    cli,
                    "key",
                    "add",
                    "github",
                    "--agent",
                    name,
                    "--secret-file",
                    str(secret_file),
                )
            gateway(cli, "enable")
            wait_loaded(cli)
            yield real, domains
        finally:
            try:
                # Close every owned process tree even if another close fails.
                with contextlib.ExitStack() as cleanup:
                    for domain in domains.values():
                        cleanup.callback(domain.close)
            finally:
                try:
                    wait_status(cli, lambda status: True)
                    remaining = {
                        entry["name"] for entry in gateway(cli, "status")["agents"]
                    }
                    # Domain removal, not key remove, owns preset revocation.
                    with contextlib.ExitStack() as cleanup:
                        for name in registered:
                            if name in remaining:
                                cleanup.callback(
                                    gateway, cli, "unregister", "--agent", name
                                )
                    assert gateway(cli, "status")["agents"] == []
                    assert all(
                        entry["state"] == "retired"
                        and all(fake["retired"] for fake in entry["fakes"])
                        for entry in gateway(cli, "key", "list")["credentials"]
                    )
                finally:
                    gateway(cli, "disable")
    assert sorted(path.name for path in RUNTIME.iterdir()) == ["ca-bundle.pem"]
    assert (RUNTIME / "ca-bundle.pem").is_file()
    assert command("nft", "list", "table", "inet", TABLE, check=False).returncode != 0
    logs = command("journalctl", "--no-pager", "-u", UNIT, "-u", DAEMON_UNIT).stdout
    assert real not in logs


def test_agent_sees_only_a_fake_inside_its_domain(host, upstream):
    cli, user = host
    nobody = pwd.getpwnam("nobody")
    assert nobody.pw_uid not in (0, pwd.getpwnam(user).pw_uid)
    foreign_user = nobody.pw_name
    with protected_token(cli, user, foreign_user=foreign_user) as (real, domains):
        agent = domains[AGENT]
        grant = gateway(cli, "credentials", "--agent", AGENT)
        assert set(grant) == {"agent", "scope", "environment"}
        assert grant["agent"] == AGENT
        for snapshot in (False, True):
            probe = agent.request(probe=True, snapshot=snapshot)
            assert probe["cgroup"].endswith(f"/agentsec.slice/{grant['scope']}")
            assert probe["descendantCgroup"] == probe["cgroup"]
            environment = probe["environment"]
            token = environment["GITHUB_TOKEN"]
            assert (
                token == environment["GH_TOKEN"] == grant["environment"]["GITHUB_TOKEN"]
            )
            assert (
                token != real and token.startswith("ghp_") and len(token) == len(real)
            )
            assert real not in json.dumps(environment)
            for variable in (
                "SSL_CERT_FILE",
                "REQUESTS_CA_BUNDLE",
                "CURL_CA_BUNDLE",
                "GIT_SSL_CAINFO",
            ):
                assert environment[variable] == grant["environment"][variable]
                assert Path(environment[variable]).is_file()
        before = agent.request(probe=True, refresh=False)["cgroup"]
        own = agent.execute(
            [cli, "gateway", "credentials", "--agent", AGENT], refresh=False
        )
        assert own.returncode == 0, own.stderr
        assert json.loads(own.stdout) == grant
        assert agent.request(probe=True, refresh=False)["cgroup"] == before
        # A matching UID may query outside the scope without moving the caller.
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
            "credentials",
            "--agent",
            AGENT,
        )
        assert json.loads(outside.stdout) == grant
        controls = [
            ["enable"],
            ["disable"],
            ["key", "list"],
            ["key", "remove", "github", "--agent", AGENT],
            [
                "key",
                "add",
                "github",
                "--agent",
                AGENT,
                "--secret-file",
                "/does-not-exist",
            ],
            [
                "register",
                "--agent",
                "asc-e2e-denied",
                "--uid",
                str(agent.ready["uid"]),
                "--pid",
                str(agent.ready["pid"]),
            ],
            ["unregister", "--agent", AGENT],
        ]
        for args in controls:
            denied = agent.execute([cli, "gateway", *args], refresh=False)
            assert denied.returncode != 0, args
            assert "permission_denied" in (denied.stdout + denied.stderr), args
        status = agent.execute([cli, "gateway", "status"], refresh=False)
        assert status.returncode == 0, status.stderr
        assert {entry["name"] for entry in json.loads(status.stdout)["agents"]} == {
            AGENT
        }
        foreign = domains[OTHER_AGENT]
        for caller, target in ((agent, OTHER_AGENT), (foreign, AGENT)):
            denied = caller.execute(
                [cli, "gateway", "credentials", "--agent", target], refresh=False
            )
            assert denied.returncode != 0
            assert "permission_denied" in (denied.stdout + denied.stderr)
        foreign_status = foreign.execute([cli, "gateway", "status"], refresh=False)
        assert foreign_status.returncode == 0, foreign_status.stderr
        assert {
            entry["name"] for entry in json.loads(foreign_status.stdout)["agents"]
        } == {OTHER_AGENT}
        assert {entry["name"] for entry in gateway(cli, "status")["agents"]} == {
            AGENT,
            OTHER_AGENT,
        }


def test_rest_requests_carry_the_real_token_upstream(host, upstream):
    cli, user = host
    seen = upstream["seen"]
    with protected_token(cli, user) as (real, domains):
        agent = domains[AGENT]
        fake = fake_of(agent)
        for snapshot in (False, True):
            echoed = curl(agent, "https://api.github.com/user", snapshot=snapshot)
            assert seen[-1] == (
                "https",
                "api.github.com",
                "GET",
                "/user",
                f"Bearer {real}",
            )
            # The upstream echo is rewritten even for the pre-registration child.
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


def test_git_clone_and_push_use_the_real_token_upstream(host, upstream):
    cli, user = host
    seen = upstream["seen"]
    with protected_token(cli, user) as (real, domains):
        agent = domains[AGENT]
        fake = fake_of(agent)
        script = (
            'set -e; work=$(mktemp -d); trap \'rm -rf "$work"\' EXIT; cd "$work";'
            f" git {GIT_RESOLVE} clone -q https://github.com/{REPOSITORY} repo; cd repo;"
            " echo pushed > CHANGE; git add CHANGE;"
            " git -c user.name=agent -c user.email=agent@example.invalid"
            " commit -q -m 'pushed through the gateway';"
            f" git {GIT_RESOLVE} push -q origin main"
        )
        result = in_domain(agent, script)
        assert result.returncode == 0, result.stderr
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
        removed = gateway(cli, "key", "remove", "github", "--agent", AGENT)
        assert removed["credential"] == "fake_retired" and removed["applied"]
        cases.append((curl(other, api, token=fake), "retired_credential"))
        # key remove leaves the domain registered, but credentials no longer
        # supplies either token. unregister then makes the read-only query fail.
        grant = gateway(cli, "credentials", "--agent", AGENT)
        assert not {"GITHUB_TOKEN", "GH_TOKEN"} & grant["environment"].keys()
        assert {entry["name"] for entry in gateway(cli, "status")["agents"]} == {
            AGENT,
            OTHER_AGENT,
        }
        gateway(cli, "unregister", "--agent", AGENT)
        revoked = agent.request(argv=["true"])
        assert revoked["returncode"] != 0 and revoked["stage"] == "credentials"
        for snapshot in (False, True):
            probe = agent.request(probe=True, refresh=False, snapshot=snapshot)
            expected = agent.ready["child"]["pid"] if snapshot else agent.ready["pid"]
            assert probe["pid"] == expected
        # The other domain still has github: unregister must revoke it without
        # a separate key remove, while leaving its independent Agent alive.
        gateway(cli, "unregister", "--agent", OTHER_AGENT)
        assert gateway(cli, "status")["agents"] == []
        assert all(
            entry["state"] == "retired"
            and all(fake["retired"] for fake in entry["fakes"])
            for entry in gateway(cli, "key", "list")["credentials"]
        )
        revoked = other.request(argv=["true"])
        assert revoked["returncode"] != 0 and revoked["stage"] == "credentials"
        for snapshot in (False, True):
            probe = other.request(probe=True, refresh=False, snapshot=snapshot)
            expected = other.ready["child"]["pid"] if snapshot else other.ready["pid"]
            assert probe["pid"] == expected
        assert (
            gateway(cli, "credentials", "--agent", OTHER_AGENT, check=False).returncode
            != 0
        )
        for body, code in cases:
            assert body == {"error": code, "source": "agent-sec-gateway"}
    # None of the rejected requests reached the upstream.
    assert seen == []


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


def test_replacement_survives_daemon_and_gateway_restarts(host, upstream):
    cli, user = host
    seen = upstream["seen"]
    with protected_token(cli, user) as (real, domains):
        agent = domains[AGENT]
        for unit in (DAEMON_UNIT, UNIT):
            command("systemctl", "restart", unit)
            wait_loaded(cli)
            curl(agent, "https://api.github.com/user")
            assert seen[-1][4] == f"Bearer {real}", unit
