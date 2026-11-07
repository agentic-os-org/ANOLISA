#!/usr/bin/env python3
"""
命令安全分类器 - 四层分类 + 统一规则引擎

分类层级（优先级从高到低）：
1. destructive -> 直接拒绝，不进沙箱
2. dangerous   -> 沙箱执行，禁止自动补权限
3. safe        -> 沙箱执行，无需补权限
4. default     -> 沙箱执行，可自动补最小权限

注：分类直接决定沙箱策略（safe->只读，dangerous/default->workspace-write），分类层同时控制是否允许扩权。

用法：
    python3 classify_command.py "git status"
    python3 classify_command.py --json "rm -rf /"
"""

import argparse
import json
import re
import shlex
import sys
from pathlib import PurePath
from typing import Any, Dict, List, Optional, Tuple

from agent_sec_cli.sandbox.rules import (
    DANGEROUS_RULES,
    DESTRUCTIVE_RULES,
    PERMISSION_RULES,
    SAFE_COMMANDS,
    SAFE_COMMANDS_LINUX,
    SAFE_CONDITIONAL,
)

_FD_REDIRECT_RE = re.compile(r"^(?:\d+)?(?:>&|<&)(?:\d+|-)$")
_STANDALONE_REDIR_RE = re.compile(
    r"^(?:\d+|&)?(?:&>>|1>>|2>>|>>|<<<|<<|>&|<&|>\||&>|[<>])(?:\d+)?$"
)
_ATTACHED_REDIR_RE = re.compile(
    r"^(?:\d+|&)?(?:&>>|1>>|2>>|>>|<<<|<<|>&|<&|>\||&>|[<>])(?:\d+)?(.+)$"
)

_BLOCK_DEVICE_PREFIXES = (
    "/dev/sd",
    "/dev/nvme",
    "/dev/hd",
    "/dev/vd",
    "/dev/xvd",
    "/dev/mmcblk",
)

_CRITICAL_SYSTEM_TARGETS = (
    "/",
    "/etc",
    "/boot",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/usr",
    "/root",
)

_SUDO_ARGS_TAKING_VALUE = frozenset(
    [
        "-u",
        "--user",
        "-g",
        "--group",
        "-h",
        "--host",
        "-p",
        "--prompt",
        "-c",
        "-C",
        "--close-from",
        "-R",
        "--chroot",
        "-T",
        "--command-timeout",
        "-U",
        "--other-user",
    ]
)

_SHELL_NAMES = frozenset(["bash", "sh", "zsh", "dash", "ksh"])


def _parse_redirections(tokens: List[str]) -> Tuple[List[str], List[Tuple[str, str]]]:
    """分离普通命令参数与重定向操作符及目标路径。"""
    cleaned: List[str] = []
    redirections: List[Tuple[str, str]] = []
    i = 0
    while i < len(tokens):
        tok = tokens[i]
        if _FD_REDIRECT_RE.match(tok):
            redirections.append((tok, ""))
            i += 1
            continue
        if _STANDALONE_REDIR_RE.match(tok):
            if i + 1 < len(tokens):
                redirections.append((tok, tokens[i + 1]))
                i += 2
                continue
            redirections.append((tok, ""))
            i += 1
            continue
        m_attach = _ATTACHED_REDIR_RE.match(tok)
        if m_attach:
            target = m_attach.group(1)
            op = tok[: -len(target)]
            redirections.append((op, target))
            i += 1
            continue
        cleaned.append(tok)
        i += 1
    return cleaned, redirections


def _is_sensitive_path(path: str) -> bool:
    """检查路径是否为凭据或敏感配置文件。"""
    p = path.strip().strip("'\"")
    name = PurePath(p).name
    if name.startswith(".env") or name in (
        ".env",
        "credentials",
        "credentials.json",
        "service_account.json",
        "token.json",
    ):
        return True
    if any(name.startswith(prefix) for prefix in ("id_rsa", "id_ed25519", "id_ecdsa", "id_dsa")):
        return True
    if ".aws/credentials" in p or ".aws/config" in p or ".ssh/id_" in p or ".config/gcloud" in p:
        return True
    return False


