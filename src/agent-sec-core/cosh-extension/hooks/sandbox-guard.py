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
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from sandbox_authorization import (
    all_shell_enabled,
    authorization_error,
    finish_authorized,
    prepare_authorized,
)
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


def _find_linux_sandbox() -> str:
    """Respect PATH before falling back to system and legacy install locations."""
    candidates = (
        shutil.which("linux-sandbox"),
        "/usr/bin/linux-sandbox",
        "/usr/local/bin/linux-sandbox",
    )
    for candidate in candidates:
        if candidate and os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return "/usr/bin/linux-sandbox"


LINUX_SANDBOX = _find_linux_sandbox()

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


@dataclass(frozen=True)
class _ShellToken:
    """单个 shell 词法单元，保留未引号重定向操作符与引号/转义来源信息。"""

    text: str
    is_redirect: bool = False
    quoted: bool = False
    has_malformed_expansion: bool = False


@dataclass(frozen=True)
class _ShellSegment:
    """单个子命令片段的词法 token 列表及嵌套命令/进程替换片段。"""

    tokens: list[_ShellToken]
    substitutions: list[str]


@dataclass(frozen=True)
class _WrapperSpec:
    """受支持命令包装器的选项带参规格。"""

    short_flag_opts: frozenset[str] = frozenset()
    short_arg_opts: frozenset[str] = frozenset()
    short_opt_arg_opts: frozenset[str] = frozenset()
    long_flag_opts: frozenset[str] = frozenset()
    long_arg_opts: frozenset[str] = frozenset()
    long_opt_arg_opts: frozenset[str] = frozenset()
    positional_args: int = 0
    allow_env_assign: bool = False
    allow_numeric_short_opt: bool = False
    allow_lone_dash_flag: bool = False


_ANSI_C_SIMPLE_ESCAPES: dict[str, str] = {
    "a": "\a",
    "b": "\b",
    "e": "\x1b",
    "E": "\x1b",
    "f": "\f",
    "n": "\n",
    "r": "\r",
    "t": "\t",
    "v": "\v",
    "\\": "\\",
    "'": "'",
    '"': '"',
    "?": "?",
}
_HEX_DIGITS = frozenset("0123456789abcdefABCDEF")
_OCT_DIGITS = frozenset("01234567")
_REDIRECT_OPS_3 = frozenset({"<<<", "&>>", "<<-"})
_REDIRECT_OPS_2 = frozenset({"&>", ">>", "<<", "<>", ">&", "<&", ">|"})
_CONTROL_OPS_2 = frozenset({"&&", "||", "|&", ";;", ";&"})
_CONTROL_OPS_1 = frozenset(";|&()")
_SHELL_LEADING_KEYWORDS = frozenset(
    {
        "{",
        "}",
        "!",
        "if",
        "then",
        "else",
        "elif",
        "while",
        "until",
        "do",
    }
)
_ENV_ASSIGN_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=")
_NUMERIC_SHORT_OPT_RE = re.compile(r"^-[+-]?\d+$")

