#!/usr/bin/env python3
"""
sandbox-guard.py - PreToolUse hook
检测危险 shell 命令，自动替换为 linux-sandbox 沙箱内执行。

沙箱策略：
  - 文件系统：root 只读 + cwd 可写 + tmpdir 可写
  - 网络：根据命令类型自动选择（restricted 或 unrestricted）

stdin: PreToolUse JSON (tool_name, tool_input, cwd, ...)
stdout: HookOutput JSON (decision, systemMessage, hookSpecificOutput)
"""

import base64
import json
import os
import re
import shutil
import subprocess
import sys
from typing import Any

from trace_context import with_trace_context


def _log_sandbox_event(
    input_data: dict[str, Any], action: str = "log-sandbox", **kwargs: Any
) -> None:
    """Log security event via agent-sec-cli CLI (subprocess call).

    Falls back silently if agent-sec-cli is not installed.

    Args:
        input_data: Hook payload used to extract optional trace context.
        action: CLI subcommand name (default: 'log_sandbox')
        **kwargs: Action-specific parameters
    """
    try:
        # Check if agent-sec-cli is available
        if shutil.which("agent-sec-cli") is None:
            return

        # Build command: agent-sec-cli <action> [args...]
        cmd = ["agent-sec-cli", action]

        # Convert kwargs to CLI arguments
        for key, value in kwargs.items():
            if value is not None:
                cmd.append(f"--{key.replace('_', '-')}")
                cmd.append(str(value))

        cmd = with_trace_context(cmd, input_data)

        # Execute asynchronously to avoid blocking the hook.
        # start_new_session=True detaches the child into its own session so
        # it is reparented to init(1) once this hook process exits, preventing
        # zombie accumulation in long-running Agent Loop sessions.
        subprocess.Popen(
            cmd,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
    except Exception:
        # Silently ignore any errors - logging must not affect hook behavior
        pass


LINUX_SANDBOX = shutil.which("linux-sandbox") or "/usr/local/bin/linux-sandbox"

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# 规则收敛设计说明
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
#
# 当前启用的规则集为「默认策略」，仅保留高风险且误报率低的核心规则。
# 用户可根据自身安全需求，取消注释或新增规则来定制更精细化的拦截策略。
#
# 收敛原因：
#   1. 减少对用户的频繁打扰。在安全与易用性之间取平衡，优先保障用户体验的流畅性。
#   2. 实际场景验证：此前规则过宽时（如包管理器、systemctl、mount 等全部启用），
#      用户在执行 openclaw 安装等正常运维操作时会被频繁拦截/弹窗确认，
#      严重影响工作效率和使用体验。
#   3. 被注释掉的规则并非无意义，而是作为「可选加固项」保留，供安全要求更高的
#      场景（如生产环境、多租户环境）按需启用。
#
# 定制方式：
#   - 取消注释已收敛的规则即可启用对应拦截
#   - 新增自定义规则：添加 (regex_pattern, reason_label) 元组到对应列表
#   - 通过 `/hooks disable sandbox-guard` 可临时关闭整个沙箱防护
#
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

# 危险命令检测规则：(regex_pattern, reason_label)
# 分为两类：
#   BLOCK_PATTERNS  - 直接阻止，不进沙箱（沙箱内也无法缓解的风险）
#   SANDBOX_PATTERNS - 替换为沙箱执行（文件系统/权限类风险，沙箱可有效隔离）

# 直接 block 的命令（沙箱无法缓解）
BLOCK_PATTERNS = [
    (r"\bshutdown\b", "shutdown 关机命令"),
    (r"\breboot\b", "reboot 重启命令"),
    (r"\bhalt\b", "halt 停机命令"),
    (r"\bpoweroff\b", "poweroff 断电命令"),
    (r":\(\)\s*\{", "fork bomb"),
]

# 替换为沙箱执行的命令（文件系统/权限/服务类）- 网络隔离
# 注意：sudo 不在此列表，由 strip_sudo() 预处理后对底层命令评估
DANGEROUS_PATTERNS = [
    # rm 危险操作：-rf、-fr、-r -f、--recursive、--force 各种写法
    (r"\brm\b.*(-[a-zA-Z]*[rf]|-[a-zA-Z]*[fr]|--recursive|--force)", "递归/强制删除"),
    (r"\bchmod\s+[0-7]{3,4}\s+/", "修改系统路径权限"),
    (r"\bchown\b", "修改文件所有者"),
    (r"\b(cp|mv)\s+.*\s+/etc/", "cp/mv 操作 /etc"),
    (r"\b(cp|mv)\s+.*\s+/usr/", "cp/mv 操作 /usr"),
    (r"\b(cp|mv)\s+.*\s+/var/", "cp/mv 操作 /var"),
    # ── 以下规则已收敛，按需恢复 ──
    # (r"\bsu\b", "su 切换用户"),
    # (r"\bpkexec\b", "pkexec 提权"),
    # (r"\bmkfs\.?\w*\b", "格式化磁盘"),
    # (r"\bdd\s+(if|of)=", "dd 磁盘读写操作"),
    # 写入系统目录：> / tee / cp / mv 等多种方式
    # (r"(>|>>)\s*/etc/", "重定向写入 /etc"),
    # (r"(>|>>)\s*/usr/", "重定向写入 /usr"),
    # (r"(>|>>)\s*/var/", "重定向写入 /var"),
    # (r"(>|>>)\s*/boot/", "重定向写入 /boot"),
    # (r"\btee\s+.*/etc/", "tee 写入 /etc"),
    # (r"\btee\s+.*/usr/", "tee 写入 /usr"),
    # (r"\btee\s+.*/var/", "tee 写入 /var"),
    # (r"\bsystemctl\s+(stop|disable|mask|restart|kill)", "systemctl 危险操作"),
    # (r"\bservice\s+\w+\s+(stop|restart)", "service 危险操作"),
    # (r"\bkill\s+-9\b", "强制杀进程 SIGKILL"),
    # (r"\bkillall\b", "killall 批量杀进程"),
    # (r"\bmount\b", "挂载文件系统"),
    # (r"\bumount\b", "卸载文件系统"),
    # (r"\biptables\b", "iptables 修改防火墙"),
    # (r"\bnft\b", "nftables 修改防火墙"),
    # (r"\bcrontab\s+(-[re]|.*\|)", "crontab 修改定时任务"),
]

# 网络相关命令 - 需要放开网络权限，但保留文件系统隔离
NETWORK_PATTERNS = [
    (r"\bcurl\b", "curl 网络请求"),
    (r"\bwget\b", "wget 网络下载"),
    # 管道执行网络内容（curl/wget pipe to shell）
    (
        r"(curl|wget)\b.*(\|\s*(bash|sh|python|python3|perl|ruby|node))",
        "网络内容直接执行",
    ),
    (r"(\|\s*(bash|sh|python|python3)).*\b(curl|wget)\b", "网络内容直接执行(反向管道)"),
    # ── 以下规则已收敛，按需恢复 ──
    # (r"\bnc\b|\bnetcat\b", "netcat 网络工具"),
    # (r"\bnmap\b", "nmap 网络扫描"),
    # 包管理器（已收敛）：
    # 注意：以下包管理器规则默认不启用。如启用，因沙箱文件系统只读，包管理器
    # 写入系统目录会失败，从而触发 sandbox-failure-handler bypass 弹框让用户决策。
    # 默认不启用的原因：安装 openclaw 等正常运维流程会频繁触发弹框，严重影响体验。
    # (r"\byum\s+\S", "yum 包管理"),
    # (r"\bdnf\s+\S", "dnf 包管理"),
    # (r"\bapt\s+\S", "apt 包管理"),
    # (r"\bapt-get\s+\S", "apt-get 包管理"),
    # (r"\bapt-cache\s+\S", "apt-cache 包管理"),
    # (r"\bpip[23]?\s+\S", "pip 包管理"),
    # (r"\bnpm\s+\S", "npm 包管理"),
    # (r"\bpnpm\s+\S", "pnpm 包管理"),
    # (r"\byarn\s+\S", "yarn 包管理"),
    # (r"\bgem\s+\S", "gem 包管理"),
    # (r"\bcargo\s+(install|add|update)\b", "cargo 包管理"),
    # ssh/scp 远程连接（已收敛）：
    # 安全论证：ssh/scp 本身需要认证凭据，无凭据时无法建立连接，风险可控。
    # 且开发者频繁使用 ssh 进行远程调试/部署，误报率较高。
    # 如需启用，取消注释以下规则：
    # (r"\bssh\s+[^/\s]", "ssh 远程连接"),
    # (r"\bscp\b", "scp 远程传输"),
    # 脚本语言网络操作（已收敛 - 误报率高，正常开发频繁触发）
    # (r"python[23]?\b.*\bsocket\b", "Python socket 网络操作"),
    # (
    #     r"python[23]?\b.*\b(requests|urllib|aiohttp|httpx|httplib)\b",
    #     "Python HTTP 网络请求",
    # ),
    # (r"python[23]?\b.*\.connect\(", "Python 建立网络连接"),
    # (r"\bnode\b.*\b(http|https|net|dgram)\b", "Node.js 网络模块"),
    # (r"\bperl\b.*\b(socket|IO::Socket|LWP)\b", "Perl 网络操作"),
]

# 沙箱文件系统策略 JSON
SANDBOX_FS_POLICY = json.dumps(
    {
        "kind": "restricted",
        "entries": [
            {"path": {"type": "special", "value": {"kind": "root"}}, "access": "read"},
            {
                "path": {
                    "type": "special",
                    "value": {"kind": "current_working_directory"},
                },
                "access": "write",
            },
            {
                "path": {"type": "special", "value": {"kind": "tmpdir"}},
                "access": "write",
            },
            {
                "path": {"type": "special", "value": {"kind": "slash_tmp"}},
                "access": "write",
            },
        ],
    },
    separators=(",", ":"),
)  # compact JSON

# 匹配 sudo 及其常见无交互选项，覆盖自动化脚本最常见场景：
#   sudo cmd
#   sudo -n cmd
#   sudo -u root cmd
#   sudo -E -n cmd
#   sudo -- cmd
# 不尝试覆盖 -i/-s（交互式 shell），这类保留原始命令不处理。
_SUDO_PREFIX_RE = re.compile(
    r"^sudo\s+"
    r"(?:(?:-[nEbkKHPvA]+|--(?:non-interactive|preserve-env|reset-timestamp))\s+)*"  # 无参选项
    r"(?:(?:-[uUgcCTt])\s+\S+\s+)*"  # 带参选项（-u user 等）
    r"(?:--\s+)?"  # 分隔符 --
)


def strip_sudo(command: str) -> tuple:
    """剥离命令开头的 sudo 前缀及其非交互选项。

    Returns:
        (stripped_command, had_sudo): 剥离后命令 及 是否包含 sudo 前缀
    """
    cmd = command.strip()
    m = _SUDO_PREFIX_RE.match(cmd)
    if m:
        rest = cmd[m.end() :].strip()
        # 确保剥离后仍有命令体（不是空串），且不是交互式 shell 调用
        if rest and not re.match(r"^-[is]\b", rest):
            return rest, True
    return command, False


def build_sandbox_command(
    original_command: str,
    cwd: str,
    network_policy: str = "restricted",
    restore_command: str = "",
) -> str:
    """将原始命令包裹进 linux-sandbox 执行

    Args:
        original_command: 沙箱内实际执行的命令（已剥离 sudo）
        cwd: 当前工作目录
        network_policy: 网络策略，"restricted" 或 "unrestricted"
        restore_command: bypass 时还原执行的命令（含 sudo 等完整前缀）；
                         与 original_command 相同或为空时不编码
    """
    # 转义单引号：' → '\''
    escaped_cmd = original_command.replace("'", "'\\''")

    # 若 restore_command 与实际执行命令不同（如含 sudo），将其 base64 编码
    # 嵌入为沙箱内 bash 的临时环境变量 COSH_RC，不影响命令执行语义，
    # sandbox-failure-handler.py 在 bypass 时还原完整原始命令。
    if restore_command and restore_command != original_command:
        b64 = base64.b64encode(restore_command.encode()).decode()
        full_bash_cmd = f"COSH_RC={b64} {escaped_cmd}"
    else:
        full_bash_cmd = escaped_cmd

    return (
        f"{LINUX_SANDBOX}"
        f' --sandbox-policy-cwd "{cwd}"'
        f" --file-system-sandbox-policy '{SANDBOX_FS_POLICY}'"
        f" --network-sandbox-policy '\"{network_policy}\"'"
        f" -- bash -c '{full_bash_cmd}'"
    )


def detect_patterns(command: str, patterns: list) -> list:
    """检测命令中的危险模式，返回匹配的原因列表"""
    reasons = []
    for pattern, reason in patterns:
        if re.search(pattern, command, re.IGNORECASE):
            reasons.append(reason)
    return reasons


def main():
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        # 无法解析输入，安全放行
        print(json.dumps({"decision": "allow"}))
        return

    tool_input = input_data.get("tool_input", {})
    command = tool_input.get("command", "")
    cwd = input_data.get("cwd", "/tmp")

    # matcher 已保证只在 shell 类工具上触发，这里只剩空命令短路。
    if not command.strip():
        print(json.dumps({"decision": "allow"}))
        return

    # ── sudo 预处理
    # 剥离 sudo 前缀后对底层命令做统一的危险性评估，避免把整条命令
    # 塞进沙箱（沙箱内 sudo 因权限受限必然失败）。
    stripped_command, has_sudo = strip_sudo(command)
    is_root = os.getuid() == 0

    # 用剥离 sudo 后的命令做危险检测（以底层命令的语义为准）
    eval_command = stripped_command if has_sudo else command

    # 第一优先级：直接 block 的命令
    block_reasons = detect_patterns(eval_command, BLOCK_PATTERNS)
    if block_reasons:
        reasons_str = ", ".join(block_reasons)
        result = {
            "decision": "block",
            "reason": (
                f"🚫 安全策略已阻止执行 (检测到: {reasons_str})。此类命令不允许执行。\n"
                "💡 如确认当前命令无风险，可在聊天框输入 `/hooks disable sandbox-guard` 临时关闭沙箱防护（本会话有效），"
                "执行完毕后可用 `/hooks enable sandbox-guard` 恢复。"
            ),
        }
        # --- middleware prehook logging (additive) ---
        _log_sandbox_event(
            input_data,
            decision="block",
            command=command,
            reasons=", ".join(block_reasons),
            cwd=cwd,
        )

        print(json.dumps(result, ensure_ascii=False))
        return

    # 第二优先级：替换为沙箱执行（文件系统/权限类风险）
    sandbox_reasons = detect_patterns(eval_command, DANGEROUS_PATTERNS)
    network_reasons = detect_patterns(eval_command, NETWORK_PATTERNS)

    if not sandbox_reasons and not network_reasons:
        # 底层命令安全，根据 sudo + 身份决策：
        #
        #   root + sudo：sudo 在此上下文中多余，剥离后直接执行，
        #   避免"root 执行 sudo"因沙箱权限报错。
        #
        #   非 root + sudo：保留 sudo，让系统正常完成权限提升后执行。
        #
        #   无 sudo：直接放行。
        if has_sudo and is_root:
            result = {
                "decision": "allow",
                "systemMessage": (
                    "ℹ️ 检测到 root 用户使用 sudo，已自动剥离 sudo 前缀直接执行"
                    "（root 不需要 sudo 进行权限提升）。"
                ),
                "hookSpecificOutput": {"tool_input": {"command": stripped_command}},
            }
            print(json.dumps(result, ensure_ascii=False))
        else:
            # 安全命令（含非 root 的合法 sudo），直接放行
            print(json.dumps({"decision": "allow"}))
        return

    # 底层命令危险：进沙箱隔离。
    # 沙箱内不支持 sudo/pkexec 等提权机制，因此始终以剥离后的命令入沙箱，
    # 并通过 systemMessage 告知用户 sudo 已被忽略、命令在受限环境中执行。
    sandbox_source = stripped_command if has_sudo else command

    # 判断是否需要放开网络权限
    if network_reasons and not sandbox_reasons:
        # 纯网络命令：放开网络，但保留文件系统隔离
        network_policy = "enabled"
        all_reasons = network_reasons
        policy_desc = "🔒 已替换安全沙箱执行（网络已放行）"
    else:
        # 文件系统危险命令或混合命令：网络隔离
        network_policy = "restricted"
        all_reasons = sandbox_reasons + network_reasons
        policy_desc = "🔒 已替换安全沙箱执行（网络隔离）"

    sudo_strip_note = (
        "\nℹ️ 检测到 sudo 前缀：沙箱不支持权限提升，已自动剥离 sudo 后在受限环境中执行。"
        if has_sudo
        else ""
    )

    # 构建沙箱命令
    # sandbox_source 是沙箱内实际执行的命令（已剥离 sudo）
    # command 是用户输入的原始完整命令（含 sudo），bypass 时用于还原执行
    sandbox_cmd = build_sandbox_command(
        sandbox_source, cwd, network_policy, restore_command=command
    )
    reasons_str = ", ".join(all_reasons)

    result = {
        "decision": "allow",
        "systemMessage": (
            f"{policy_desc} (检测到: {reasons_str}){sudo_strip_note}\n"
            "💡 如沙箱执行出错且确认命令无风险，可在聊天框输入 `/hooks disable sandbox-guard` 临时关闭防护"
        ),
        "hookSpecificOutput": {"tool_input": {"command": sandbox_cmd}},
    }

    # --- middleware prehook logging (additive) ---
    _log_sandbox_event(
        input_data,
        decision="sandbox",
        command=command,
        reasons=", ".join(all_reasons),
        network_policy=network_policy,
        cwd=cwd,
    )

    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    main()
