"""验证按 PID 绑定进程后的 AgentSight / ActPlane 文件保护。"""

import json
import subprocess
import sys
import time
from contextlib import ExitStack
from pathlib import Path
from urllib.request import ProxyHandler, build_opener
from uuid import NAMESPACE_URL, uuid5

from support.common import (
    PolicyRun,
    as_account,
    await_evidence,
    checking,
    cli,
    consumer_cli,
    fixture_reader,
    record,
    require,
    run,
    section,
)


def sight(endpoint: str):
    opener = build_opener(ProxyHandler({}))
    with opener.open(
        "http://127.0.0.1:7396/api/enforcement/" + endpoint, timeout=5
    ) as response:
        return json.load(response)


# ActPlane receives a translated Binding identity: the daemon sends a UUIDv5 of
# "urn:agentseccore:agentsight-binding:{bindingId}:revision:{bindingRevision}"
# (asc-agentsight-client::target_binding_id) and violation rows carry that id,
# not the PAP `bindingId` that `binding list` reports.
ACTPLANE_BINDING_PREFIX = "urn:agentseccore:agentsight-binding:"


def actplane_binding(binding: str, revision: int) -> str:
    """Translate a PAP Binding identity into the id ActPlane reports."""
    return str(
        uuid5(NAMESPACE_URL, f"{ACTPLANE_BINDING_PREFIX}{binding}:revision:{revision}")
    )


def check_actplane() -> None:
    section("ActPlane 健康与能力")
    with checking("ActPlane 健康接口：http://127.0.0.1:7396/api/enforcement/health"):
        health = sight("health")
        require(
            health["ready"] is True and health["backend"] == "actplane",
            "ActPlane health is not ready",
        )
        print("  [INFO] ready=True, backend=actplane", flush=True)
    for cap in (
        "file_delete_guard",
        "file_entry_delete_protection",
        "directory_tree_delete_protection",
        "file_read_protection",
        "static_mount_topology",
    ):
        with checking(f"ActPlane 必需能力：{cap}=True", progress=False):
            require(
                health["capabilities"].get(cap) is True,
                f"Missing {cap}; check the agent-file-guard profile",
            )


def check_violation(policy: str, binding: str, case: str, start_ns: int) -> dict:
    """Confirm this round's ActPlane denial of one case fixture.

    `binding` is the PAP Binding identity; `ready` pins the first spec revision,
    so the translated id ActPlane records can be derived from it.
    """
    operation, target = (
        ("read", "deploy.env") if case == "4" else ("unlink", "previous.zip")
    )
    translated = actplane_binding(binding, 1)
    rows = sight("violations?limit=200")["violations"]
    for row in rows:
        if (
            row["policy_id"] == policy
            and row["binding_id"] == translated
            and row["occurred_at_ns"] >= start_ns
            and row["blocked"] is True
            and row["operation"] == operation
            and row["target"] == target
        ):
            return {
                "violation": row["event_id"],
                "actplane_binding": translated,
                "operation": operation,
                "target": target,
                "blocked": True,
            }
    raise RuntimeError(
        f"Expected a new blocked {operation} of {target} for binding {binding} "
        f"(ActPlane binding {translated})"
    )


def scope_for(policy: str, cleanup: ExitStack, *selector: str) -> str:
    scope = cli(
        "scope", "create", *selector, "--policy-id", policy, "--policy-revision", "1"
    )["scopeId"]
    print(f"  Scope {scope} for {policy}", flush=True)
    cleanup.callback(cli, "scope", "delete", "--scope-id", scope)
    return scope