_WRAPPER_SPECS: dict[str, _WrapperSpec] = {
    "sudo": _WrapperSpec(
        short_flag_opts=frozenset("AbBEeiKklnPSsvV"),
        short_arg_opts=frozenset("uUgcCDhprRtT"),
        long_flag_opts=frozenset(
            {
                "--askpass",
                "--background",
                "--bell",
                "--edit",
                "--help",
                "--list",
                "--login",
                "--non-interactive",
                "--preserve-groups",
                "--remove-timestamp",
                "--reset-timestamp",
                "--set-home",
                "--shell",
                "--stdin",
                "--validate",
                "--version",
            }
        ),
        long_arg_opts=frozenset(
            {
                "--chdir",
                "--chroot",
                "--close-from",
                "--command-timeout",
                "--group",
                "--host",
                "--login-class",
                "--other-user",
                "--prompt",
                "--role",
                "--type",
                "--user",
            }
        ),
        long_opt_arg_opts=frozenset({"--preserve-env"}),
        allow_env_assign=True,
    ),
    "doas": _WrapperSpec(
        short_flag_opts=frozenset("LnSs"),
        short_arg_opts=frozenset("Cua"),
    ),
    "pkexec": _WrapperSpec(
        long_flag_opts=frozenset(
            {"--disable-internal-agent", "--keep-cwd", "--help", "--version"}
        ),
        long_arg_opts=frozenset({"--user"}),
    ),
    "nice": _WrapperSpec(
        short_arg_opts=frozenset("n"),
        long_flag_opts=frozenset({"--help", "--version"}),
        long_arg_opts=frozenset({"--adjustment"}),
        allow_numeric_short_opt=True,
    ),
    "nohup": _WrapperSpec(
        long_flag_opts=frozenset({"--help", "--version"}),
    ),
    "time": _WrapperSpec(
        short_flag_opts=frozenset("apqvV"),
        short_arg_opts=frozenset("fo"),
        long_flag_opts=frozenset(
            {"--append", "--portability", "--quiet", "--verbose", "--version", "--help"}
        ),
        long_arg_opts=frozenset({"--format", "--output"}),
    ),
    "timeout": _WrapperSpec(
        short_flag_opts=frozenset("fPv"),
        short_arg_opts=frozenset("ks"),
        long_flag_opts=frozenset(
            {"--foreground", "--preserve-status", "--verbose", "--help", "--version"}
        ),
        long_arg_opts=frozenset({"--kill-after", "--signal"}),
        positional_args=1,
    ),
    "stdbuf": _WrapperSpec(
        short_arg_opts=frozenset("ioe"),
        long_flag_opts=frozenset({"--help", "--version"}),
        long_arg_opts=frozenset({"--input", "--output", "--error"}),
    ),
    "ionice": _WrapperSpec(
        short_flag_opts=frozenset("thV"),
        short_arg_opts=frozenset("cnpPu"),
        long_flag_opts=frozenset({"--ignore", "--help", "--version"}),
        long_arg_opts=frozenset({"--class", "--classdata", "--pid", "--pgid", "--uid"}),
    ),
    "taskset": _WrapperSpec(
        short_flag_opts=frozenset("apchV"),
        long_flag_opts=frozenset(
            {"--all-tasks", "--pid", "--cpu-list", "--help", "--version"}
        ),
        positional_args=1,
    ),
    "chroot": _WrapperSpec(
        long_flag_opts=frozenset({"--skip-chdir", "--help", "--version"}),
        long_arg_opts=frozenset({"--userspec", "--groups"}),
        positional_args=1,
    ),
    "strace": _WrapperSpec(
        short_flag_opts=frozenset("cCdDfFikqrTtVvwxXyYzZh"),
        short_arg_opts=frozenset("abeEoOpPsSuUI"),
        long_flag_opts=frozenset(
            {
                "--summary-only",
                "--summary",
                "--debug",
                "--follow-forks",
                "--instruction-pointer",
                "--stack-traces",
                "--quiet",
                "--relative-timestamps",
                "--absolute-timestamps",
                "--syscall-times",
                "--no-abbrev",
                "--version",
                "--summary-wall-clock",
                "--strings-in-hex",
                "--decode-fds",
                "--decode-pids",
                "--successful-only",
                "--failed-only",
                "--help",
                "--seccomp-bpf",
            }
        ),
        long_arg_opts=frozenset(
            {
                "--columns",
                "--detach-on",
                "--trace",
                "--signal",
                "--status",
                "--trace-fds",
                "--abbrev",
                "--verbose",
                "--raw",
                "--read",
                "--write",
                "--inject",
                "--fault",
                "--env",
                "--output",
                "--output-separately",
                "--overhead",
                "--attach",
                "--trace-path",
                "--string-limit",
                "--summary-sort-by",
                "--user",
                "--output-format",
                "--interruptible",
            }
        ),
    ),
    "ltrace": _WrapperSpec(
        short_flag_opts=frozenset("bcCdfiLnrStTVh"),
        short_arg_opts=frozenset("aADeFlopsuwx"),
        long_flag_opts=frozenset(
            {"--demangle", "--debug", "--no-signals", "--version", "--help"}
        ),
        long_arg_opts=frozenset(
            {
                "--align",
                "--config",
                "--filter",
                "--indent",
                "--library",
                "--output",
                "--attach",
                "--strsize",
                "--username",
            }
        ),
    ),
    "setsid": _WrapperSpec(
        short_flag_opts=frozenset("cfwhV"),
        long_flag_opts=frozenset({"--ctty", "--fork", "--wait", "--help", "--version"}),
    ),
    "command": _WrapperSpec(
        short_flag_opts=frozenset("pvV"),
    ),
    "builtin": _WrapperSpec(),
    "exec": _WrapperSpec(
        short_flag_opts=frozenset("cl"),
        short_arg_opts=frozenset("a"),
    ),
    "xargs": _WrapperSpec(
        short_flag_opts=frozenset("0oprtx"),
        short_arg_opts=frozenset("adEILnPs"),
        short_opt_arg_opts=frozenset("eil"),
        long_flag_opts=frozenset(
            {
                "--null",
                "--open-tty",
                "--interactive",
                "--no-run-if-empty",
                "--verbose",
                "--exit",
                "--show-limits",
                "--help",
                "--version",
            }
        ),
        long_arg_opts=frozenset(
            {
                "--arg-file",
                "--delimiter",
                "--max-args",
                "--max-procs",
                "--max-chars",
                "--process-slot-var",
            }
        ),
        long_opt_arg_opts=frozenset({"--eof", "--replace", "--max-lines"}),
    ),
    "unshare": _WrapperSpec(
        short_flag_opts=frozenset("mupinUCrTfkVh"),
        short_arg_opts=frozenset("RwgS"),
        long_flag_opts=frozenset(
            {
                "--mount",
                "--uts",
                "--ipc",
                "--net",
                "--pid",
                "--user",
                "--cgroup",
                "--time",
                "--fork",
                "--map-root-user",
                "--map-current-user",
                "--keep-caps",
                "--help",
                "--version",
            }
        ),
        long_arg_opts=frozenset(
            {
                "--root",
                "--wd",
                "--setgroups",
                "--propagation",
                "--map-user",
                "--map-group",
                "--kill-child",
                "--mount-proc",
            }
        ),
    ),
    "chrt": _WrapperSpec(
        short_flag_opts=frozenset("abdfiomprRvhV"),
        long_flag_opts=frozenset(
            {
                "--all-tasks",
                "--batch",
                "--deadline",
                "--fifo",
                "--idle",
                "--other",
                "--pid",
                "--rr",
                "--reset-on-fork",
                "--verbose",
                "--help",
                "--version",
            }
        ),
        positional_args=1,
    ),
    "watch": _WrapperSpec(
        short_flag_opts=frozenset("bcegpqtxhV"),
        short_arg_opts=frozenset("dn"),
        long_flag_opts=frozenset(
            {
                "--beep",
                "--color",
                "--errexit",
                "--chgexit",
                "--precise",
                "--equexit",
                "--no-title",
                "--exec",
                "--help",
                "--version",
            }
        ),
        long_arg_opts=frozenset({"--interval"}),
        long_opt_arg_opts=frozenset({"--differences"}),
    ),
    "busybox": _WrapperSpec(),
}

_ENV_SHORT_FLAG_OPTS = frozenset("i0v")
_ENV_SHORT_ARG_OPTS = frozenset("uCa")
_ENV_LONG_FLAG_OPTS = frozenset(
    {
        "--ignore-environment",
        "--null",
        "--debug",
        "--help",
        "--version",
        "--list-signal-handling",
    }
)
_ENV_LONG_ARG_OPTS = frozenset({"--unset", "--chdir", "--argv0"})
_ENV_LONG_OPT_ARG_OPTS = frozenset(
    {"--default-signal", "--ignore-signal", "--block-signal"}
)

_SU_SHORT_FLAG_OPTS = frozenset("flmpPhV")
_SU_SHORT_ARG_OPTS = frozenset("gGsw")
_SU_LONG_FLAG_OPTS = frozenset(
    {"--fast", "--login", "--preserve-environment", "--pty", "--help", "--version"}
)
_SU_LONG_ARG_OPTS = frozenset(
    {"--group", "--supp-group", "--shell", "--whitelist-environment"}
)
_SU_CMD_LONG_OPTS = frozenset({"--command", "--session-command"})

_FIND_EXEC_FLAGS = frozenset({"-exec", "-execdir", "-ok", "-okdir"})
_SHELL_INTERPRETERS = frozenset({"bash", "sh", "zsh", "dash", "ksh", "ash", "mksh"})
_SHELL_LONG_ARG_OPTS = frozenset({"--rcfile", "--init-file"})
_SHELL_LONG_FLAG_OPTS = frozenset(
    {
        "--debugger",
        "--dump-po-strings",
        "--dump-strings",
        "--help",
        "--login",
        "--noediting",
        "--noprofile",
        "--norc",
        "--posix",
        "--restricted",
        "--verbose",
        "--version",
    }
)
_SPECIAL_WRAPPERS = frozenset({"env", "su", "runuser", "eval", "find"})
_ALL_KNOWN_WRAPPERS = (
    frozenset(_WRAPPER_SPECS) | _SPECIAL_WRAPPERS | _SHELL_INTERPRETERS
)
_MAX_RM_PARSE_DEPTH = 8


