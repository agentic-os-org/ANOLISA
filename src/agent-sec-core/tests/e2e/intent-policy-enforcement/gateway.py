"""独立验收 Gateway 凭据替换，不启动 cosh、不调用意图服务或 Sandbox。"""

import argparse
import hashlib
import os
import select
import signal
import subprocess
import sys
import tempfile
from contextlib import ExitStack
from pathlib import Path

import sight
from support import gateway
from support.common import (
    FIXTURES,
    as_account,
    banner,
    check_host,
    checking,
    cli,
    create_workspace,
    fixture_reader,
    local_run,
    publish,
    record,
    require,
    result_summary,
    run,
    section,
    test_account,
)

# 探针等待 Binding READY 后才发请求；验收完成前保持存活，避免路由计数随退出撤回。
CLIENT = """
import subprocess
import sys
from pathlib import Path

if sys.stdin.readline() != "ready\\n":
    sys.exit(2)
Path("out/debug.log").write_bytes(Path("home/github-header").read_bytes())
result = subprocess.run(
    ["/usr/bin/curl", "--disable", "--ipv4", "--noproxy", "*", "--silent", "--show-error",
     "--max-time", "30", "--cacert", "home/gateway-ca.pem",
     "--header", "@out/debug.log", "--user-agent", "anolisa-local-selftest",
     "--output", "out/github-user.json", "--write-out", "%{http_code}",
     "https://api.github.com/user"],
    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False, timeout=35,
)
if result.returncode:
    sys.exit("Gateway 客户端请求失败")
Path("out/http-status").write_bytes(result.stdout)
print("done", flush=True)
sys.stdin.readline()
"""


def preflight(user, source):
    section("环境预检")
    check_host(progress=False)
    account = test_account(user)
    gateway.preflight(cli, source)
    with checking("Gateway 客户端：/usr/bin/curl 可执行", progress=False):
        require(
            os.access("/usr/bin/curl", os.X_OK), "Missing executable: /usr/bin/curl"
        )
    with checking("Gateway 策略模板存在", progress=False):
        require(
            (FIXTURES / "gateway.cedar.in").is_file(), "Missing Gateway policy fixture"
        )
    section("PAP 与 Scope 服务预检")
    with checking("PAP 策略存储隔离：查询 policy list --limit 1"):
        require(
            not cli("policy", "list", "--limit", "1")["items"],
            "Use an isolated PAP store for the Gateway test",
        )
    with checking("daemon Scope 接口：查询 scope list --limit 1"):
        cli("scope", "list", "--limit", "1")
    return account


def stop_client(process) -> None:
    # 探针在独立会话中运行；同时结束它启动的 curl，再回收探针。
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=5)