def ready(scope: str, policy: str, pid: int) -> str:
    with checking("探针 Binding 达到 READY"):
        print(
            f"  Scope={scope!r}，PID={pid}；最多 30 轮查询，轮间等待 1 秒。", flush=True
        )
        phase = "undiscovered"
        for _ in range(30):
            for item in cli("binding", "list")["items"]:
                spec = item["spec"]
                if (
                    spec["scope"]["scopeId"] != scope
                    or spec["scope"]["process"]["pid"] != pid
                ):
                    continue
                phase = item["status"]["phase"]
                require(
                    phase != "APPLY_FAILED",
                    f"Binding {spec['bindingId']} APPLY_FAILED for {scope}",
                )
                if phase == "READY":
                    require(
                        spec["policy"]["policyId"] == policy
                        and spec["policy"]["revision"] == 1,
                        "Binding references a different policy revision",
                    )
                    # check_violation derives the ActPlane identity assuming the
                    # initial spec revision; a spec update would change that id.
                    require(
                        spec["bindingRevision"] == 1,
                        "Binding already moved past its first spec revision",
                    )
                    print(
                        f"  Binding={spec['bindingId']!r}，策略 revision=1", flush=True
                    )
                    return spec["bindingId"]
            time.sleep(1)
        raise RuntimeError(f"Scope {scope}, cosh PID {pid}: last binding phase {phase}")


ACTPLANE_PROBE = """
import errno
import sys
from pathlib import Path

if sys.stdin.readline() != "ready\\n":
    sys.exit(2)
if not Path("project/source-guard.txt").read_bytes():
    sys.exit("Failed to read control file")
control = Path("releases/control")
control.touch(exist_ok=False)
control.unlink()
for operation in (Path("releases/previous.zip").unlink, Path("project/deploy.env").read_bytes):
    try:
        operation()
    except OSError as error:
        if error.errno not in (errno.EACCES, errno.EPERM):
            raise
    else:
        sys.exit("Expected the protected fixture operation to be denied")
"""


def start_actplane(row: Path, account) -> subprocess.Popen:
    # The ordinary user's probe waits for its exact PID binding to become READY.
    return subprocess.Popen(
        [sys.executable, "-I", "-c", ACTPLANE_PROBE],
        stdin=subprocess.PIPE,
        **as_account(account, row),
    )


def preflight():
    section("Scope 服务预检")
    with checking("daemon Scope 接口：查询 scope list --limit 1"):
        cli("scope", "list", "--limit", "1")
        print("  [INFO] Scope 查询成功；本次未创建 Scope 或 Binding。", flush=True)
    check_actplane()


def exercise(work: PolicyRun, cleanup):
    section("ActPlane 探针与 Binding")
    with start_actplane(work.row, work.account) as process:
        try:
            scope = scope_for(work.policy, cleanup, "--pid", str(process.pid))
            binding = ready(scope, work.policy, process.pid)
            with checking("ActPlane 文件操作探针"):
                print(
                    "  执行对照操作、删除保护和读取保护探针；超时预算 15 秒。",
                    flush=True,
                )
                start = time.time_ns()
                process.communicate(input=b"ready\n", timeout=15)
                require(
                    process.returncode == 0,
                    f"ActPlane probe exited {process.returncode}",
                )
            read = fixture_reader(work.row, work.account)
            for case in ("3", "4"):
                title = "回滚包删除保护" if case == "3" else "模拟凭据读取保护"
                section(f"ActPlane 场景 {case}：{title}")
                with checking(f"actplane/{case}：内核拦截验收"):
                    evidence = await_evidence(
                        check_violation, work.policy, binding, case, start
                    )
                    require(
                        read("releases/previous.zip") == work.previous,
                        "Rollback package changed",
                    )
                    print(
                        f"  operation={evidence['operation']}，target={evidence['target']}，"
                        f"blocked={evidence['blocked']}，violation={evidence['violation']!r}",
                        flush=True,
                    )
                record(
                    work.report, f"actplane/{case}", {"binding": binding, **evidence}
                )
        finally:
            if process.poll() is None:
                process.kill()


def main():
    consumer_cli(__doc__, preflight, exercise, title="AgentSight / ActPlane 执行验收")


if __name__ == "__main__":
    run(main)