def _rm_args_have_dangerous_flag(args: list[_ShellToken]) -> bool:
    """判断 rm 参数列表中是否包含递归/强制删除选项（遵循 `--` 结束选项解析规则）。

    仅跳过由未引号/未转义重定向操作符引出的重定向目标；引号/转义形式的 `">"` 或 `\\>`
    是普通文件名操作数，不会吞噬后续参数。
    """
    skip_redirect_target = False
    for token in args:
        if skip_redirect_target:
            skip_redirect_target = False
            continue
        if token.is_redirect:
            skip_redirect_target = True
            continue
        if token.has_malformed_expansion:
            return True
        arg = token.text
        if arg == "--":
            return False
        if arg.startswith("--") and len(arg) >= 3:
            if "--recursive".startswith(arg) or "--force".startswith(arg):
                return True
            continue
        if arg.startswith("-") and not arg.startswith("--") and len(arg) > 1:
            flags = arg[1:]
            if flags.isalpha() and any(flag in "rRf" for flag in flags):
                return True
    return False


def _decode_ansi_c_quoted(command: str, start: int) -> tuple[str, int, bool]:
    """按 Bash ANSI-C 引号 `$'...'` 语义解码字符串。

    Returns:
        (decoded_text, next_index, is_malformed)
    """
    idx = start + 2
    length = len(command)
    out: list[str] = []
    malformed = False

    while idx < length:
        ch = command[idx]
        if ch == "'":
            return "".join(out), idx + 1, malformed
        if ch == "\x00":
            malformed = True
            idx += 1
            continue
        if ch != "\\":
            out.append(ch)
            idx += 1
            continue

        idx += 1
        if idx >= length:
            out.append("\\")
            return "".join(out), idx, True

        esc = command[idx]
        if esc in _ANSI_C_SIMPLE_ESCAPES:
            out.append(_ANSI_C_SIMPLE_ESCAPES[esc])
            idx += 1
            continue
        if esc == "x":
            hex_start = idx + 1
            hex_end = hex_start
            while (
                hex_end < length
                and hex_end - hex_start < 2
                and command[hex_end] in _HEX_DIGITS
            ):
                hex_end += 1
            if hex_end > hex_start:
                byte_val = int(command[hex_start:hex_end], 16)
                if byte_val == 0:
                    malformed = True
                else:
                    out.append(chr(byte_val))
                idx = hex_end
            else:
                out.append("\\x")
                malformed = True
                idx += 1
            continue
        if esc in ("u", "U"):
            max_digits = 4 if esc == "u" else 8
            hex_start = idx + 1
            hex_end = hex_start
            while (
                hex_end < length
                and hex_end - hex_start < max_digits
                and command[hex_end] in _HEX_DIGITS
            ):
                hex_end += 1
            if hex_end > hex_start:
                codepoint = int(command[hex_start:hex_end], 16)
                if 0 < codepoint <= 0x10FFFF and not (0xD800 <= codepoint <= 0xDFFF):
                    out.append(chr(codepoint))
                else:
                    malformed = True
                idx = hex_end
            else:
                out.append("\\" + esc)
                malformed = True
                idx += 1
            continue
        if esc in _OCT_DIGITS:
            oct_end = idx
            while (
                oct_end < length
                and oct_end - idx < 3
                and command[oct_end] in _OCT_DIGITS
            ):
                oct_end += 1
            oct_val = int(command[idx:oct_end], 8) & 0xFF
            if oct_val == 0:
                malformed = True
            else:
                out.append(chr(oct_val))
            idx = oct_end
            continue
        if esc == "c":
            if idx + 1 >= length:
                malformed = True
                idx += 1
                continue
            ctrl = command[idx + 1]
            if ord(ctrl) >= 0x80:
                malformed = True
                idx += 2
                continue
            if ctrl == "\\" and idx + 2 < length and command[idx + 2] == "\\":
                out.append("\x1c")
                idx += 3
            elif ctrl == "?":
                out.append("\x7f")
                idx += 2
            else:
                ctrl_val = ord(ctrl.upper()) ^ 0x40
                if ctrl_val == 0:
                    malformed = True
                else:
                    out.append(chr(ctrl_val))
                idx += 2
            continue

        out.append("\\" + esc)
        idx += 1

    return "".join(out), idx, True


def _scan_paren_substitution(command: str, start: int) -> tuple[str, str, int]:
    """扫描平衡的 `$(...)`、`<( ... )` 或 `>( ... )`，返回 `(raw, inner, next_idx)`。"""
    length = len(command)
    cur = start + 2
    depth = 1
    in_single = False
    in_double = False
    escaped = False

    while cur < length and depth > 0:
        ch = command[cur]
        if escaped:
            escaped = False
            cur += 1
            continue
        if in_single:
            if ch == "'":
                in_single = False
            cur += 1
            continue
        if in_double:
            if ch == "\\":
                escaped = True
            elif ch == '"':
                in_double = False
            cur += 1
            continue
        if ch == "\\":
            escaped = True
            cur += 1
            continue
        if ch == "$" and cur + 1 < length and command[cur + 1] == "'":
            _, cur, _ = _decode_ansi_c_quoted(command, cur)
            continue
        if ch == "'":
            in_single = True
            cur += 1
            continue
        if ch == '"':
            in_double = True
            cur += 1
            continue
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        cur += 1

    if depth == 0:
        return command[start:cur], command[start + 2 : cur - 1], cur
    return command[start:], command[start + 2 :], length


def _scan_backtick_substitution(
    command: str, start: int, in_double: bool
) -> tuple[str, str, int]:
    """扫描反引号命令替换，返回 `(raw, inner, next_idx)`。"""
    length = len(command)
    cur = start + 1
    inner_buf: list[str] = []

    while cur < length:
        ch = command[cur]
        if ch == "\\":
            if cur + 1 < length:
                nxt = command[cur + 1]
                if nxt in ("$", "`", "\\") or (in_double and nxt == '"'):
                    inner_buf.append(nxt)
                    cur += 2
                    continue
                if nxt == "\n":
                    cur += 2
                    continue
            inner_buf.append("\\")
            cur += 1
            continue
        if ch == "`":
            return command[start : cur + 1], "".join(inner_buf), cur + 1
        inner_buf.append(ch)
        cur += 1

    return command[start:], "".join(inner_buf), length


