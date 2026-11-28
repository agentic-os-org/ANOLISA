"""为可选的 GitHub GET /user 场景准备凭据并验收替换结果。"""

import getpass
import hashlib
import http.client
import json
import os
import pwd
import shutil
import stat
import subprocess
import time
import warnings
from contextlib import ExitStack
from dataclasses import dataclass
from pathlib import Path

from support.common import FIXTURES, checking, delete_policy, require, section

GITHUB_HOST = "api.github.com"
HTTP_SCHEMA = {"schemaId": "seccore.system", "revision": 3}
TOKEN_SOURCES = ("auto", "prompt")


@dataclass(frozen=True)
class Control:
    """Token pair plus the account a direct GET /user returned for it."""

    token: bytes
    surrogate: bytes
    account: str


def _environment_token() -> bytes:
    """Returns a token already exported for GitHub tooling."""
    for name in ("GH_TOKEN", "GITHUB_TOKEN"):
        value = os.environ.get(name, "").strip()
        if value:
            return value.encode()
    return b""


def _gh_token() -> bytes:
    """Returns the token `gh` holds for github.com, when it is installed."""
    executable = shutil.which("gh")
    if not executable:
        return b""
    result = subprocess.run(
        [executable, "auth", "token", "--hostname", "github.com"],
        capture_output=True,
        timeout=20,
        check=False,
    )
    return result.stdout.strip() if result.returncode == 0 else b""


def _stored_token() -> bytes:
    """Returns the token git itself uses for github.com, without prompting."""
    result = subprocess.run(
        ["git", "credential", "fill"],
        input=b"protocol=https\nhost=github.com\n\n",
        capture_output=True,
        timeout=20,
        check=False,
        env={**os.environ, "GIT_TERMINAL_PROMPT": "0"},
    )
    for line in result.stdout.splitlines():
        if line.startswith(b"password="):
            return line.removeprefix(b"password=").strip()
    return b""


def discover_token() -> bytes:
    """Returns the first real credential this host already holds for GitHub."""
    for find in (_environment_token, _gh_token, _stored_token):
        token = find()
        if token:
            return token
    raise RuntimeError(
        "No GitHub credential available: export GH_TOKEN, run 'gh auth login', "
        "or store a github.com git credential"
    )


def _git_helper_configured() -> bool:
    """Whether a git config file declares a credential helper."""
    for path in (Path("/etc/gitconfig"), Path.home() / ".gitconfig"):
        try:
            text = path.read_text(errors="ignore").lower()
        except OSError:
            continue
        if "credential" in text and "helper" in text:
            return True
    return False


def credential_hint() -> str:
    """Names candidate credential sources without reading their values."""
    hints = [
        f"环境变量 {name}"
        for name in ("GH_TOKEN", "GITHUB_TOKEN")
        if os.environ.get(name, "").strip()
    ]
    home = Path.home()
    if (home / ".config/gh/hosts.yml").is_file() or shutil.which("gh"):
        hints.append("gh 登录")
    if (home / ".git-credentials").is_file() or _git_helper_configured():
        hints.append("git credential")
    return "、".join(hints)


def preflight(cli, source: str) -> None:
    section("Gateway 环境与服务预检")
    with checking("Gateway 服务账号：agent-sec-gw", progress=False):
        try:
            pwd.getpwnam("agent-sec-gw")
        except KeyError as exc:
            raise RuntimeError("缺少 Gateway 服务账号 agent-sec-gw") from exc
    with checking(
        "Gateway 运行时：/opt/agent-sec-gateway/current/mitmdump 可执行", progress=False
    ):
        require(
            os.access("/opt/agent-sec-gateway/current/mitmdump", os.X_OK),
            "Install the Gateway mitmdump runtime",
        )
    with checking("系统工具：nft", progress=False):
        executable = shutil.which("nft")
        require(executable, "Missing executable: nft")
        print(f"  [INFO] nft={executable}", flush=True)
    with checking("Gateway 状态接口：查询 gateway status"):
        status = cli("gateway", "status")
        for key in ("enabled", "ready"):
            value = status.get(key)
            print(
                f"  [INFO] Gateway {key}={value if isinstance(value, bool) else 'unknown'}",
                flush=True,
            )
        print(
            f"  [INFO] Token 来源={source}；本次不读取凭据、不请求 GitHub、不启用 Gateway。",
            flush=True,
        )
    if source == "auto":
        with checking("管理员环境已有 GitHub 凭据来源，且不读取其值", progress=False):
            hint = credential_hint()
            require(
                hint,
                "No GitHub credential available: export GH_TOKEN, run "
                "'gh auth login', or store a github.com git credential",
            )
            print(f"  [INFO] 候选来源：{hint}", flush=True)


