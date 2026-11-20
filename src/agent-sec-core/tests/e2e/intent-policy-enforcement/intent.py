"""验证意图服务，并导出供执行组验收的 task.cedar。"""

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import tempfile
import textwrap
from pathlib import Path

from support.common import (
    FIXTURES,
    SHARED_ROW,
    banner,
    check_host,
    checking,
    cli,
    record,
    require,
    run,
    section,
)

# A single-command candidate is short; cap the excerpt so a malformed model answer
# cannot flood the terminal.
CEDAR_LINE_LIMIT = 40


def visible(text: str) -> str:
    """Escape control characters; the Cedar text is model output, not a trusted terminal string."""
    return "".join(
        character if character.isprintable() else f"\\x{ord(character):02x}"
        for character in text
    )


def show_candidate(policy: str, caveats: list[str] | None) -> None:
    print("  候选 Cedar（未审批、未生效）：", flush=True)
    width = max(20, min(88, shutil.get_terminal_size((88, 24)).columns) - 4)
    lines = [
        wrapped
        for line in policy.splitlines()
        for wrapped in textwrap.wrap(visible(line), width=width) or [""]
    ]
    if not lines:
        print("    （空策略：不请求任何权限）", flush=True)
    for line in lines[:CEDAR_LINE_LIMIT]:
        print(f"    {line}", flush=True)
    if len(lines) > CEDAR_LINE_LIMIT:
        print(
            f"    ... 其余 {len(lines) - CEDAR_LINE_LIMIT} 行已省略",
            flush=True,
        )
    for caveat in caveats or ():
        print(f"  [INFO] 未覆盖：{visible(str(caveat))}", flush=True)


def check_intent() -> None:
    command = "cat /etc/os-release"
    cwd = str(SHARED_ROW.parent)
    section("命令意图")
    with checking("命令意图"):
        print(f"  命令：{command}（cwd={cwd}）", flush=True)
        print(
            "  请求：agent-sec-cli intent propose-command → daemon；超时预算 60 秒。",
            flush=True,
        )
        candidate = cli(
            "--timeout-ms",
            "60000",
            "intent",
            "propose-command",
            "--command",
            command,
            "--cwd",
            cwd,
            "--format",
            "json",
        )
        require(
            candidate.get("outcome") == "candidate",
            f"Intent preflight returned {candidate.get('outcome')}",
        )
        print(
            "  结果：outcome=candidate（符合预期）；未执行命令、未发布策略。",
            flush=True,
        )
        policy = candidate.get("policy")
        require(
            isinstance(policy, str),
            "Intent preflight returned no Cedar text; check the daemon and CLI builds",
        )
        show_candidate(policy, candidate.get("caveats"))


def draft_notes(cedar: str) -> list[str]:
    """Return the draft's own notes; they are shown but never block acceptance."""
    marker = next(
        (line for line in cedar.splitlines() if line.startswith("// unresolved:")),
        None,
    )
    if marker is None:
        return []
    try:
        items = json.loads(marker.removeprefix("// unresolved:").strip())
    except ValueError:
        return [marker]
    if not isinstance(items, list):
        return [marker]
    return [str(item) for item in items]


def check_draft(cedar: str, draft: Path) -> None:
    section("草案校验")
    # Notes are model-reported and drift with wording and model version, so acceptance
    # follows the statement shape that the consumer entries rely on instead.
    for note in draft_notes(cedar):
        print(f"  [INFO] 未决（不阻断）：{visible(note)}", flush=True)
    with checking("草案不含不适合发布的标记", progress=False):
        require(
            not any(
                note in cedar
                for note in (
                    "would widen authorization",
                    "// RUNTIME VIEW:",
                    "// LAUNCH VIEW:",
                )
            ),
            "Intent generator marked this draft unsuitable for publication",
        )
    statements = re.sub(r"(?m)^\s*//.*$", "", cedar)
    with checking("草案审批注解：3 条 @advice", progress=False):
        confirmations = statements.count("@advice(")
        require(
            confirmations == 3,
            f"Expected 3 @advice confirmations, found {confirmations}: {draft}",
        )
    for rule in ("R2", "R4", "R5"):
        with checking(f"草案审批注解：{rule}", progress=False):
            require(
                f'@id("{rule}")' in statements,
                f"Missing {rule} in generated Cedar: {draft}",
            )


def generate_policy(row: Path, report: Path) -> Path:
    task = (FIXTURES / "task.txt").read_text().replace("{workspace}", str(row))
    (report / "task.txt").write_text(task)
    draft = report / "task.cedar"
    section("任务策略生成")
    print(f"  草案文件：{draft}", flush=True)
    with checking("任务意图"):
        print(
            f"  请求：task.txt → daemon 意图服务；输入 {len(task)} 字符，超时预算 190 秒。",
            flush=True,
        )
        draft.write_bytes(
            subprocess.check_output(
                [
                    "agent-sec-cli",
                    "--timeout-ms",
                    "180000",
                    "intent",
                    "propose",
                    "--task",
                    task,
                    "--format",
                    "cedar",
                ],
                timeout=190,
            )
        )
        print(f"  生成：草案 {len(draft.read_bytes())} 字节", flush=True)
    check_draft(draft.read_text(), draft)
    return draft


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check", action="store_true", help="probe command intent only"
    )
    args = parser.parse_args()
    banner("意图策略验收", check=args.check)
    print("  不发布策略、不执行命令或探针。", flush=True)
    section("环境预检")
    check_host(progress=False)
    check_intent()
    if args.check:
        section("结果摘要")
        print("  命令意图链路通过；未生成任务策略或报告。", flush=True)
        print("PASS command intent", flush=True)
        return
    report = Path(tempfile.mkdtemp(prefix="anolisa-intent-report-", dir="/var/tmp"))
    print(f"\nRun report: {report}", flush=True)
    draft = generate_policy(SHARED_ROW, report)
    digest = hashlib.sha256(draft.read_bytes()).hexdigest()
    section("结果摘要")
    print("  草案检查通过（未审批、未生效）。", flush=True)
    print(f"  SHA-256：{digest}", flush=True)
    print("  下一步：将以下策略文件交给执行组验收。", flush=True)
    print(f"Consumer input: --policy {draft}", flush=True)
    record(report, "intent", {"policy_file": str(draft), "sha256": digest})


if __name__ == "__main__":
    run(main)
