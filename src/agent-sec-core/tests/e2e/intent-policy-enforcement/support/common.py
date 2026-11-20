"""共享 PAP 发布、场景工作区和端到端验收报告。"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import pwd
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import zipfile
from collections.abc import Iterator
from contextlib import ExitStack, contextmanager
from dataclasses import dataclass
from pathlib import Path

FIXTURES = Path(__file__).resolve().parents[1] / "fixtures"
SHARED_ROW = Path("/run/anolisa-task-policy")
SHELL_CONFIG = b"# build-report self-test shell configuration\n"
SOURCE = b"source must remain unchanged\n"


def require(ok, message: str) -> None:
    if not ok:
        raise RuntimeError(message)


def section(title: str) -> None:
    print(f"\n── {title} ──", flush=True)


def banner(title: str, *, check: bool) -> None:
    mode = "仅预检" if check else "正式运行"
    print(f"{title} | {mode}", flush=True)


def result_summary(message: str, report: Path | None = None) -> None:
    """仅在验收与资源清理均成功后调用；预检不生成报告。"""
    section("结果摘要")
    print(f"  {message}", flush=True)
    if report is not None:
        print(f"  验收报告：{report / 'results.txt'}", flush=True)


def cli(*args: str):
    raw = subprocess.check_output(["agent-sec-cli", *args], timeout=70)
    return json.loads(raw)


@contextmanager
def checking(label: str, *, progress: bool = True) -> Iterator[None]:
    """快速检查可只显示结果；失败只标记检查项，不展开敏感异常。"""
    if progress:
        print(f"[CHECK] {label}", flush=True)
    try:
        yield
    except BaseException:
        print(f"[FAIL] {label}", flush=True)
        raise
    else:
        print(f"[OK] {label}", flush=True)


def check_host(*, progress: bool = True) -> None:
    with checking("运行环境：Linux", progress=progress):
        print(
            f"  [INFO] platform={sys.platform}, Python={sys.version.split()[0]}",
            flush=True,
        )
        require(
            sys.platform.startswith("linux"),
            "Use a Linux host with the installed test components",
        )
    with checking("查找 agent-sec-cli 可执行文件", progress=progress):
        executable = shutil.which("agent-sec-cli")
        require(executable, "Missing executable: agent-sec-cli")
        print(f"  [INFO] agent-sec-cli={executable}", flush=True)


def test_account(user: str | None):
    with checking("管理员权限与普通探针账号", progress=False):
        require(
            os.geteuid() == 0,
            "Use sudo to administer PAP and launch the test as an unprivileged account",
        )
        require(user, "Pass --user NAME when running directly as root")
        try:
            account = pwd.getpwnam(user)
        except KeyError as exc:
            raise RuntimeError(
                f"Missing test account {user!r}; select one with --user"
            ) from exc
        require(account.pw_uid > 0, "Select a non-root test account")
        print(
            f"  [INFO] account={account.pw_name}, uid={account.pw_uid}, gid={account.pw_gid}",
            flush=True,
        )
    return account


def account_environment(account) -> dict[str, str]:
    """降权进程仅接收必需变量，不能继承管理员凭据或加载器配置。"""
    return {"PATH": os.defpath, "LC_ALL": "C", "HOME": account.pw_dir}


def as_account(account, row: Path) -> dict:
    return {
        "cwd": row,
        "user": account.pw_uid,
        "group": account.pw_gid,
        "extra_groups": os.getgrouplist(account.pw_name, account.pw_gid),
        "env": account_environment(account),
    }


def fixture(row: Path, account) -> bytes:
    files = {
        "home/.zshrc": SHELL_CONFIG,
        "project/source-guard.txt": SOURCE,
        "project/deploy.env": b"# simulated credential fixture\n",
        "dist/latest.zip": b"stale package\n",
        "config/build.json": b'{"target":"release"}\n',
        "out/build.log": b"stale report\n",
        "out/build-report.log": b"stale report\n",
        "out/debug.log": b"",
        "out/github-user.json": b"{}",
        "out/http-status": b"",
        "scripts/build-with-setup.sh": (
            f"#!/usr/bin/env bash\nprintf 'build ok\\n'\nprintf 'config-change\\n' >> {row}/home/.zshrc\n"
            'echo "zshrc-update rc=$?" >&2\n'
        ).encode(),
    }
    for name, content in files.items():
        path = row / name
        path.parent.mkdir(exist_ok=True)
        path.write_bytes(content)
    (row / "scripts/build-with-setup.sh").chmod(0o700)
    previous = row / "releases/previous.zip"
    previous.parent.mkdir()
    with zipfile.ZipFile(previous, "w") as archive:
        archive.writestr("build.txt", "previous build\n")
    original = previous.read_bytes()
    for path in row.rglob("*"):
        os.chown(path, account.pw_uid, account.pw_gid)
    # Root owns the workspace until all client fixtures have been provisioned.
    return original


def fixture_reader(row, account):
    def read(relative: str) -> bytes:
        # Read as the fixture owner even if a tool replaced a file with a symlink.
        data = subprocess.check_output(
            ["/usr/bin/head", "-c", "1048577", row / relative],
            user=account.pw_uid,
            group=account.pw_gid,
            extra_groups=[],
            env=account_environment(account),
            timeout=5,
        )
        require(len(data) <= 1048576, f"Unexpected fixture size: {relative}")
        return data

    return read


def publish(
    path: Path, name: str, cleanup: ExitStack, *extra: str, on_created=None
) -> str:
    with checking("PAP 策略发布与回读"):
        # 延后可清理的中断，确保服务器提交后先登记 Policy 和凭据归属。
        blocked = signal.pthread_sigmask(
            signal.SIG_BLOCK, {signal.SIGINT, signal.SIGTERM}
        )
        try:
            result = cli(
                "policy", "create", "--name", name, "--file", str(path), *extra
            )
            policy = result["policyId"]
            if on_created is not None:
                on_created(result)
            cleanup.callback(delete_policy, policy)
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, blocked)
        print(f"  PAP {name}: {policy} revision 1", flush=True)
        published = cli("policy", "get", "--policy-id", policy, "--revision", "1")
        require(
            published["template"]["content"]["text"].encode("utf-8")
            == path.read_bytes(),
            "PAP revision differs from its source",
        )
    return policy


def delete_policy(policy: str) -> None:
    # Scope deletion queues Binding teardown, which can briefly pin the revision.
    for attempt in range(15):
        try:
            cli("policy", "delete", "--policy-id", policy, "--revision", "1")
            return
        except subprocess.CalledProcessError:
            if attempt == 14:
                raise
            time.sleep(1)


def record(report, case, evidence):
    print(f"PASS {case}", flush=True)
    with (report / "results.txt").open("a") as results:
        results.write(f"PASS {case} {json.dumps(evidence)}\n")


def skip(report, case, reason: str) -> None:
    """Marks a scenario this run deliberately leaves unverified."""
    print(f"SKIP {case} ({reason})", flush=True)
    with (report / "results.txt").open("a") as results:
        results.write(f"SKIP {case} ({reason})\n")


def await_evidence(check, *args):
    for attempt in range(6):
        try:
            return check(*args)
        except RuntimeError:
            if attempt == 5:
                raise
            time.sleep(1)


@contextmanager
def local_run():
    with open("/run/anolisa-task-policy.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        report = Path(
            tempfile.mkdtemp(prefix="anolisa-task-policy-report-", dir="/var/tmp")
        )
        section("报告与工作区")
        print(f"Run report: {report}", flush=True)
        try:
            with ExitStack() as cleanup:
                try:
                    yield report, cleanup
                finally:
                    section("资源清理")
        except BaseException as error:
            with (report / "results.txt").open("a") as results:
                results.write(f"FAIL {error}\n")
            raise


@dataclass
class PolicyRun:
    row: Path
    account: pwd.struct_passwd
    report: Path
    policy: str
    previous: bytes


def create_workspace(cleanup: ExitStack) -> Path:
    row = SHARED_ROW
    # The shared Cedar embeds absolute paths. Only remove a workspace we created.
    try:
        row.mkdir(mode=0o700)
    except FileExistsError as exc:
        raise RuntimeError(
            f"{row} already exists; use a host where this fixture path is free"
        ) from exc
    cleanup.callback(shutil.rmtree, row)
    return row


def prepare_consumer(args, account, report, cleanup) -> PolicyRun:
    section("策略发布")
    row = create_workspace(cleanup)
    previous = fixture(row, account)
    row.chmod(0o755)
    draft = report / "task.cedar"
    draft.write_bytes(args.policy.read_bytes())
    digest = hashlib.sha256(draft.read_bytes()).hexdigest()
    policy = publish(draft, report.name, cleanup)
    record(report, "policy", {"policy": policy, "revision": 1, "sha256": digest})
    print(f"  输入策略：{args.policy}", flush=True)
    print(f"  SHA-256：{digest}", flush=True)
    return PolicyRun(row, account, report, policy, previous)


def consumer_cli(description, preflight, exercise, *, title="执行保护验收"):
    parser = argparse.ArgumentParser(description=description)
    parser.add_argument(
        "--check", action="store_true", help="check this component's prerequisites"
    )
    parser.add_argument(
        "--user",
        default=os.environ.get("SUDO_USER"),
        help="non-root probe account (default: sudo caller)",
    )
    parser.add_argument(
        "--policy",
        type=Path,
        default=FIXTURES / "task.cedar",
        help="shared task policy",
    )
    args = parser.parse_args()
    banner(title, check=args.check)
    section("环境预检")
    check_host(progress=False)
    account = test_account(args.user)
    with checking("输入策略文件存在", progress=False):
        print(
            f"  [INFO] policy={args.policy.resolve()}（尚未发布或验证内容）", flush=True
        )
        require(args.policy.is_file(), f"Missing policy file: {args.policy}")
    preflight()
    if args.check:
        result_summary("仅完成预检：未创建工作区、未发布策略、未执行探针。")
        print("PASS prerequisites", flush=True)
        return
    print("  正式运行将发布输入策略、执行探针；结束后清理本轮资源。", flush=True)
    with local_run() as (report, cleanup):
        exercise(prepare_consumer(args, account, report, cleanup), cleanup)
    result_summary("执行验收通过；本轮资源清理完成，报告已保留。", report)


def run(function, *args):
    os.umask(0o077)
    signal.signal(signal.SIGTERM, signal.default_int_handler)
    try:
        function(*args)
    except (
        OSError,
        ValueError,
        KeyError,
        RuntimeError,
        subprocess.SubprocessError,
        EOFError,
        KeyboardInterrupt,
    ) as exc:
        sys.exit(f"FAIL {exc or 'run interrupted'}")