def _read_double_quoted(
    command: str,
    start: int,
    word_buf: list[str],
    cur_subs: list[str],
) -> int:
    """读取从 `start`（指向开头 `"`）开始的双引号字符串并提取内部命令替换。"""
    length = len(command)
    idx = start + 1
    while idx < length:
        ch = command[idx]
        if ch == '"':
            return idx + 1
        if ch == "\\" and idx + 1 < length:
            nxt = command[idx + 1]
            if nxt == "\n":
                idx += 2
                continue
            if nxt in ('"', "\\", "$", "`"):
                word_buf.append(nxt)
                idx += 2
                continue
            word_buf.append("\\")
            word_buf.append(nxt)
            idx += 2
            continue
        if ch == "$" and idx + 1 < length and command[idx + 1] == "(":
            raw_span, inner, idx = _scan_paren_substitution(command, idx)
            word_buf.append(raw_span)
            if inner.strip():
                cur_subs.append(inner)
            continue
        if ch == "`":
            raw_span, inner, idx = _scan_backtick_substitution(
                command, idx, in_double=True
            )
            word_buf.append(raw_span)
            if inner.strip():
                cur_subs.append(inner)
            continue
        word_buf.append(ch)
        idx += 1
    return length


def _tokenize_shell_segments(command: str) -> list[_ShellSegment]:
    """按 Bash 词法拆分命令为独立子命令片段，保留引号/转义与重定向操作符来源。"""
    segments: list[_ShellSegment] = []
    cur_tokens: list[_ShellToken] = []
    cur_subs: list[str] = []
    word_buf: list[str] = []
    word_active = False
    word_quoted = False
    word_malformed = False

    def flush_word() -> None:
        nonlocal word_buf, word_active, word_quoted, word_malformed
        if word_active:
            cur_tokens.append(
                _ShellToken(
                    text="".join(word_buf),
                    is_redirect=False,
                    quoted=word_quoted,
                    has_malformed_expansion=word_malformed,
                )
            )
            word_buf = []
            word_active = False
            word_quoted = False
            word_malformed = False

    def flush_segment() -> None:
        nonlocal cur_tokens, cur_subs
        flush_word()
        if cur_tokens or cur_subs:
            segments.append(_ShellSegment(tokens=cur_tokens, substitutions=cur_subs))
            cur_tokens = []
            cur_subs = []

    idx = 0
    length = len(command)
    while idx < length:
        ch = command[idx]

        if ch in " \t\r":
            flush_word()
            idx += 1
            continue
        if ch == "\n":
            flush_segment()
            idx += 1
            continue
        if ch == "#" and not word_active:
            while idx < length and command[idx] != "\n":
                idx += 1
            continue
        if ch == "\\":
            if idx + 1 < length and command[idx + 1] == "\n":
                idx += 2
                continue
            word_active = True
            word_quoted = True
            if idx + 1 < length:
                word_buf.append(command[idx + 1])
                idx += 2
            else:
                word_buf.append("\\")
                idx += 1
            continue
        if ch == "$" and idx + 1 < length and command[idx + 1] == "'":
            decoded, idx, malformed = _decode_ansi_c_quoted(command, idx)
            word_active = True
            word_quoted = True
            if malformed:
                word_malformed = True
            word_buf.append(decoded)
            continue
        if ch == "$" and idx + 1 < length and command[idx + 1] == '"':
            word_active = True
            word_quoted = True
            idx = _read_double_quoted(command, idx + 1, word_buf, cur_subs)
            continue
        if ch == "$" and idx + 1 < length and command[idx + 1] == "(":
            raw_span, inner, idx = _scan_paren_substitution(command, idx)
            word_active = True
            word_buf.append(raw_span)
            if inner.strip():
                cur_subs.append(inner)
            continue
        if ch in "<>" and idx + 1 < length and command[idx + 1] == "(":
            raw_span, inner, idx = _scan_paren_substitution(command, idx)
            word_active = True
            word_buf.append(raw_span)
            if inner.strip():
                cur_subs.append(inner)
            continue
        if ch == "`":
            raw_span, inner, idx = _scan_backtick_substitution(
                command, idx, in_double=False
            )
            word_active = True
            word_buf.append(raw_span)
            if inner.strip():
                cur_subs.append(inner)
            continue
        if ch == "'":
            word_active = True
            word_quoted = True
            idx += 1
            while idx < length and command[idx] != "'":
                word_buf.append(command[idx])
                idx += 1
            if idx < length:
                idx += 1
            continue
        if ch == '"':
            word_active = True
            word_quoted = True
            idx = _read_double_quoted(command, idx, word_buf, cur_subs)
            continue

        # 先识别未引号重定向操作符，避免 `&>` 被单字符 `&` 误切分
        redir_op = ""
        if command[idx : idx + 3] in _REDIRECT_OPS_3:
            redir_op = command[idx : idx + 3]
        elif command[idx : idx + 2] in _REDIRECT_OPS_2:
            redir_op = command[idx : idx + 2]
        elif ch in "<>":
            redir_op = ch
        if redir_op:
            if (
                word_active
                and not word_quoted
                and not redir_op.startswith("&")
                and "".join(word_buf).isdigit()
            ):
                fd_op = "".join(word_buf) + redir_op
                word_buf = []
                word_active = False
                word_quoted = False
                word_malformed = False
                cur_tokens.append(_ShellToken(text=fd_op, is_redirect=True))
            else:
                flush_word()
                cur_tokens.append(_ShellToken(text=redir_op, is_redirect=True))
            idx += len(redir_op)
            continue

        if command[idx : idx + 3] == ";;&":
            flush_segment()
            idx += 3
            continue
        if command[idx : idx + 2] in _CONTROL_OPS_2:
            flush_segment()
            idx += 2
            continue
        if ch in _CONTROL_OPS_1:
            flush_segment()
            idx += 1
            continue

        word_active = True
        word_buf.append(ch)
        idx += 1

    flush_segment()
    return segments


