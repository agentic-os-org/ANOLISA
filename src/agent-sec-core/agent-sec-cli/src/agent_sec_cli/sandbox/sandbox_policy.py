#!/usr/bin/env python3
"""
沙箱策略生成器 — 一步完成命令分类 + linux-sandbox 命令行生成

输入一条 shell 命令和工作目录，输出：
- destructive → deny（拒绝执行）
- 其他分类   → sandbox（含完整的 linux-sandbox 命令行参数）

用法：
    python3 sandbox_policy.py --cwd /workspace "git status"
    python3 sandbox_policy.py --cwd /workspace "rm -rf /"
"""

import json
import shlex
from typing import Any, Dict, List, Optional

from agent_sec_cli.sandbox.classify_command import CommandClassifier

# ============================================================================
# linux-sandbox FileSystemSandboxPolicy JSON 构建
# ============================================================================


def _special_entry(kind: str, access: str) -> dict[str, Any]:
    """构建 Special 类型的 FileSystemSandboxEntry。"""
    return {
        "path": {"type": "special", "value": {"kind": kind}},
        "access": access,
    }


def _absolute_path_entry(path: str, access: str) -> dict[str, Any]:
    """构建绝对路径类型的 FileSystemSandboxEntry。"""
    return {
        "path": {"type": "path", "path": path},
        "access": access,
    }


# 基础策略模板：分类 → FileSystemSandboxPolicy entries
_READ_ONLY_ENTRIES = [
    _special_entry("root", "read"),
]

_WORKSPACE_WRITE_ENTRIES = [
    _special_entry("root", "read"),
    _special_entry("current_working_directory", "write"),
    _special_entry("slash_tmp", "write"),
]


class SandboxPolicyBuilder:
    """根据分类结果 + cwd 生成 linux-sandbox 命令行参数。"""

    LINUX_SANDBOX_BIN = "linux-sandbox"

    @staticmethod
    def build(classification: Dict[str, Any], cwd: str) -> Dict[str, Any]:
        """
        根据分类结果生成最终的沙箱策略。

        返回格式：
        - destructive: {"decision": "deny", "classification": "destructive", "reason": "..."}
        - 其他:       {"decision": "sandbox", "classification": "...", "reason": "...",
                       "sandbox_argv": [...], "sandbox_command": "..."}
        """
        decision = classification["decision"]
        reason = classification["reason"]
        command = classification["command"]

        if decision == "destructive":
            return {
                "decision": "deny",
                "classification": "destructive",
                "reason": reason,
            }

        sandbox_mode = "read-only" if decision == "safe" else "workspace-write"

        filesystem_policy = SandboxPolicyBuilder._build_filesystem_policy(
            decision, classification.get("additional_permissions")
        )
        network_policy = SandboxPolicyBuilder._build_network_policy(
            decision, classification.get("additional_permissions")
        )

        sandbox_argv = SandboxPolicyBuilder._build_argv(
            cwd, filesystem_policy, network_policy, command
        )
        sandbox_command = SandboxPolicyBuilder._build_shell_command(
            cwd, filesystem_policy, network_policy, command
        )
        sandbox_summary = SandboxPolicyBuilder._build_summary(
            sandbox_mode, network_policy, filesystem_policy
        )

        return {
            "decision": "sandbox",
            "classification": decision,
            "sandbox_mode": sandbox_mode,
            "reason": reason,
            "sandbox_summary": sandbox_summary,
            "sandbox_argv": sandbox_argv,
            "sandbox_command": sandbox_command,
        }

    @staticmethod
    def _build_filesystem_policy(
        decision: str, additional_permissions: Optional[dict]
    ) -> dict:
        """根据分类和额外权限构建 FileSystemSandboxPolicy JSON。"""
        if decision == "safe":
            entries = list(_READ_ONLY_ENTRIES)
        else:
            entries = list(_WORKSPACE_WRITE_ENTRIES)

        # 仅 default 分类允许合并额外权限
        if decision == "default" and additional_permissions:
            filesystem_perms = additional_permissions.get("file_system", {})
            for write_path in filesystem_perms.get("write", []):
                entries.append(_absolute_path_entry(write_path, "write"))
            for read_path in filesystem_perms.get("read", []):
                entries.append(_absolute_path_entry(read_path, "read"))

        return {"kind": "restricted", "entries": entries}

    @staticmethod
    def _build_network_policy(
        decision: str, additional_permissions: Optional[dict]
    ) -> str:
        """根据分类和额外权限确定 NetworkSandboxPolicy。"""
        if (
            decision == "default"
            and additional_permissions
            and additional_permissions.get("network", {}).get("enabled")
        ):
            return "enabled"
        return "restricted"

    @staticmethod
    def _build_argv(
        cwd: str,
        filesystem_policy: dict,
        network_policy: str,
        command: str,
    ) -> List[str]:
        """拼接 linux-sandbox 命令行参数数组（供 subprocess 使用）。"""
        filesystem_policy_json = json.dumps(filesystem_policy, separators=(",", ":"))
        network_policy_json = json.dumps(network_policy)

        argv = [
            SandboxPolicyBuilder.LINUX_SANDBOX_BIN,
            "--sandbox-policy-cwd",
            cwd,
            "--file-system-sandbox-policy",
            filesystem_policy_json,
            "--network-sandbox-policy",
            network_policy_json,
        ]

        argv.append("--")

        try:
            argv.extend(shlex.split(command))
        except ValueError:
            argv.extend(command.split())

        return argv

    @staticmethod
    def _build_shell_command(
        cwd: str,
        filesystem_policy: dict,
        network_policy: str,
        command: str,
    ) -> str:
        """生成人类可读的 shell 命令预览（隐藏冗长的 JSON 策略细节）。"""
        entry_count = len(filesystem_policy.get("entries", []))
        return (
            f"linux-sandbox"
            f" --sandbox-policy-cwd {shlex.quote(cwd)}"
            f" --file-system-sandbox-policy '<{entry_count} entries>'"
            f" --network-sandbox-policy {network_policy}"
            f" -- {command}"
        )

    @staticmethod
    def _build_summary(
        sandbox_mode: str,
        network_policy: str,
        filesystem_policy: dict,
    ) -> str:
        """生成人类可读的策略摘要（供 AI 解释使用）。"""
        network_label = "开启" if network_policy == "enabled" else "禁止"

        extra_write_paths = []
        for entry in filesystem_policy.get("entries", []):
            path_info = entry.get("path", {})
            if entry.get("access") == "write" and path_info.get("type") == "path":
                extra_write_paths.append(path_info["path"])

        parts = [f"{sandbox_mode} 模式", f"网络: {network_label}"]
        if extra_write_paths:
            parts.append(f"额外写路径: {', '.join(extra_write_paths)}")

        return " | ".join(parts)


def generate_sandbox_policy(command: str, cwd: str) -> Dict[str, Any]:
    """一步完成：命令分类 → 沙箱策略生成。"""
    classification = CommandClassifier().classify(command)
    return SandboxPolicyBuilder.build(classification, cwd)


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(
        description="沙箱策略生成器（命令分类 + linux-sandbox 命令行生成）"
    )
    parser.add_argument("command", help="要执行的 shell 命令")
    parser.add_argument("--cwd", required=True, help="命令的工作目录（绝对路径）")
    args = parser.parse_args()

    result = generate_sandbox_policy(args.command, args.cwd)
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