def _check_redirection_security(
    redirections: List[Tuple[str, str]],
) -> Tuple[Optional[str], str]:
    """检查重定向目标是否包含块设备、关键系统路径或敏感文件。"""
    for op, target in redirections:
        tgt = target.strip().strip("'\"")
        if not tgt:
            continue
        is_output = any(
            op.startswith(prefix)
            for prefix in (
                ">",
                "1>",
                "2>",
                "&>",
                ">&",
                ">|",
                ">>",
                "1>>",
                "2>>",
                "&>>",
            )
        )
        if not is_output:
            continue
        if any(tgt.startswith(bdev) for bdev in _BLOCK_DEVICE_PREFIXES):
            return "destructive", f"重定向写入块设备: {tgt}"
        if any(tgt == c or tgt.startswith(c + "/") for c in _CRITICAL_SYSTEM_TARGETS):
            return "destructive", f"重定向覆盖系统关键文件: {tgt}"
        if _is_sensitive_path(tgt):
            return "dangerous", f"重定向写入敏感凭据文件: {tgt}"
    return None, ""


def _has_workspace_write_redirection(
    redirections: List[Tuple[str, str]],
) -> bool:
    """判断是否存在向工作区文件的实际输出重定向。"""
    for op, target in redirections:
        tgt = target.strip().strip("'\"")
        is_output = any(
            op.startswith(prefix)
            for prefix in (
                ">",
                "1>",
                "2>",
                "&>",
                ">&",
                ">|",
                ">>",
                "1>>",
                "2>>",
                "&>>",
            )
        )
        if is_output and tgt not in ("/dev/null", ""):
            return True
    return False


def _extract_script_from_shell_parts(parts: List[str]) -> Optional[str]:
    """从 shell 调用参数列表解析出 -c 脚本内容。"""
    if len(parts) < 3:
        return None
    cmd = PurePath(parts[0]).name
    if cmd not in _SHELL_NAMES:
        return None
    idx = 1
    while idx < len(parts):
        arg = parts[idx]
        if arg == "-c":
            if idx + 1 < len(parts):
                return parts[idx + 1]
            return None
        if arg.startswith("-") and not arg.startswith("--") and arg.endswith("c") and len(arg) > 1:
            if idx + 1 < len(parts):
                return parts[idx + 1]
            return None
        if arg == "--":
            if idx + 1 < len(parts) and parts[idx + 1] == "-c" and idx + 2 < len(parts):
                return parts[idx + 2]
            return None
        if arg in ("-o", "+o", "-O", "+O") and idx + 1 < len(parts):
            idx += 2
            continue
        if arg.startswith("-") or arg.startswith("+"):
            idx += 1
            continue
        break
    return None


def _split_shell_commands(script: str) -> List[str]:
    """在保持引号内语义的前提下按 shell 操作符拆分命令。"""
    s = script.strip()
    if s.startswith("(") and s.endswith(")"):
        s = s[1:-1].strip()
    commands: List[str] = []
    current: List[str] = []
    in_single = False
    in_double = False
    escape = False
    i = 0
    n = len(s)
    while i < n:
        ch = s[i]
        if escape:
            current.append(ch)
            escape = False
            i += 1
            continue
        if ch == "\\":
            if not in_single:
                escape = True
            current.append(ch)
            i += 1
            continue
        if ch == "'" and not in_double:
            in_single = not in_single
            current.append(ch)
            i += 1
            continue
        if ch == '"' and not in_single:
            in_double = not in_double
            current.append(ch)
            i += 1
            continue
        if not in_single and not in_double:
            if i + 1 < n and s[i : i + 2] in ("&&", "||", "|&"):
                cmd_str = "".join(current).strip()
                if cmd_str:
                    commands.append(cmd_str)
                current = []
                i += 2
                continue
            if ch in (";", "|", "\n"):
                cmd_str = "".join(current).strip()
                if cmd_str:
                    commands.append(cmd_str)
                current = []
                i += 1
                continue
            if ch == "&":
                is_prev_redir = i > 0 and s[i - 1] in (">", "<")
                is_next_redir = i + 1 < n and (s[i + 1] in (">", "&") or s[i + 1].isdigit())
                if not is_prev_redir and not is_next_redir:
                    cmd_str = "".join(current).strip()
                    if cmd_str:
                        commands.append(cmd_str)
                    current = []
                    i += 1
                    continue
        current.append(ch)
        i += 1
    rem = "".join(current).strip()
    if rem:
        commands.append(rem)
    return commands