def _split_env_string(text: str) -> list[_ShellToken]:
    """按 GNU `env -S` 语义拆分字符串参数，返回普通参数 token 列表。"""
    tokens: list[_ShellToken] = []
    buf: list[str] = []
    active = False
    quoted = False
    malformed = False

    def flush() -> None:
        nonlocal buf, active, quoted, malformed
        if active:
            tokens.append(
                _ShellToken(
                    text="".join(buf),
                    is_redirect=False,
                    quoted=quoted,
                    has_malformed_expansion=malformed,
                )
            )
            buf = []
            active = False
            quoted = False
            malformed = False

    idx = 0
    length = len(text)
    env_escapes = {
        "c": "",
        "f": "\f",
        "n": "\n",
        "r": "\r",
        "t": "\t",
        "v": "\v",
        "_": " ",
        "#": "#",
        "$": "$",
        "'": "'",
        '"': '"',
        "\\": "\\",
    }
    while idx < length:
        ch = text[idx]
        if ch in " \t\r\n":
            flush()
            idx += 1
            continue
        if ch == "#" and not active:
            break
        if ch == "\\" and idx + 1 < length:
            nxt = text[idx + 1]
            if nxt == "c":
                break
            if nxt == "_":
                # GNU env -S 中未加引号的 \_ 作为参数分隔符（区别于双引号内字面空格）
                flush()
                idx += 2
                continue
            active = True
            quoted = True
            buf.append(env_escapes.get(nxt, nxt))
            idx += 2
            continue
        if ch == "$" and idx + 1 < length and text[idx + 1] == "'":
            decoded, idx, is_bad = _decode_ansi_c_quoted(text, idx)
            active = True
            quoted = True
            if is_bad:
                malformed = True
            buf.append(decoded)
            continue
        if ch == "'":
            active = True
            quoted = True
            idx += 1
            while idx < length and text[idx] != "'":
                if text[idx] == "\\" and idx + 1 < length and text[idx + 1] == "'":
                    buf.append("'")
                    idx += 2
                    continue
                buf.append(text[idx])
                idx += 1
            if idx < length:
                idx += 1
            continue
        if ch == '"':
            active = True
            quoted = True
            idx += 1
            while idx < length and text[idx] != '"':
                if text[idx] == "\\" and idx + 1 < length:
                    nxt = text[idx + 1]
                    buf.append(env_escapes.get(nxt, "\\" + nxt))
                    idx += 2
                    continue
                buf.append(text[idx])
                idx += 1
            if idx < length:
                idx += 1
            continue
        active = True
        buf.append(ch)
        idx += 1

    flush()
    return tokens


def _parse_wrapper_options(
    tokens: list[_ShellToken],
    start: int,
    spec: _WrapperSpec,
) -> tuple[int, bool]:
    """解析受支持包装器的短/长选项、短选项簇及前置位置参数。

    Returns:
        (next_index, unresolved): 下一个待解析命令的起始下标，以及是否遇到未知选项。
    """
    idx = start
    unresolved = False
    while idx < len(tokens):
        token = tokens[idx]
        if token.is_redirect:
            idx += 2
            continue
        text = token.text
        if text == "--":
            idx += 1
            break
        if spec.allow_env_assign and _ENV_ASSIGN_RE.match(text):
            idx += 1
            continue
        if spec.allow_lone_dash_flag and text == "-":
            idx += 1
            continue
        if text.startswith("--") and len(text) > 2:
            if "=" in text:
                opt_name = text.split("=", 1)[0]
                if (
                    opt_name not in spec.long_arg_opts
                    and opt_name not in spec.long_opt_arg_opts
                    and opt_name not in spec.long_flag_opts
                ):
                    unresolved = True
                idx += 1
                continue
            if text in spec.long_arg_opts:
                idx += 2
                continue
            if text in spec.long_flag_opts or text in spec.long_opt_arg_opts:
                idx += 1
                continue
            unresolved = True
            idx += 1
            continue
        if text.startswith("-") and len(text) > 1:
            if spec.allow_numeric_short_opt and _NUMERIC_SHORT_OPT_RE.match(text):
                idx += 1
                continue
            pos = 1
            consumed = False
            while pos < len(text):
                opt_ch = text[pos]
                if opt_ch in spec.short_arg_opts:
                    idx += 1 if pos + 1 < len(text) else 2
                    consumed = True
                    break
                if opt_ch in spec.short_opt_arg_opts:
                    idx += 1
                    consumed = True
                    break
                if opt_ch in spec.short_flag_opts:
                    pos += 1
                    continue
                unresolved = True
                idx += 1
                consumed = True
                break
            if not consumed:
                idx += 1
            continue
        break

    skipped = 0
    while idx < len(tokens) and skipped < spec.positional_args:
        if tokens[idx].is_redirect:
            idx += 2
            continue
        idx += 1
        skipped += 1

    return idx, unresolved


def _fallback_scan_remaining(
    tokens: list[_ShellToken],
    start: int,
    depth: int,
    include_wrappers: bool,
) -> bool:
    """对未知选项或未建模启动器的剩余参数做保守回退扫描，绝不将解析歧义当成安全证明。"""
    if depth > _MAX_RM_PARSE_DEPTH:
        return True
    pos = start
    while pos < len(tokens):
        tok = tokens[pos]
        if tok.is_redirect:
            pos += 2
            continue
        if tok.has_malformed_expansion:
            return True
        candidate = tok.text.rsplit("/", 1)[-1]
        if candidate == "rm":
            if _rm_args_have_dangerous_flag(tokens[pos + 1 :]):
                return True
        elif candidate in _SHELL_INTERPRETERS or (
            include_wrappers and candidate in _ALL_KNOWN_WRAPPERS
        ):
            if _segment_has_dangerous_rm(tokens[pos:], depth + 1):
                return True
        pos += 1
    return False


