"""
命令执行安全规则
"""

import re
from typing import List

from agent_sec_cli.skill_score.vendor.masb.rules.base import Match, SecurityRule


class DangerousCommandRule(SecurityRule):
    """危险命令规则"""

    def match(self, content: str) -> List[Match]:
        """
        匹配危险命令执行模式

        Args:
            content: 文件内容

        Returns:
            List[Match]: 匹配结果
        """
        matches = []
        lines = content.split("\n")

        for i, line in enumerate(lines, 1):
            # 检测 sudo 执行
            if re.search(r"\bsudo\b", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.75))

            # 检测 rm -rf
            if re.search(r"rm\s+-rf\s+/", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.95))

            # 检测重定向到设备文件
            if re.search(r">\s*/dev/(tcp|udp|null)", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.90))

            # 检测 dd 命令
            if re.search(r"\bdd\s+.*of=/", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.90))

        return matches


class SystemCommandRule(SecurityRule):
    """系统命令规则"""

    def match(self, content: str) -> List[Match]:
        """
        匹配系统命令执行模式

        Args:
            content: 文件内容

        Returns:
            List[Match]: 匹配结果
        """
        matches = []
        lines = content.split("\n")

        for i, line in enumerate(lines, 1):
            # 检测 os.system
            if re.search(r"\bos\.system\s*\(", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.80))

            # 检测 subprocess.call/shell=True
            if re.search(r"subprocess\.(call|run|Popen).*shell\s*=\s*True", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.85))

            # 检测 exec 函数
            if re.search(r"\bexec\s*\(", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.75))

        return matches