def start_client(row: Path, account):
    script = row / "gateway-probe.py"
    script.write_text(CLIENT)
    script.chmod(0o644)
    return subprocess.Popen(
        [sys.executable, "-I", str(script)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
        **as_account(account, row),
    )


def exercise(account, source: str, report: Path, cleanup: ExitStack) -> gateway.Control:
    """Runs the Gateway scenario and returns the credential pair it verified."""
    row = create_workspace(cleanup)
    for name in ("home", "out"):
        path = row / name
        path.mkdir(mode=0o755)
        os.chown(path, account.pw_uid, account.pw_gid)
    secrets = Path(
        cleanup.enter_context(
            tempfile.TemporaryDirectory(prefix="anolisa-gateway-tokens-", dir="/root")
        )
    )
    section("GitHub 直连对照")
    with checking("直连对照：真实 Token=200 且账号一致，替身 Token=401"):
        control = gateway.credentials(secrets, source)
    record(
        report,
        "gateway/controls",
        {
            "real_status": 200,
            "fake_status": 401,
            "account_matched": True,
        },
    )
    section("Gateway 就绪")
    with checking("Gateway 就绪与客户端 CA、替身请求头"):
        ca = gateway.enable(cli, cleanup)
        gateway.client_files(row, ca, control.surrogate, account.pw_uid, account.pw_gid)
    section("Gateway 策略发布")
    path = report / "gateway.cedar"
    path.write_text(gateway.policy_text(secrets))
    lease = gateway.CredentialLease(cli, cleanup)
    name = report.name + "-gateway"
    lease.prepare_publication(path, name)
    policy = publish(
        path,
        name,
        lease.policy_cleanup,
        "--schema-id",
        gateway.HTTP_SCHEMA["schemaId"],
        "--schema-revision",
        str(gateway.HTTP_SCHEMA["revision"]),
        "--language",
        "cedar",
        on_created=lease.capture,
    )
    record(
        report,
        "gateway/policy",
        {
            "policy": policy,
            "revision": 1,
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        },
    )
    print(f"  策略文件：{path}", flush=True)
    print(f"  SHA-256：{hashlib.sha256(path.read_bytes()).hexdigest()}", flush=True)
    section("Gateway 请求与证据")
    row.chmod(0o755)
    with start_client(row, account) as process:
        try:
            # 凭据 Scope 要求非 root owner 和持续 selector；不选择共用 Python 解释器。
            scope = lease.scope_for(row / "gateway-probe.py", account.pw_uid)
            binding = sight.ready(scope, policy, process.pid)
            before = gateway.packets(scope)
            with checking("普通账号使用替身请求头，经 Gateway 请求 GitHub GET /user"):
                print(
                    "  等待客户端完成请求；超时预算 45 秒，不显示请求头或响应正文。",
                    flush=True,
                )
                process.stdin.write(b"ready\n")
                process.stdin.flush()
                require(
                    select.select([process.stdout], [], [], 45)[0],
                    "Gateway probe timed out",
                )
                require(process.stdout.readline() == b"done\n", "Gateway probe failed")
            with checking("Gateway 响应、账号、客户端无真实 Token、包计数"):
                gateway.verify(fixture_reader(row, account), control)
                after = gateway.packets(scope)
                require(after > before, "Gateway scope packet counter did not advance")
                print(
                    f"  HTTP=200，账号={control.account}；客户端无真实 Token，"
                    "注入值被 GitHub 接受。",
                    flush=True,
                )
                print(f"  Scope 包计数：{before} → {after}", flush=True)
            record(
                report,
                "gateway/6",
                {
                    "policy": policy,
                    "gateway_scope": scope,
                    "binding": binding,
                    "packets_before": before,
                    "packets_after": after,
                    "github_status": 200,
                    "account": control.account,
                    "account_matched": True,
                },
            )
        finally:
            stop_client(process)
    return control


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check", action="store_true", help="仅预检，不读取 Token 或请求 GitHub"
    )
    parser.add_argument(
        "--user",
        default=os.environ.get("SUDO_USER"),
        help="普通探针账号（默认 sudo 调用者）",
    )
    parser.add_argument(
        "--token-source",
        choices=gateway.TOKEN_SOURCES,
        default="auto",
        help="测试 Token 来源（auto 复用管理员环境已有的 GitHub 凭据）",
    )
    args = parser.parse_args()
    banner("Gateway 凭据替换验收", check=args.check)
    print("  不启动 cosh、不调用意图服务或 Sandbox。", flush=True)
    if args.token_source == "auto":
        print(
            "  默认复用管理员环境已有的 GitHub 凭据（GH_TOKEN、gh auth 或 git credential），"
            "不需要输入，并核验注入后返回 200 且账号一致。",
            flush=True,
        )
    account = preflight(args.user, args.token_source)
    if args.check:
        result_summary(
            "仅完成 Gateway 预检：未创建报告或工作区、未发布策略、未读取凭据。"
        )
        print("PASS gateway prerequisites", flush=True)
        return
    print(
        "  正式运行将发布策略、启用 Gateway 并请求 GitHub；结束后清理本轮资源。",
        flush=True,
    )
    with local_run() as (report, cleanup):
        control = exercise(account, args.token_source, report, cleanup)
    result_summary(
        f"Gateway 验收通过（GitHub 返回 200，账号 {control.account} 与直连一致）；"
        "本轮资源清理完成，报告已保留。",
        report,
    )


if __name__ == "__main__":
    run(main)