def _segment_has_dangerous_rm(
    tokens: list[_ShellToken],
    depth: int,
    substitutions: list[str] | None = None,
) -> bool:
    """分析单个子命令 token 列表，识别直接、嵌套或经包装器调用的危险 rm。

    达到递归深度上限 `_MAX_RM_PARSE_DEPTH` 时采取保守策略返回 `True`（fail-closed），
    确保未完成检查的命令进入沙箱而不是被放行。
    """
    if depth > _MAX_RM_PARSE_DEPTH:
        return True
    if substitutions:
        for sub_cmd in substitutions:
            if _is_dangerous_rm_command(sub_cmd, _depth=depth + 1):
                return True
    if not tokens:
        return False

    idx = 0
    while idx < len(tokens):
        # 跳过前置 shell 关键字、环境变量赋值及前导重定向（含 2>/dev/null、&>/dev/null）
        while idx < len(tokens):
            tok = tokens[idx]
            if tok.is_redirect:
                idx += 2
                continue
            if tok.has_malformed_expansion:
                return True
            if (
                not tok.quoted and tok.text in _SHELL_LEADING_KEYWORDS
            ) or _ENV_ASSIGN_RE.match(tok.text):
                idx += 1
                continue
            break

        if idx >= len(tokens):
            return False

        cmd_token = tokens[idx]
        if cmd_token.has_malformed_expansion:
            return True
        cmd = cmd_token.text.rsplit("/", 1)[-1]
        if cmd == "rm":
            return _rm_args_have_dangerous_flag(tokens[idx + 1 :])

        if cmd == "eval":
            eval_start = idx + 1
            if (
                eval_start < len(tokens)
                and not tokens[eval_start].is_redirect
                and tokens[eval_start].text == "--"
            ):
                eval_start += 1
            eval_parts: list[str] = []
            scan = eval_start
            while scan < len(tokens):
                if tokens[scan].is_redirect:
                    scan += 2
                    continue
                eval_parts.append(tokens[scan].text)
                scan += 1
            eval_script = " ".join(eval_parts)
            if not eval_script.strip():
                return False
            return _is_dangerous_rm_command(eval_script, _depth=depth + 1)

        if cmd == "env":
            scan = idx + 1
            unresolved = False
            while scan < len(tokens):
                tok = tokens[scan]
                if tok.is_redirect:
                    scan += 2
                    continue
                text = tok.text
                if text == "--":
                    scan += 1
                    break
                if text == "-" or _ENV_ASSIGN_RE.match(text):
                    scan += 1
                    continue
                if text in ("-S", "--split-string"):
                    if scan + 1 >= len(tokens):
                        return False
                    split_tokens = _split_env_string(tokens[scan + 1].text)
                    combined = (
                        [_ShellToken("env")] + split_tokens + tokens[scan + 2 :]
                    )
                    return _segment_has_dangerous_rm(combined, depth + 1)
                if text.startswith("--split-string="):
                    split_tokens = _split_env_string(text.split("=", 1)[1])
                    combined = (
                        [_ShellToken("env")] + split_tokens + tokens[scan + 1 :]
                    )
                    return _segment_has_dangerous_rm(combined, depth + 1)
                if text.startswith("--") and len(text) > 2:
                    if "=" in text:
                        opt_name = text.split("=", 1)[0]
                        if (
                            opt_name not in _ENV_LONG_ARG_OPTS
                            and opt_name not in _ENV_LONG_OPT_ARG_OPTS
                            and opt_name not in _ENV_LONG_FLAG_OPTS
                        ):
                            unresolved = True
                        scan += 1
                        continue
                    if text in _ENV_LONG_ARG_OPTS:
                        scan += 2
                        continue
                    if text in _ENV_LONG_FLAG_OPTS or text in _ENV_LONG_OPT_ARG_OPTS:
                        scan += 1
                        continue
                    unresolved = True
                    scan += 1
                    continue
                if text.startswith("-") and len(text) > 1:
                    pos = 1
                    consumed = False
                    while pos < len(text):
                        opt_ch = text[pos]
                        if opt_ch == "S":
                            if pos + 1 < len(text):
                                split_tokens = _split_env_string(text[pos + 1 :])
                                rest_start = scan + 1
                            elif scan + 1 < len(tokens):
                                split_tokens = _split_env_string(tokens[scan + 1].text)
                                rest_start = scan + 2
                            else:
                                return False
                            combined = [
                                _ShellToken("env"),
                                *split_tokens,
                                *tokens[rest_start:],
                            ]
                            return _segment_has_dangerous_rm(combined, depth + 1)
                        if opt_ch in _ENV_SHORT_ARG_OPTS:
                            scan += 1 if pos + 1 < len(text) else 2
                            consumed = True
                            break
                        if opt_ch in _ENV_SHORT_FLAG_OPTS:
                            pos += 1
                            continue
                        unresolved = True
                        scan += 1
                        consumed = True
                        break
                    if not consumed:
                        scan += 1
                    continue
                break
            if unresolved:
                return _fallback_scan_remaining(
                    tokens, idx + 1, depth + 1, include_wrappers=True
                )
            idx = scan
            continue

        if cmd in ("su", "runuser"):
            scan = idx + 1
            unresolved = False
            while scan < len(tokens):
                tok = tokens[scan]
                if tok.is_redirect:
                    scan += 2
                    continue
                text = tok.text
                if text == "--":
                    rest = tokens[scan + 1 :]
                    if _segment_has_dangerous_rm(rest, depth + 1):
                        return True
                    if _segment_has_dangerous_rm(
                        [_ShellToken("bash"), *rest], depth + 1
                    ):
                        return True
                    break
                if text == "-":
                    scan += 1
                    continue
                if text in _SU_CMD_LONG_OPTS:
                    if scan + 1 < len(tokens):
                        if _is_dangerous_rm_command(
                            tokens[scan + 1].text, _depth=depth + 1
                        ):
                            return True
                        scan += 2
                        continue
                    return False
                if any(text.startswith(opt + "=") for opt in _SU_CMD_LONG_OPTS):
                    if _is_dangerous_rm_command(
                        text.split("=", 1)[1], _depth=depth + 1
                    ):
                        return True
                    scan += 1
                    continue
                if text.startswith("--") and len(text) > 2:
                    if "=" in text:
                        opt_name = text.split("=", 1)[0]
                        if (
                            opt_name not in _SU_LONG_ARG_OPTS
                            and opt_name not in _SU_LONG_FLAG_OPTS
                        ):
                            unresolved = True
                        scan += 1
                        continue
                    if text in _SU_LONG_ARG_OPTS:
                        scan += 2
                        continue
                    if text in _SU_LONG_FLAG_OPTS:
                        scan += 1
                        continue
                    unresolved = True
                    scan += 1
                    continue
                if text.startswith("-") and len(text) > 1:
                    pos = 1
                    consumed = False
                    while pos < len(text):
                        opt_ch = text[pos]
                        if opt_ch == "c":
                            if pos + 1 < len(text):
                                if _is_dangerous_rm_command(
                                    text[pos + 1 :], _depth=depth + 1
                                ):
                                    return True
                                scan += 1
                                consumed = True
                                break
                            if scan + 1 < len(tokens):
                                if _is_dangerous_rm_command(
                                    tokens[scan + 1].text, _depth=depth + 1
                                ):
                                    return True
                                scan += 2
                                consumed = True
                                break
                            return False
                        if opt_ch in _SU_SHORT_ARG_OPTS:
                            scan += 1 if pos + 1 < len(text) else 2
                            consumed = True
                            break
                        if opt_ch in _SU_SHORT_FLAG_OPTS:
                            pos += 1
                            continue
                        unresolved = True
                        scan += 1
                        consumed = True
                        break
                    if not consumed:
                        scan += 1
                    continue
                scan += 1
            if unresolved:
                return _fallback_scan_remaining(
                    tokens, idx + 1, depth + 1, include_wrappers=True
                )
            return False

        if cmd == "find":
            scan = idx + 1
            while scan < len(tokens):
                tok = tokens[scan]
                if tok.is_redirect:
                    scan += 2
                    continue
                if tok.text in _FIND_EXEC_FLAGS:
                    sub_start = scan + 1
                    sub_end = sub_start
                    while sub_end < len(tokens) and (
                        tokens[sub_end].is_redirect
                        or tokens[sub_end].text not in (";", "+")
                    ):
                        sub_end += 1
                    if _segment_has_dangerous_rm(tokens[sub_start:sub_end], depth + 1):
                        return True
                    scan = sub_end + 1
                else:
                    scan += 1
            return False

        if cmd in _SHELL_INTERPRETERS:
            scan = idx + 1
            has_c = False
            unresolved = False
            while scan < len(tokens):
                tok = tokens[scan]
                if tok.is_redirect:
                    scan += 2
                    continue
                text = tok.text
                if text == "--":
                    scan += 1
                    break
                if text in _SHELL_LONG_ARG_OPTS:
                    scan += 2
                    continue
                if text.startswith("--") and len(text) > 2:
                    opt_name = text.split("=", 1)[0]
                    if (
                        opt_name not in _SHELL_LONG_FLAG_OPTS
                        and opt_name not in _SHELL_LONG_ARG_OPTS
                    ):
                        unresolved = True
                    scan += 1
                    continue
                if text.startswith(("-", "+")) and len(text) > 1:
                    is_minus = text.startswith("-")
                    if is_minus and "c" in text[1:]:
                        has_c = True
                    opt_arg_count = sum(
                        1 for opt_ch in text[1:] if opt_ch in ("o", "O")
                    )
                    scan += 1
                    skipped_opt_args = 0
                    while scan < len(tokens) and skipped_opt_args < opt_arg_count:
                        if tokens[scan].is_redirect:
                            scan += 2
                            continue
                        scan += 1
                        skipped_opt_args += 1
                    continue
                break
            if has_c:
                if unresolved:
                    for rem in tokens[idx + 1 :]:
                        if not rem.is_redirect and _is_dangerous_rm_command(
                            rem.text, _depth=depth + 1
                        ):
                            return True
                    return False
                while scan < len(tokens) and tokens[scan].is_redirect:
                    scan += 2
                if scan < len(tokens):
                    return _is_dangerous_rm_command(
                        tokens[scan].text, _depth=depth + 1
                    )
                return False
            if unresolved:
                for pos in range(idx + 1, len(tokens)):
                    rem = tokens[pos]
                    if rem.is_redirect:
                        continue
                    if (
                        rem.text.startswith("-")
                        and not rem.text.startswith("--")
                        and "c" in rem.text[1:]
                    ):
                        for cand in tokens[pos + 1 :]:
                            if not cand.is_redirect and _is_dangerous_rm_command(
                                cand.text, _depth=depth + 1
                            ):
                                return True
                        break
                return _fallback_scan_remaining(
                    tokens, idx + 1, depth + 1, include_wrappers=True
                )
            return False

        if cmd == "watch":
            next_idx, unresolved = _parse_wrapper_options(
                tokens, idx + 1, _WRAPPER_SPECS["watch"]
            )
            if unresolved:
                if _fallback_scan_remaining(
                    tokens, idx + 1, depth + 1, include_wrappers=True
                ):
                    return True
                rem_idx = idx + 1
                while rem_idx < len(tokens):
                    if tokens[rem_idx].is_redirect:
                        rem_idx += 2
                        continue
                    if _is_dangerous_rm_command(
                        tokens[rem_idx].text, _depth=depth + 1
                    ):
                        return True
                    rem_idx += 1
                return False
            use_exec = False
            opt_idx = idx + 1
            while opt_idx < next_idx:
                opt_tok = tokens[opt_idx]
                if opt_tok.is_redirect:
                    opt_idx += 2
                    continue
                if opt_tok.text == "--":
                    break
                if opt_tok.text == "--exec":
                    use_exec = True
                elif (
                    opt_tok.text.startswith("-")
                    and not opt_tok.text.startswith("--")
                    and len(opt_tok.text) > 1
                ):
                    for opt_ch in opt_tok.text[1:]:
                        if opt_ch in ("d", "n"):
                            break
                        if opt_ch == "x":
                            use_exec = True
                opt_idx += 1
            rest = tokens[next_idx:]
            if _segment_has_dangerous_rm(rest, depth + 1):
                return True
            if not use_exec:
                watch_parts: list[str] = []
                w_idx = 0
                while w_idx < len(rest):
                    if rest[w_idx].is_redirect:
                        w_idx += 2
                        continue
                    watch_parts.append(rest[w_idx].text)
                    w_idx += 1
                watch_script = " ".join(watch_parts)
                if watch_script.strip() and _is_dangerous_rm_command(
                    watch_script, _depth=depth + 1
                ):
                    return True
            return False

        if cmd in _WRAPPER_SPECS:
            next_idx, unresolved = _parse_wrapper_options(
                tokens, idx + 1, _WRAPPER_SPECS[cmd]
            )
            if unresolved:
                return _fallback_scan_remaining(
                    tokens, idx + 1, depth + 1, include_wrappers=True
                )
            idx = next_idx
            continue

        # 未建模的启动器：保守扫描剩余非重定向参数中的危险 rm / 子 shell，避免漏报
        return _fallback_scan_remaining(
            tokens, idx + 1, depth + 1, include_wrappers=False
        )

    return False