def _strip_sudo_args(parts: List[str]) -> Tuple[List[str], bool]:
    """剥离 sudo 自身选项和标志，获取底层真实执行命令。"""
    if not parts or PurePath(parts[0]).name != "sudo":
        return parts, False
    idx = 1
    while idx < len(parts):
        arg = parts[idx]
        if arg == "--":
            idx += 1
            break
        if arg.startswith("-") and "=" in arg:
            idx += 1
            continue
        if arg in _SUDO_ARGS_TAKING_VALUE:
            idx += 2
            continue
        if (arg.startswith("-u") or arg.startswith("-g")) and len(arg) > 2:
            idx += 1
            continue
        if arg.startswith("-"):
            idx += 1
            continue
        break
    return parts[idx:], True


class RuleEngine:
    """统一规则匹配引擎 - 支持 rules.py 中所有 Match Schema 字段"""

    @staticmethod
    def match_rule(rule: dict, parts: List[str], full_cmd: str) -> Tuple[bool, str]:
        """检查命令是否匹配规则，返回 (matched, reason)。"""
        if not parts:
            return False, ""

        cmd = PurePath(parts[0]).name
        args = parts[1:]
        reason = rule.get("reason", "")

        # OS 限制
        if "os" in rule and sys.platform != rule["os"]:
            return False, ""

        # --- 主匹配条件（互斥） ---

        # pattern: 全命令字面子串匹配
        if "pattern" in rule:
            return (True, reason) if rule["pattern"] in full_cmd else (False, "")

        # command_prefix: 前缀匹配（mkfs.ext4 等）
        if "command_prefix" in rule:
            return (True, reason) if cmd.startswith(rule["command_prefix"]) else (False, "")

        # command: 精确匹配可执行文件名
        if "command" in rule:
            if cmd != rule["command"]:
                return False, ""
        else:
            return False, ""

        # --- 附加条件（AND 逻辑，全部满足才命中） ---

        # recursive: sudo 等递归检查子命令
        if rule.get("recursive") and len(parts) > 1:
            return True, reason

        # first_arg_in: 第一个参数须在列表中
        if "first_arg_in" in rule:
            if not args or args[0] not in rule["first_arg_in"]:
                return False, ""

        # flags: 含这些 flag 才命中（OR 逻辑）
        if "flags" in rule:
            if not any(arg in rule["flags"] for arg in args):
                return False, ""

        # subcommands: 子命令匹配（第一个非 flag 参数）
        if "subcommands" in rule:
            subcmd = next((a for a in args if not a.startswith("-")), None)
            if subcmd not in rule["subcommands"]:
                return False, ""

        # target_in: 位置参数在此列表中
        if "target_in" in rule:
            targets = [a for a in args if not a.startswith("-")]
            if not any(t == d or t.startswith(d + "/") for t in targets for d in rule["target_in"]):
                return False, ""

        # args_contain: 参数含这些子串（OR 逻辑）
        if "args_contain" in rule:
            found = False
            for pat in rule["args_contain"]:
                if " " in pat:
                    if all(t in args for t in pat.split()):
                        found = True
                        break
                else:
                    if any(pat in arg for arg in args):
                        found = True
                        break
            if not found:
                return False, ""

        return True, reason