def github_user(token: bytes, expected: int) -> str:
    """Requests GET /user from GitHub and returns the login of a 200 reply."""
    connection = http.client.HTTPSConnection(GITHUB_HOST, timeout=20)
    try:
        connection.request(
            "GET",
            "/user",
            headers={
                "User-Agent": "anolisa-local-selftest",
                "Authorization": b"Bearer " + token,
            },
        )
        response = connection.getresponse()
        if response.status != expected:
            raise RuntimeError(
                f"Direct control expected HTTP {expected}, got {response.status}"
            )
        return json.loads(response.read())["login"] if expected == 200 else ""
    finally:
        connection.close()


def credentials(directory: Path, source: str) -> Control:
    """Resolves the real GitHub token, derives its surrogate, and proves 200.

    ``source="auto"`` reuses a credential the admin environment already holds,
    so a run needs no input; ``prompt`` asks for a token on stdin instead.
    """
    print(
        f"  Token 来源={source}；凭据不显示，接下来会实际请求 GET /user 做直连对照。",
        flush=True,
    )
    if source == "auto":
        token = discover_token()
        print("  使用管理员环境已有的 GitHub 凭据；不需要输入。", flush=True)
    else:
        with warnings.catch_warnings():
            warnings.simplefilter("error", getpass.GetPassWarning)
            token = (
                getpass.getpass("请输入 GitHub 测试 Token（隐藏输入）：")
                .strip()
                .encode()
            )
    if not token or not token.isascii() or not all(33 <= char <= 126 for char in token):
        raise RuntimeError("Expected one nonempty ASCII token")
    real = directory / "real"
    fake = directory / "fake"
    real.write_bytes(token)
    subprocess.run(
        [
            "agent-sec-cli",
            "key",
            "generate-fake",
            "--real-token-file",
            real,
            "--output",
            fake,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=True,
        timeout=20,
    )
    surrogate = fake.read_bytes().strip()
    if not surrogate or token in surrogate:
        raise RuntimeError("Invalid surrogate returned by generate-fake")
    login = github_user(token, 200)
    if not login:
        raise RuntimeError("Missing login in direct control")
    # The surrogate must never be accepted by GitHub either.
    github_user(surrogate, 401)
    return Control(token, surrogate, login)


def enable(cli, cleanup) -> Path:
    if cli("gateway", "status")["enabled"] is False:
        cleanup.callback(cli, "gateway", "disable")
        cli("gateway", "enable")
    for _ in range(30):
        if cli("gateway", "status")["ready"] is True:
            bundle = Path(cli("gateway", "ca")["caBundlePath"])
            if bundle.is_file():
                return bundle
        time.sleep(1)
    raise RuntimeError("Gateway did not become ready with a CA bundle")


def client_files(row: Path, bundle: Path, fake: bytes, uid: int, gid: int) -> None:
    shutil.copyfile(bundle, row / "home/gateway-ca.pem")
    header = row / "home/github-header"
    header.write_bytes(b"Authorization: Bearer " + fake + b"\n")
    for path in (header, row / "home/gateway-ca.pem"):
        os.chown(path, uid, gid)


def policy_text(directory: Path) -> str:
    template = (FIXTURES / "gateway.cedar.in").read_text()
    return template.replace("{real_file}", str(directory / "real")).replace(
        "{fake_file}", str(directory / "fake")
    )


def all_items(cli, kind: str) -> list[dict]:
    """引用检查必须翻页，不能把第一页以外的资源当成不存在。"""
    items = []
    while True:
        page = cli(kind, "list", "--limit", "1000", "--offset", str(len(items)))[
            "items"
        ]
        items.extend(page)
        if len(page) < 1000:
            return items


def pair_ids(policy: dict) -> set[str]:
    return {binding["pairId"] for binding in policy.get("credentialBindings", [])}


def registered_pair(source_key: str) -> str:
    """Reads only the registration identity into the recovery result."""
    data = Path(os.environ.get("AGENT_SEC_DATA_DIR") or "/var/lib/agent-sec")
    store = data / "credentials/store.json"
    fd = os.open(store, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        require(
            stat.S_ISREG(info.st_mode)
            and stat.S_IMODE(info.st_mode) == 0o600
            and info.st_uid == os.geteuid()
            and info.st_size <= 8 * 1024 * 1024,
            "Expected the daemon's private credential store",
        )
        document = json.load(stream)
    require(document["version"] == 2, "Unknown credential store format")
    ids = document["registrations"].get(source_key)
    require(
        isinstance(ids, list) and len(ids) == 1 and isinstance(ids[0], str),
        "Missing or ambiguous credential registration for the submitted source",
    )
    require(
        not any(
            ids[0] in others
            for key, others in document["registrations"].items()
            if key != source_key
        ),
        "Credential registration is shared by another source",
    )
    return ids[0]


class CredentialLease:
    """登记本轮导入归属，先收敛 Scope 删除，再回收无其他引用的新凭据。"""

    def __init__(self, cli, cleanup: ExitStack):
        self.cli = cli
        self.existing = set(cli("gateway", "key", "list")["pairids"])
        self.policy_cleanup = ExitStack()
        self.journal = None
        self.policy = None
        self.pairs = set()
        self.reused = set()
        self.selector = None
        self.uid = None
        self.scope_uncertain = False
        cleanup.callback(self.close)

    def prepare_publication(self, path: Path, name: str) -> None:
        # GatewayService::register hashes the serde JSON of CedarPolicySource.
        self.source = {"schema": HTTP_SCHEMA, "text": path.read_bytes().decode()}
        encoded = json.dumps(self.source, ensure_ascii=False, separators=(",", ":"))
        self.source_key = hashlib.sha256(encoded.encode()).hexdigest()
        self.name = name
        self.prior_policies = {p["policyId"] for p in all_items(self.cli, "policy")}
        journal = path.parent / "gateway-recovery.json"
        with journal.open("x") as file:
            json.dump(
                {
                    "policyName": name,
                    "sourceFile": path.name,
                    "schema": HTTP_SCHEMA,
                    "registrationKey": self.source_key,
                    "priorPairIds": sorted(self.existing),
                    "priorPolicyIds": sorted(self.prior_policies),
                },
                file,
            )
        self.journal = journal

    def matches_publication(self, policy: dict) -> bool:
        return (
            policy["policyId"] not in self.prior_policies
            and policy.get("policyName") == self.name
            and policy.get("revision") == 1
            and policy.get("template") == {"language": "cedar", "content": self.source}
        )

    def capture(self, policy: dict) -> None:
        if self.journal is not None:
            require(
                self.matches_publication(policy), "Unexpected Gateway Policy source"
            )
        bindings = policy.get("credentialBindings", [])
        require(
            len(bindings) == 1
            and bindings[0].get("ruleId") == "case6-github-user"
            and isinstance(bindings[0].get("pairId"), str)
            and bindings[0]["pairId"],
            "Expected one credential pair for the Gateway rule",
        )
        imported = pair_ids(policy)
        self.pairs = imported - self.existing
        self.reused = imported & self.existing
        self.policy = policy["policyId"]

    def reconcile_publication(self) -> None:
        # A lost create reply can precede the PAP commit. Never replay the create.
        for attempt in range(5):
            matches = [
                p for p in all_items(self.cli, "policy") if self.matches_publication(p)
            ]
            if matches:
                require(len(matches) == 1, "Ambiguous Gateway Policy ownership")
                self.capture(matches[0])
                self.policy_cleanup.callback(delete_policy, self.policy)
                return
            if attempt < 4:
                time.sleep(1)
        pair = registered_pair(self.source_key)
        self.pairs = {pair} - self.existing

    def scope_for(self, script: Path, uid: int) -> str:
        # 最后一个 Scope 的删除会自动退役 pair，不能把已有凭据带入本轮 Scope。
        require(
            not self.reused,
            "Gateway policy reused existing credentials; use a dedicated test credential",
        )
        # 在 RPC 前记录选择器；响应丢失或中断后仍能找回已提交的 Scope。
        self.selector = str(script)
        self.uid = uid
        self.scope_uncertain = True
        scope = self.cli(
            "scope",
            "create",
            "--cmdline-arg",
            self.selector,
            "--owner-uid",
            str(uid),
            "--policy-id",
            self.policy,
            "--policy-revision",
            "1",
        )["scopeId"]
        self.scope_uncertain = False
        print(f"  Scope {scope} for {self.policy}", flush=True)
        return scope

    def owns_scope(self, scope: dict) -> bool:
        return (
            self.selector is not None
            and scope.get("ownerUid") == self.uid
            and scope["selector"]
            == {"kind": "process", "match": {"cmdlineArg": self.selector}}
            and any(
                policy["policyId"] == self.policy and policy["revision"] == 1
                for policy in scope["policySnapshots"]
            )
        )

    def close(self) -> None:
        if self.journal is None and self.policy is None:
            return
        try:
            if self.policy is None:
                self.reconcile_publication()
            self.retire()
            if self.journal is not None:
                # Without a PAP snapshot we cannot rule out a late create commit.
                require(
                    self.policy is not None, "Policy publication outcome still unknown"
                )
                self.journal.unlink()
        except BaseException as error:
            if self.journal is not None:
                raise RuntimeError(
                    f"Gateway cleanup incomplete ({error}); see {self.journal}"
                ) from error
            raise

    def retire(self) -> None:
        try:
            with checking("本轮 Scope 收敛与持久凭据回收"):
                # 不把 scope delete 的接收回执当成异步 Binding 已清理的证明。
                for attempt in range(30):
                    scopes = all_items(self.cli, "scope")
                    owned = [scope for scope in scopes if self.owns_scope(scope)]
                    if not owned:
                        if self.scope_uncertain and attempt < 29:
                            time.sleep(1)
                            continue
                        break
                    self.scope_uncertain = False
                    for scope in owned:
                        self.cli("scope", "delete", "--scope-id", scope["scopeId"])
                    if attempt == 29:
                        raise RuntimeError(
                            f"Scope teardown incomplete; retry cleanup for pair IDs {sorted(self.pairs)}"
                        )
                    time.sleep(1)
                referenced = {
                    pair
                    for scope in scopes
                    for policy in scope["policySnapshots"]
                    for pair in pair_ids(policy)
                }
                for policy in all_items(self.cli, "policy"):
                    if (policy["policyId"], policy["revision"]) != (self.policy, 1):
                        referenced.update(pair_ids(policy))
                active = set(self.cli("gateway", "key", "list")["pairids"])
                for pair in sorted(self.pairs & active):
                    if pair in referenced:
                        print(f"  [INFO] 保留仍被其他资源引用的凭据 {pair}", flush=True)
                        continue
                    self.cli("gateway", "key", "remove", pair)
                remaining = set(self.cli("gateway", "key", "list")["pairids"])
                require(
                    not (remaining & (self.pairs - referenced)),
                    f"Credential retirement incomplete for pair IDs {sorted(self.pairs - referenced)}",
                )
        finally:
            self.policy_cleanup.close()


def packets(scope: str) -> int:
    output = subprocess.check_output(
        ["nft", "-j", "list", "table", "inet", "agent_sec_gateway"], timeout=10
    )
    counters = [
        expr["counter"]["packets"]
        for entry in json.loads(output)["nftables"]
        if entry.get("rule", {}).get("comment") == f"asc:scope:{scope}"
        for expr in entry["rule"]["expr"]
        if "counter" in expr
    ]
    if not counters:
        raise RuntimeError("Missing nftables packet counter for this Gateway scope")
    return sum(counters)


def verify(read, control: Control) -> None:
    """Requires the client to hold only the surrogate and GitHub the real value."""
    log = read("out/debug.log")
    body = read("out/github-user.json")
    if control.token in log or control.token in body:
        raise RuntimeError("Real credential found in a client artifact")
    if log.splitlines() != [b"Authorization: Bearer " + control.surrogate]:
        raise RuntimeError(
            "Expected exactly the surrogate header in the client debug log"
        )
    if read("out/http-status").strip() != b"200":
        raise RuntimeError("Expected HTTP 200 through the Gateway")
    document = json.loads(body)
    if not isinstance(document, dict) or document.get("login") != control.account:
        raise RuntimeError("Expected the same account as the direct control")