def _is_dangerous_rm_command(command: str, _depth: int = 0) -> bool:
    """检测命令中是否包含带递归/强制标志的 rm 调用。

    按 Bash 词法拆分并解析独立选项与受支持包装器，避免将带连字符的普通文件名
    （如 `rm foo-bar.txt`、`rm my-file-rf.log`）或 `--` 之后的参数误判为危险选项。
    超出递归深度上限 `_MAX_RM_PARSE_DEPTH` 时保守返回 `True`（fail-closed 进沙箱）。
    """
    if _depth > _MAX_RM_PARSE_DEPTH:
        return True
    if not command.strip():
        return False
    for segment in _tokenize_shell_segments(command):
        if _segment_has_dangerous_rm(segment.tokens, _depth, segment.substitutions):
            return True
    return False


# 替换为沙箱执行的命令（文件系统/权限/服务类）- 网络隔离
# 注意：sudo 不在此列表，由 strip_sudo() 预处理后对底层命令评估
DANGEROUS_PATTERNS: list[tuple[str | Callable[[str], bool], str]] = [
    # rm 危险操作：按 shell token 解析 -r/-R/-f/--recursive/--force，
    # 避免将带连字符的文件名或 `--` 后的文件名误判为删除标志
    (_is_dangerous_rm_command, "递归/强制删除"),
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
_SUDO_LONG_ARG_NAMES = r"(?:user|group|host|prompt|chroot|chdir)"
_SUDO_OPT_VAL = r"(?:'[^']*'|\"(?:\\.|[^\"\\])*\"|[^\s'\"]+)"
_SUDO_PREFIX_RE = re.compile(
    r"^sudo\s+"
    r"(?:"
    r"(?:-[nEbkKHPvA]+|--(?:non-interactive|preserve-env|reset-timestamp))\s+"
    rf"|(?:--{_SUDO_LONG_ARG_NAMES}={_SUDO_OPT_VAL})\s+"
    rf"|(?:(?:-[uUgcCTthpr]|--{_SUDO_LONG_ARG_NAMES})\s+{_SUDO_OPT_VAL}\s+)"
    r")*"
    r"(?:(--)\s+)?"
)


_SUDO_PREFIX_SUBSTITUTION_MARKERS = ("$(", "`", "<(", ">(")


def strip_sudo(command: str) -> tuple[str, bool]:
    """剥离命令开头的 sudo 前缀及其非交互选项。

    若前缀选项中包含外层 shell 可执行的命令/进程替换（如 `$(...)`、反引号、
    `<(...)`、`>(...)`），或存在未完整解析的 sudo 选项，则拒绝剥离以保留原始语义。

    Returns:
        (stripped_command, had_sudo): 剥离后命令 及 是否包含 sudo 前缀
    """
    cmd = command.strip()
    m = _SUDO_PREFIX_RE.match(cmd)
    if m:
        prefix = cmd[: m.end()]
        if any(marker in prefix for marker in _SUDO_PREFIX_SUBSTITUTION_MARKERS):
            return command, False
        rest = cmd[m.end() :].strip()
        # 确保剥离后仍有命令体，且未残留交互式/未解析的 sudo 选项
        if (
            rest
            and not re.match(r"^-[is]\b", rest)
            and (m.group(1) or not rest.startswith("-"))
        ):
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
    escaped_cwd = cwd.replace("'", "'\\''")

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
        f" --sandbox-policy-cwd '{escaped_cwd}'"
        f" --file-system-sandbox-policy '{SANDBOX_FS_POLICY}'"
        f" --network-sandbox-policy '\"{network_policy}\"'"
        f" -- bash -c '{full_bash_cmd}'"
    )


