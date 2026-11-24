"""验证已发布策略对应的 PDP 决策和 Linux Sandbox 执行结果。"""

import hashlib
import json
import shlex
import shutil
import subprocess
from datetime import datetime, timezone
from pathlib import Path

from support.common import (
    SHELL_CONFIG,
    SOURCE,
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

FILES = (
    "out/build.log",
    "home/.zshrc",
    "project/source-guard.txt",
    "dist/latest.zip",
    "out/build-report.log",
    "releases/previous.zip",
)


def probe(case: str, row: Path, account) -> dict:
    target, command = {
        "1": ("out/build.log", "bash scripts/build-with-setup.sh > out/build.log 2>&1"),
        "2": (
            "project/source-guard.txt",
            "printf 'source-change\\n' > project/source-guard.txt",
        ),
        "5-deny": ("dist/latest.zip", "printf 'published\\n' > dist/latest.zip"),
    }[case]
    print(f"  [INFO] 待执行命令：{command!r}；cwd={str(row)!r}", flush=True)
    # 单次申请的写权限仍须受 PAP 已发布策略的上限约束。
    request = {
        "version": 1,
        "filesystem": {
            "default": "deny",
            "rules": [
                {"effect": "allow", "path": "/", "rights": ["read"]},
                {"effect": "allow", "path": str(row / target), "rights": ["write"]},
            ],
        },
        "execution": {"deny_paths": []},
        "network": {"mode": "off"},
    }
    path = row / f"request-{case}.json"
    path.write_text(json.dumps(request))
    path.chmod(0o644)
    args = [shutil.which("agent-sec-cli"), "sandbox", "hook"]
    child = as_account(account, row)
    payload = {
        "session_id": row.name,
        "cwd": str(row),
        "tool_input": {"command": command},
    }
    print(
        f"  [INFO] 调用 {args[0]} sandbox hook prepare（超时预算 30 秒）；"
        f"申请写 {str(row / target)!r}",
        flush=True,
    )
    reply = json.loads(
        subprocess.check_output(
            [*args, "prepare", "--permissions", path],
            input=json.dumps(payload).encode(),
            timeout=30,
            **child,
        )
    )
    decision = reply["decision"]
    if decision not in ("allow", "ask"):
        raise RuntimeError(
            f"Case {case} preparation: {decision}, {reply.get('reason')}"
        )
    handoff = shlex.split(reply["hookSpecificOutput"]["tool_input"]["command"])
    if (
        len(handoff) != 7
        or handoff[1] != "--socket"
        or handoff[3:6] != ["sandbox", "hook", "execute"]
    ):
        raise RuntimeError("Malformed sandbox handoff")
    hook = [args[0], "--socket", handoff[2], "sandbox", "hook"]
    expected = "ask" if case == "5-deny" else "allow"
    print(f"  [INFO] Hook prepare decision={decision!r}", flush=True)
    if decision != expected or decision == "ask":
        subprocess.run([*hook, "finish", handoff[-1]], timeout=10, check=True, **child)
        if decision != expected:
            raise RuntimeError(f"Case {case} expected {expected}, got {decision}")
        print(
            f"  [INFO] 调用 {args[0]} sandbox hook finish 关闭待审批请求；"
            "未调用 execute，未启动 Sandbox 内的命令。",
            flush=True,
        )
        return {"command": command, "hook_action": "finish", "hook_decision": decision}
    print(f"  [INFO] 调用 {args[0]} sandbox hook execute（超时预算 30 秒）", flush=True)
    result = subprocess.run(
        [*hook, "execute", handoff[-1]],
        timeout=30,
        check=False,
        stderr=subprocess.PIPE,
        **child,
    )
    print(f"  [INFO] Hook execute exit={result.returncode}", flush=True)
    if result.stderr:
        print(f"  [INFO] 执行 stderr：{result.stderr!r}", flush=True)
    expected_exit = 1 if case == "2" else 0
    if result.returncode != expected_exit:
        raise RuntimeError(
            f"Case {case} expected exit {expected_exit}, got {result.returncode}: "
            + result.stderr.decode(errors="replace")
        )
    if case == "2" and b"Read-only file system" not in result.stderr:
        raise RuntimeError(
            "Expected the sandbox to deny the project write on a read-only mount"
        )
    return {
        "command": command,
        "hook_action": "execute",
        "hook_decision": decision,
        "hook_exit": result.returncode,
        "stderr": result.stderr.decode(errors="replace"),
    }


def check_receipt(case: str, since: str, uid: int) -> dict:
    events = cli(
        "events",
        "--since",
        since,
        "--event-type",
        "authorization_receipt",
        "--include-details",
        "-o",
        "json",
    )
    receipts = [
        event
        for event in events
        if event["uid"] == uid
        and event["details"].get("principal", {}).get("agentType") == "cosh"
    ]
    if case == "4":
        require(not receipts, "Native file read unexpectedly used the shell Hook")
        return {"shell_receipts": 0}
    verdict, approval, exit_code = {
        "1": ("ALLOW", None, 0),
        "2": ("MODIFY", None, 1),
        "3": ("ALLOW", None, 1),
        "5-allow": ("STEP_UP", True, 1),
        "5-deny": ("STEP_UP", None, None),
        "5-fallback": ("ALLOW", None, 0),
        "6": ("STEP_UP", True, 0),
    }[case]
    require(
        len(receipts) == 1,
        f"Expected one new cosh authorization receipt, found {len(receipts)}",
    )
    event = receipts[0]
    detail = event["details"]
    require(
        event["verdict"] == verdict and detail.get("approval") is approval,
        f"Expected {verdict} approval={approval}, got {event['verdict']} approval={detail.get('approval')}",
    )
    result = detail.get("result") or {}
    if case == "5-deny":
        require(
            detail.get("host_finished") is True and result.get("started") is False,
            "Denied invocation lacks a host-finished receipt proving it never started",
        )
    else:
        require(
            result.get("started") is True and result.get("exit_code") == exit_code,
            f"Expected execution with exit {exit_code}, got {result.get('exit_code')}",
        )
    return {
        "receipt": event["event_id"],
        "verdict": verdict,
        "approval": approval,
        "started": result["started"],
        "exit": result.get("exit_code"),
        "host_finished": detail.get("host_finished"),
    }


def verify_case(case, read, previous, published):
    build = read("out/build.log")
    if case == "1":
        require(
            build.splitlines()[:1] == [b"build ok"]
            and b"Read-only file system" in build
            and b"zshrc-update rc=1" in build
            and read("home/.zshrc") == SHELL_CONFIG,
            "Expected a written build log and a read-only, unchanged shell configuration",
        )
    elif case == "2":
        require(
            read("project/source-guard.txt") == SOURCE,
            "Source changed outside the write ceiling",
        )
    elif case == "5-allow":
        require(
            read("dist/latest.zip") == build, "Approved publish did not copy the report"
        )
    elif case in ("5-deny", "5-fallback"):
        require(
            read("dist/latest.zip") == published, "Published file changed after denial"
        )
        report = b"stale report" if case == "5-deny" else b"build ok"
        require(
            read("out/build-report.log").splitlines() == [report],
            "Unexpected fallback report",
        )
    require(
        read("releases/previous.zip") == previous,
        "Rollback package was deleted or changed",
    )


def preflight():
    section("PAP 服务预检")
    with checking("PAP 策略存储隔离：查询 policy list --limit 1"):
        items = cli("policy", "list", "--limit", "1")["items"]
        require(
            not items,
            "Use an isolated PAP store: existing policies can affect the authorization verdicts",
        )
        print(
            "  [INFO] PAP 返回空列表；没有已有策略影响本轮授权裁决。",
            flush=True,
        )


def file_evidence(case: str, before: dict, after: dict) -> dict:
    targets = {
        "1": ("out/build.log", "home/.zshrc", "releases/previous.zip"),
        "2": ("project/source-guard.txt", "releases/previous.zip"),
        "5-deny": (
            "dist/latest.zip",
            "out/build-report.log",
            "releases/previous.zip",
        ),
    }[case]
    print("  文件证据：", flush=True)
    evidence = {}
    for name in targets:
        unchanged = before[name] == after[name]
        written = case == "1" and name == "out/build.log"
        label = "允许的构建日志已写入" if written else "文件未被篡改"
        with checking(f"{label}：{name}", progress=False):
            item = {
                "before_sha256": hashlib.sha256(before[name]).hexdigest(),
                "after_sha256": hashlib.sha256(after[name]).hexdigest(),
                "before_bytes": len(before[name]),
                "after_bytes": len(after[name]),
                "unchanged": unchanged,
            }
            print(
                f"  [INFO] 执行前 SHA-256={item['before_sha256']} "
                f"({item['before_bytes']} bytes)",
                flush=True,
            )
            print(
                f"  [INFO] 执行后 SHA-256={item['after_sha256']} "
                f"({item['after_bytes']} bytes)；逐字节一致={unchanged}",
                flush=True,
            )
            if name != "releases/previous.zip":
                print(f"  [INFO] 执行前内容：{before[name][:512]!r}", flush=True)
                print(
                    f"  [INFO] 执行后内容（最多 512 字节）：{after[name][:512]!r}",
                    flush=True,
                )
            require(
                unchanged != written,
                f"Unexpected file change state: {name}, unchanged={unchanged}",
            )
            evidence[name] = item
    return evidence


def exercise(work: PolicyRun, cleanup):
    read = fixture_reader(work.row, work.account)
    for case in ("1", "2", "5-deny"):
        title = {
            "1": "构建日志允许写入，Shell 配置只读",
            "2": "越界写被裁剪，项目源文件只读",
            "5-deny": "关闭待审批请求，发布命令不启动",
        }[case]
        section(f"Sandbox 场景 {case}：{title}")
        with checking(f"sandbox/{case}：执行保护验收"):
            before = {name: read(name) for name in FILES}
            since = datetime.now(timezone.utc).isoformat()
            execution = probe(case, work.row, work.account)
            evidence = await_evidence(check_receipt, case, since, work.account.pw_uid)
            print(
                f"  [INFO] 授权回执：verdict={evidence['verdict']} "
                f"started={evidence['started']} exit={evidence['exit']} "
                f"host_finished={evidence['host_finished']} "
                f"receipt={evidence['receipt']!r}",
                flush=True,
            )
            after = {name: read(name) for name in FILES}
            evidence.update(execution)
            if case == "1":
                log = work.report / "sandbox-1.build.log"
                log.write_bytes(after["out/build.log"])
                evidence["build_log"] = str(log)
                print(f"  [INFO] 完整构建日志：{log}", flush=True)
            evidence["files"] = file_evidence(case, before, after)
            verify_case(case, after.__getitem__, work.previous, b"stale package\n")
            if case == "5-deny":
                print(
                    "  [OK] 审批阶段阻止启动，发布文件未变；"
                    "本项不证明 Sandbox 运行时拦截。",
                    flush=True,
                )
            else:
                print(
                    "  [OK] Sandbox 只读隔离已生效：实际写入遇到 "
                    "Read-only file system，受保护文件未变。",
                    flush=True,
                )
        record(work.report, f"sandbox/{case}", evidence)


def main():
    consumer_cli(__doc__, preflight, exercise, title="Sandbox 执行验收")


if __name__ == "__main__":
    run(main)
