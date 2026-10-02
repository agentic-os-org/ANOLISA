#!/usr/bin/env python3
"""
sandbox-failure-handler.py - PostToolUseFailure hook
当 run_shell_command 工具因沙箱执行失败时触发。
解析出原始命令并发出 sandbox_bypass_request，触发 UI 层弹出审批对话框。

stdin: PostToolUseFailure JSON
  {
    "tool_name": "run_shell_command",
    "tool_input": {"command": "<sandboxed-cmd>"},
    "error": "<error message>",
    "error_type": "...",
    ...
  }

stdout: HookOutput JSON
  {
    "hookSpecificOutput": {
      "hookEventName": "PostToolUseFailure",
      "sandbox_bypass_request": {
        "original_command": "<original command>",
        "reason": "沙箱执行失败: <error>"
      }
    }
  }
  OR {} (no action needed)
"""

from __future__ import annotations

import base64
import json
import re
import sys

# 沙箱命令的特征前缀/模式
# sandbox-guard.py 生成的格式:
#   /usr/local/bin/linux-sandbox --sandbox-policy-cwd "..." ... -- bash -c 'ORIGINAL_CMD'
# 若原始命令含 sudo，则格式为:
#   ... -- bash -c 'COSH_RC=<base64> STRIPPED_CMD'
_SANDBOX_CMD_RE = re.compile(
    r"linux-sandbox\b.*?--\s+bash\s+-c\s+'((?:[^'\\]|\\.|'\\'')*)'",
    re.DOTALL,
)

# COSH_RC 编码的还原命令（sandbox-guard.py 在剥离 sudo 时嵌入）
_COSH_RC_RE = re.compile(r"\bCOSH_RC=([A-Za-z0-9+/=]+)")

# 已知的沙箱失败特征关键词（出现在 error 中）
SANDBOX_FAILURE_INDICATORS = [
    "linux-sandbox",
    "sandbox",
    "permission denied",
    "operation not permitted",
    "not permitted",
    "seccomp",
]


def unescape_bash_single_quote(s: str) -> str:
    """将 bash 单引号转义 '\\'' 还原为 '"""
    return s.replace("'\\''", "'")


def extract_original_command(sandboxed_cmd: str) -> str | None:
    """从 linux-sandbox 命令中提取用于 bypass 执行的原始命令。

    优先尝试 COSH_RC 环境变量（含 sudo 等完整前缀的 base64 编码），
    回退到从 bash -c '...' 中直接提取（无 sudo 的执行命令）。
    """
    # 优先：COSH_RC 存在时，解码 base64 获取完整原始命令（含 sudo）
    m = _COSH_RC_RE.search(sandboxed_cmd)
    if m:
        try:
            return base64.b64decode(m.group(1)).decode("utf-8")
        except Exception:
            pass
    # 回退：从 bash -c '...' 提取（无 sudo 版本）
    m = _SANDBOX_CMD_RE.search(sandboxed_cmd)
    if m:
        raw = m.group(1)
        return unescape_bash_single_quote(raw)
    return None


def is_sandbox_failure(tool_input_cmd: str, error_msg: str) -> bool:
    """判断是否是沙箱相关的失败"""
    # 命令本身包含 linux-sandbox 特征
    if "linux-sandbox" in tool_input_cmd:
        return True
    # 错误信息包含沙箱失败关键词
    error_lower = error_msg.lower()
    return any(kw in error_lower for kw in SANDBOX_FAILURE_INDICATORS)


def main():
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        print(json.dumps({}))
        return

    tool_input = input_data.get("tool_input", {})
    error_msg = input_data.get("error", "")

    sandboxed_cmd = tool_input.get("command", "")
    if not sandboxed_cmd:
        print(json.dumps({}))
        return

    # 判断是否是沙箱失败
    if not is_sandbox_failure(sandboxed_cmd, error_msg):
        print(json.dumps({}))
        return

    # 提取原始命令
    original_cmd = extract_original_command(sandboxed_cmd)
    if not original_cmd:
        # 无法提取原始命令，不发出 bypass 请求
        print(json.dumps({}))
        return

    # 截断过长的错误信息
    reason_detail = error_msg[:200] if len(error_msg) > 200 else error_msg

    result = {
        "hookSpecificOutput": {
            "hookEventName": "PostToolUseFailure",
            "sandbox_bypass_request": {
                "original_command": original_cmd,
                "reason": f"沙箱执行失败: {reason_detail}",
            },
        }
    }

    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    main()