def detect_patterns(command: str, patterns: list[tuple[Any, str]]) -> list[str]:
    """检测命令中的危险模式，返回匹配的原因列表"""
    reasons: list[str] = []
    for pattern, reason in patterns:
        matched = (
            pattern(command)
            if callable(pattern)
            else bool(re.search(pattern, command, re.IGNORECASE))
        )
        if matched:
            reasons.append(reason)
    return reasons


def _detect_command_patterns(
    original_command: str,
    eval_command: str,
    patterns: list[tuple[Any, str]],
) -> list[str]:
    """同时检查原始命令与剥离执行前缀后的命令，避免前缀剥离隐藏风险。"""
    reasons = detect_patterns(original_command, patterns)
    if eval_command != original_command:
        for reason in detect_patterns(eval_command, patterns):
            if reason not in reasons:
                reasons.append(reason)
    return reasons


def main() -> None:
    flag_error = None
    try:
        all_shell = all_shell_enabled()
    except ValueError as error:
        all_shell = True
        flag_error = error
    try:
        input_data = json.load(sys.stdin)
    except (ValueError, EOFError) as error:
        # Preserve legacy passthrough; configured authorization must fail closed.
        print(
            json.dumps(
                authorization_error(f"invalid hook input: {error}")
                if all_shell
                else {"decision": "allow"}
            )
        )
        return

    if not isinstance(input_data, dict):
        print(json.dumps(authorization_error("hook input must be an object")))
        return

    event = input_data.get("hook_event_name", "PreToolUse")
    if event == "PostToolUse":
        print(json.dumps(finish_authorized(input_data), ensure_ascii=False))
        return
    if event != "PreToolUse":
        print("{}")
        return

    if flag_error is not None:
        print(json.dumps(authorization_error(flag_error)))
        return

    tool_input = input_data.get("tool_input", {})
    if all_shell and (
        not isinstance(tool_input, dict)
        or not isinstance(tool_input.get("command"), str)
        or not tool_input["command"].strip()
    ):
        print(
            json.dumps(authorization_error("hook requires a non-empty shell command"))
        )
        return
    command = tool_input.get("command", "")
    cwd = input_data.get("cwd", "/tmp")

    # matcher 已保证只在 shell 类工具上触发，这里只剩空命令短路。
    if not command.strip():
        print(json.dumps({"decision": "allow"}))
        return

    # Inspect the original command for explicit blocks independently of sudo stripping.
    stripped_command, has_sudo = strip_sudo(command)
    is_root = os.getuid() == 0

    # 用剥离 sudo 后的命令作为底层执行命令；安全检查始终覆盖原始命令
    eval_command = stripped_command if has_sudo else command

    # 第一优先级：直接 block 的命令（始终检查原始命令，避免前缀剥离抹除命令替换）
    block_reasons = _detect_command_patterns(command, eval_command, BLOCK_PATTERNS)
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

        if all_shell:
            message = f"Explicit sandbox policy blocked execution: {reasons_str}"
            result = authorization_error(message)
        print(json.dumps(result, ensure_ascii=False))
        return

    if all_shell:
        # Authorization, native approval and execution must bind the original command.
        print(json.dumps(prepare_authorized(input_data, command), ensure_ascii=False))
        return

    # 第二优先级：替换为沙箱执行（文件系统/权限类风险）
    sandbox_reasons = _detect_command_patterns(
        command, eval_command, DANGEROUS_PATTERNS
    )
    network_reasons = _detect_command_patterns(
        command, eval_command, NETWORK_PATTERNS
    )

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