class CommandClassifier:
    """四层命令安全分类器。"""

    def __init__(self) -> None:
        self.engine = RuleEngine()

    @staticmethod
    def _parse_command(command: str) -> List[str]:
        try:
            return shlex.split(command)
        except ValueError:
            return command.split()

    @staticmethod
    def _extract_shell_commands(parts: List[str]) -> Optional[List[List[str]]]:
        """从 shell wrapper 命令解析提取内部各子命令，剔除重定向干扰。"""
        script = _extract_script_from_shell_parts(parts)
        if not script:
            return None

        raw_subcmds = _split_shell_commands(script)
        result: List[List[str]] = []
        for sc in raw_subcmds:
            sc = sc.strip()
            if not sc:
                continue
            try:
                tokens = shlex.split(sc)
            except ValueError:
                tokens = sc.split()
            if not tokens:
                continue
            cleaned, _ = _parse_redirections(tokens)
            if cleaned:
                result.append(cleaned)
        return result or None

    def _collect_redirections(self, parts: List[str], full_cmd: str) -> List[Tuple[str, str]]:
        """收集命令以及嵌套 shell/复合语句中所有的重定向项。"""
        redirs: List[Tuple[str, str]] = []
        _, r = _parse_redirections(parts)
        redirs.extend(r)

        script = _extract_script_from_shell_parts(parts)
        if script:
            for sc in _split_shell_commands(script):
                try:
                    toks = shlex.split(sc)
                except ValueError:
                    toks = sc.split()
                _, sub_r = _parse_redirections(toks)
                redirs.extend(sub_r)
        else:
            raw_subcmds = _split_shell_commands(full_cmd)
            if len(raw_subcmds) > 1:
                for sc in raw_subcmds:
                    try:
                        toks = shlex.split(sc)
                    except ValueError:
                        toks = sc.split()
                    _, sub_r = _parse_redirections(toks)
                    redirs.extend(sub_r)
        return redirs

    def _check_rules(self, rules: List[dict], parts: List[str], full_cmd: str) -> Tuple[bool, str]:
        """检查命令是否匹配规则列表。"""
        for rule in rules:
            matched, reason = self.engine.match_rule(rule, parts, full_cmd)
            if matched:
                return True, reason
        return False, ""

    def _check_with_shell_wrapper(
        self, rules: List[dict], parts: List[str], full_cmd: str
    ) -> Tuple[bool, str]:
        """检查命令（含 shell wrapper 与复合管道拆解递归）。"""
        cleaned, _ = _parse_redirections(parts)
        matched, reason = self._check_rules(rules, cleaned, " ".join(cleaned))
        if matched:
            return True, reason

        all_commands = self._extract_shell_commands(parts)
        if all_commands:
            for cmd in all_commands:
                m, r = self._check_rules(rules, cmd, " ".join(cmd))
                if m:
                    return True, r
        else:
            raw_subcmds = _split_shell_commands(full_cmd)
            if len(raw_subcmds) > 1:
                for sc in raw_subcmds:
                    try:
                        toks = shlex.split(sc)
                    except ValueError:
                        toks = sc.split()
                    c, _ = _parse_redirections(toks)
                    if c:
                        m, r = self._check_rules(rules, c, " ".join(c))
                        if m:
                            return True, r
                        nested = self._extract_shell_commands(c)
                        if nested:
                            for ncmd in nested:
                                nm, nr = self._check_rules(rules, ncmd, " ".join(ncmd))
                                if nm:
                                    return True, nr
        return False, ""

    def _is_destructive(self, parts: List[str], full_cmd: str) -> Tuple[bool, str]:
        """检查是否为毁灭性命令（含 sudo 递归与敏感重定向阻断）。"""
        underlying, is_sudo = _strip_sudo_args(parts)
        if is_sudo:
            if underlying:
                ok, reason = self._is_destructive(underlying, " ".join(underlying))
                if ok:
                    return True, f"sudo 提权 + {reason}"
            return False, ""

        redirs = self._collect_redirections(parts, full_cmd)
        sec_status, sec_reason = _check_redirection_security(redirs)
        if sec_status == "destructive":
            return True, sec_reason

        return self._check_with_shell_wrapper(DESTRUCTIVE_RULES, parts, full_cmd)

    def _is_dangerous(self, parts: List[str], full_cmd: str) -> Tuple[bool, str]:
        """检查是否为危险命令（含 sudo 提权及敏感文件重定向检查）。"""
        underlying, is_sudo = _strip_sudo_args(parts)
        if is_sudo:
            target_str = " ".join(underlying) if underlying else " ".join(parts[1:])
            return True, f"sudo 提权执行: {target_str}".strip()

        redirs = self._collect_redirections(parts, full_cmd)
        sec_status, sec_reason = _check_redirection_security(redirs)
        if sec_status == "dangerous":
            return True, sec_reason

        return self._check_with_shell_wrapper(DANGEROUS_RULES, parts, full_cmd)

    @staticmethod
    def _is_safe_command(parts: List[str]) -> Tuple[bool, str]:
        """单条命令安全检查（frozenset + 条件规则 + 特殊处理）。"""
        if not parts:
            return False, ""
        cmd = PurePath(parts[0]).name
        args = parts[1:]

        # 1. 无条件安全命令
        if cmd in SAFE_COMMANDS:
            return True, f"安全命令: {cmd}"
        if sys.platform == "linux" and cmd in SAFE_COMMANDS_LINUX:
            return True, f"安全命令(Linux): {cmd}"

        # 2. 条件安全命令
        for rule in SAFE_CONDITIONAL:
            if cmd != rule["command"]:
                continue
            deny_args = rule.get("deny_args", [])
            deny_prefixes = rule.get("deny_arg_prefixes", [])
            for arg in args:
                if arg in deny_args:
                    return False, ""
                if any(arg.startswith(d) for d in deny_args if d.endswith("=")):
                    return False, ""
                if any(arg.startswith(p) and len(arg) > len(p) for p in deny_prefixes):
                    return False, ""
            return True, rule.get("reason", f"条件安全: {cmd}")

        # 3. git 特殊处理
        if cmd == "git":
            subcmd = next((a for a in args if not a.startswith("-")), None)
            dangerous_subcmds = {"clean"}
            network_subcmds = {"clone", "fetch", "pull", "push"}
            if subcmd is None or (
                subcmd not in dangerous_subcmds and subcmd not in network_subcmds
            ):
                sub_str = subcmd or ""
                return True, f"安全 git 操作: git {sub_str}".strip()
            return False, ""

        # 4. sed 特殊处理
        if cmd == "sed":
            if not any(a == "-i" or a.startswith("-i") for a in args):
                return True, "sed 只读模式"
            return False, ""

        return False, ""

    def _is_safe(self, parts: List[str], full_cmd: str) -> Tuple[bool, str]:
        """安全命令检测（含 shell wrapper 递归与复合语句完整性检查）。"""
        if parts and PurePath(parts[0]).name == "sudo":
            return False, ""

        redirs = self._collect_redirections(parts, full_cmd)
        if _has_workspace_write_redirection(redirs):
            return False, ""

        all_commands = self._extract_shell_commands(parts)
        if all_commands:
            for cmd in all_commands:
                normalized = ["bash" if p == "zsh" else p for p in cmd]
                m, _ = self._is_safe_command(normalized)
                if not m:
                    return False, ""
            return True, "shell 中所有命令都是安全的"

        raw_subcmds = _split_shell_commands(full_cmd)
        if len(raw_subcmds) > 1:
            for sc in raw_subcmds:
                try:
                    toks = shlex.split(sc)
                except ValueError:
                    toks = sc.split()
                c, sub_r = _parse_redirections(toks)
                if not c or _has_workspace_write_redirection(sub_r):
                    return False, ""
                norm = ["bash" if p == "zsh" else p for p in c]
                m, _ = self._is_safe_command(norm)
                if not m:
                    return False, ""
            return True, "所有命令都是安全的"

        cleaned, _ = _parse_redirections(parts)
        normalized = ["bash" if p == "zsh" else p for p in cleaned]
        ok, reason = self._is_safe_command(normalized)
        if ok:
            return True, reason
        return False, ""

    @staticmethod
    def _convert_grant(grant: Optional[dict]) -> Optional[dict]:
        """将 rules.py 的 grant 格式转为 additional_permissions 格式。"""
        if not grant:
            return None
        result: Dict[str, Any] = {}
        if grant.get("network"):
            result["network"] = {"enabled": True}
        if grant.get("write_paths"):
            result.setdefault("file_system", {})["write"] = grant["write_paths"]
        return result or None

    def _lookup_permission_rules(self, parts: List[str]) -> Optional[dict]:
        """单条命令从 PERMISSION_RULES 查询匹配额外权限。"""
        if not parts:
            return None
        cmd = PurePath(parts[0]).name
        args = parts[1:]

        for rule in PERMISSION_RULES:
            if rule.get("command") != cmd:
                continue
            if "subcommands" in rule:
                subcmd = next((a for a in args if not a.startswith("-")), None)
                if subcmd not in rule["subcommands"]:
                    continue
            if "args_contain" in rule:
                if not any(pat in args for pat in rule["args_contain"]):
                    continue
            return self._convert_grant(rule.get("grant"))
        return None

    def _get_additional_permissions(self, parts: List[str]) -> Optional[dict]:
        """从 PERMISSION_RULES 查找匹配的额外权限（支持命令及嵌套提取）。"""
        cleaned, _ = _parse_redirections(parts)
        res = self._lookup_permission_rules(cleaned)
        if res:
            return res

        all_commands = self._extract_shell_commands(parts)
        if all_commands:
            for cmd in all_commands:
                res = self._lookup_permission_rules(cmd)
                if res:
                    return res
        return None

    def classify(self, command: str) -> Dict[str, Any]:
        """分类命令，返回四层分类结果 + 基础策略 + 额外权限。"""
        parts = self._parse_command(command)

        ok, reason = self._is_destructive(parts, command)
        if ok:
            return {
                "decision": "destructive",
                "reason": reason,
                "command": command,
                "additional_permissions": None,
            }

        ok, reason = self._is_dangerous(parts, command)
        if ok:
            return {
                "decision": "dangerous",
                "reason": reason,
                "command": command,
                "additional_permissions": None,
            }

        ok, reason = self._is_safe(parts, command)
        if ok:
            return {
                "decision": "safe",
                "reason": reason,
                "command": command,
                "additional_permissions": None,
            }

        return {
            "decision": "default",
            "reason": "未匹配已知分类，使用默认沙箱策略",
            "command": command,
            "additional_permissions": self._get_additional_permissions(parts),
        }


def main() -> None:
    """CLI 入口函数。"""
    parser = argparse.ArgumentParser(description="命令安全分类器（四层分类）")
    parser.add_argument("command", help="要分类的命令")
    parser.add_argument("--json", action="store_true", help="输出 JSON 格式")
    args = parser.parse_args()

    result = CommandClassifier().classify(args.command)

    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        print(f"命令: {args.command}")
        print(f"分类: {result['decision']}")
        print(f"原因: {result['reason']}")
        if result["additional_permissions"]:
            print(f"额外权限: {json.dumps(result['additional_permissions'], ensure_ascii=False)}")


if __name__ == "__main__":
    main()
