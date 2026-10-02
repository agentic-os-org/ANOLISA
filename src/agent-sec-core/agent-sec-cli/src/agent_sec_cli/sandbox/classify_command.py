#!/usr/bin/env python3
"""
命令安全分类器 - 四层分类 + 统一规则引擎

分类层级（优先级从高到低）：
1. destructive → 直接拒绝，不进沙箱
2. dangerous   → 沙箱执行，禁止自动补权限
3. safe        → 沙箱执行，无需补权限
4. default     → 沙箱执行，可自动补最小权限

注：分类直接决定沙箱策略（safe→只读，dangerous/default→workspace-write），分类层同时控制是否允许扩权。

用法：
    python3 classify_command.py "git status"
    python3 classify_command.py --json "rm -rf /"
"""

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
    SHELL_UNSAFE_OPERATORS,
)


class RuleEngine:
    """统一规则匹配引擎 - 支持 rules.py 中所有 Match Schema 字段"""

    @staticmethod
    def match_rule(rule: dict, parts: List[str], full_cmd: str) -> Tuple[bool, str]:
        """
        检查命令是否匹配规则。
        返回 (matched, reason)
        """
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
            return (
                (True, reason)
                if cmd.startswith(rule["command_prefix"])
                else (False, "")
            )

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
            if not any(
                t == d or t.startswith(d + "/")
                for t in targets
                for d in rule["target_in"]
            ):
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
        """从 bash -c "..." 形式提取内部命令"""
        if len(parts) < 3:
            return None

        cmd = PurePath(parts[0]).name
        if cmd not in ("bash", "sh", "zsh") or parts[1] not in ("-c", "-lc"):
            return None

        script = parts[2]
        if any(op in script for op in SHELL_UNSAFE_OPERATORS):
            return None

        result = []
        for s in re.split(r"\s*(?:&&|\|\||;|\|)\s*", script):
            s = s.strip()
            if s:
                try:
                    result.append(shlex.split(s))
                except ValueError:
                    return None
        return result or None

    def _check_rules(
        self, rules: List[dict], parts: List[str], full_cmd: str
    ) -> Tuple[bool, str]:
        """检查命令是否匹配规则列表"""
        for rule in rules:
            matched, reason = self.engine.match_rule(rule, parts, full_cmd)
            if matched:
                return True, reason
        return False, ""

    def _check_with_shell_wrapper(
        self, rules: List[dict], parts: List[str], full_cmd: str
    ) -> Tuple[bool, str]:
        """检查命令（含 shell wrapper 递归，任一子命令匹配即可）"""
        matched, reason = self._check_rules(rules, parts, full_cmd)
        if matched:
            return True, reason
        all_commands = self._extract_shell_commands(parts)
        if all_commands:
            for cmd in all_commands:
                m, r = self._check_rules(rules, cmd, " ".join(cmd))
                if m:
                    return True, r
        return False, ""

    # --- 四层检测 ---

    def _is_destructive(self, parts: List[str], full_cmd: str) -> Tuple[bool, str]:
        """检查是否为毁灭性命令（含 sudo 递归）"""
        if parts and PurePath(parts[0]).name == "sudo" and len(parts) > 1:
            ok, reason = self._is_destructive(parts[1:], " ".join(parts[1:]))
            if ok:
                return True, f"sudo 提权 + {reason}"
        return self._check_with_shell_wrapper(DESTRUCTIVE_RULES, parts, full_cmd)

    def _is_dangerous(self, parts: List[str], full_cmd: str) -> Tuple[bool, str]:
        """检查是否为危险命令"""
        # sudo 递归：所有 sudo 子命令视为 dangerous（destructive 层已优先检查）
        if parts and PurePath(parts[0]).name == "sudo" and len(parts) > 1:
            return True, f"sudo 提权执行: {' '.join(parts[1:])}"
        return self._check_with_shell_wrapper(DANGEROUS_RULES, parts, full_cmd)

    @staticmethod
    def _is_safe_command(parts: List[str]) -> Tuple[bool, str]:
        """单条命令安全检查（frozenset + 条件规则 + 特殊处理）"""
        if not parts:
            return False, ""
        cmd = PurePath(parts[0]).name
        args = parts[1:]

        # 1. 无条件安全命令
        if cmd in SAFE_COMMANDS:
            return True, f"安全命令: {cmd}"
        if sys.platform == "linux" and cmd in SAFE_COMMANDS_LINUX:
            return True, f"安全命令(Linux): {cmd}"

        # 2. 条件安全命令（deny_args / deny_arg_prefixes 检查）
        for rule in SAFE_CONDITIONAL:
            if cmd != rule["command"]:
                continue
            deny_args = rule.get("deny_args", [])
            deny_prefixes = rule.get("deny_arg_prefixes", [])
            for arg in args:
                # 精确匹配
                if arg in deny_args:
                    return False, ""
                # = 结尾做前缀匹配（--output= 匹配 --output=file.txt）
                if any(arg.startswith(d) for d in deny_args if d.endswith("=")):
                    return False, ""
                # 前缀匹配（-o 匹配 -ofile.txt）
                if any(arg.startswith(p) and len(arg) > len(p) for p in deny_prefixes):
                    return False, ""
            return True, rule.get("reason", f"条件安全: {cmd}")

        # 3. git 特殊处理：非危险 / 非网络子命令视为安全
        if cmd == "git":
            subcmd = next((a for a in args if not a.startswith("-")), None)
            dangerous_subcmds = {"clean"}
            network_subcmds = {"clone", "fetch", "pull", "push"}
            if subcmd is None or (
                subcmd not in dangerous_subcmds and subcmd not in network_subcmds
            ):
                return True, f"安全 git 操作: git {subcmd or ''}"
            return False, ""

        # 4. sed 特殊处理：无 -i 时为只读
        if cmd == "sed":
            if not any(a == "-i" or a.startswith("-i") for a in args):
                return True, "sed 只读模式"
            return False, ""

        return False, ""

    def _is_safe(self, parts: List[str], full_cmd: str) -> Tuple[bool, str]:
        """安全命令检测（含 shell wrapper 递归，所有子命令须安全）"""
        normalized = ["bash" if p == "zsh" else p for p in parts]
        ok, reason = self._is_safe_command(normalized)
        if ok:
            return True, reason
        # shell wrapper 递归：bash -c "cmd1 && cmd2" 所有子命令都安全才算安全
        all_commands = self._extract_shell_commands(normalized)
        if all_commands:
            for cmd in all_commands:
                m, _ = self._is_safe_command(cmd)
                if not m:
                    return False, ""
            return True, "shell 中所有命令都是安全的"
        return False, ""

    # --- 额外权限 ---

    @staticmethod
    def _convert_grant(grant: Optional[dict]) -> Optional[dict]:
        """将 rules.py 的 grant 格式转为 additional_permissions 格式"""
        if not grant:
            return None
        result = {}
        if grant.get("network"):
            result["network"] = {"enabled": True}
        if grant.get("write_paths"):
            result.setdefault("file_system", {})["write"] = grant["write_paths"]
        return result or None

    def _get_additional_permissions(self, parts: List[str]) -> Optional[dict]:
        """从 PERMISSION_RULES 查找匹配的额外权限（优先匹配更具体的规则）"""
        if not parts:
            return None
        cmd = PurePath(parts[0]).name
        args = parts[1:]

        for rule in PERMISSION_RULES:
            if rule.get("command") != cmd:
                continue
            # 子命令匹配
            if "subcommands" in rule:
                subcmd = next((a for a in args if not a.startswith("-")), None)
                if subcmd not in rule["subcommands"]:
                    continue
            # args_contain 匹配（参数须包含指定值）
            if "args_contain" in rule:
                if not any(pat in args for pat in rule["args_contain"]):
                    continue
            return self._convert_grant(rule.get("grant"))
        return None

    # --- 主入口 ---

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
                "additional_permissions": None,  # dangerous 禁止自动补权限
            }

        ok, reason = self._is_safe(parts, command)
        if ok:
            return {
                "decision": "safe",
                "reason": reason,
                "command": command,
                "additional_permissions": None,  # safe 不需要补权限
            }

        # default: 唯一允许自动补权限的类别
        return {
            "decision": "default",
            "reason": "未匹配已知分类，使用默认沙箱策略",
            "command": command,
            "additional_permissions": self._get_additional_permissions(parts),
        }


def main():
    import argparse

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
            print(
                f"额外权限: {json.dumps(result['additional_permissions'], ensure_ascii=False)}"
            )


if __name__ == "__main__":
    main()
