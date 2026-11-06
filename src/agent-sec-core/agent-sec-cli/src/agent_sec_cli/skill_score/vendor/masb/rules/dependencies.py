"""
依赖安全规则
"""

import re
from typing import List

from agent_sec_cli.skill_score.vendor.masb.rules.base import Match, SecurityRule


class GlobalInstallRule(SecurityRule):
    """全局安装规则"""

    def match(self, content: str) -> List[Match]:
        """
        匹配全局包安装模式

        Args:
            content: 文件内容

        Returns:
            List[Match]: 匹配结果
        """
        matches = []
        lines = content.split("\n")

        for i, line in enumerate(lines, 1):
            # 检测 pip 全局安装
            if re.search(r"pip\s+install.*--global", line, re.IGNORECASE):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.85))

            # 检测 npm 全局安装
            if re.search(r"npm\s+install.*-g|npm\s+i.*-g", line, re.IGNORECASE):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.85))

            # 检测 yarn 全局安装
            if re.search(r"yarn\s+global", line, re.IGNORECASE):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.85))

        return matches


class VersionConflictRule(SecurityRule):
    """版本冲突规则"""

    def match(self, content: str) -> List[Match]:
        """
        匹配可能导致版本冲突的模式

        Args:
            content: 文件内容

        Returns:
            List[Match]: 匹配结果
        """
        matches = []
        lines = content.split("\n")

        for i, line in enumerate(lines, 1):
            # 检测强制安装特定版本
            if re.search(r"install.*==.*force", line, re.IGNORECASE):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.70))

            # 检测覆盖已安装包
            if re.search(r"--upgrade|--force-reinstall", line, re.IGNORECASE):
                if re.search(r"pip\s+install", line, re.IGNORECASE):
                    matches.append(Match(line=i, pattern=line.strip(), confidence=0.75))

        return matches
