"""
文件操作安全规则
"""

import re
from typing import List

from agent_sec_cli.skill_score.vendor.masb.rules.base import Match, SecurityRule


class SensitiveFileAccessRule(SecurityRule):
    """敏感文件访问规则"""

    def match(self, content: str) -> List[Match]:
        """
        匹配敏感文件访问模式

        Args:
            content: 文件内容

        Returns:
            List[Match]: 匹配结果
        """
        matches = self._find_matches(content)

        # 提高置信度的特定模式
        enhanced_matches = []
        for match in matches:
            confidence = match.confidence

            pattern = match.pattern.lower()

            # 检查高置信度模式
            if any(x in pattern for x in ["~/.ssh", "~/.env", "id_rsa", "private key"]):
                confidence = min(confidence + 0.2, 1.0)

            enhanced_matches.append(
                Match(line=match.line, pattern=match.pattern, confidence=confidence)
            )

        return enhanced_matches


class FileModificationRule(SecurityRule):
    """文件修改规则 - 检测危险的文件修改操作"""

    def match(self, content: str) -> List[Match]:
        """
        匹配危险的文件修改模式

        Args:
            content: 文件内容

        Returns:
            List[Match]: 匹配结果
        """
        matches = []
        lines = content.split("\n")

        for i, line in enumerate(lines, 1):
            # 检测删除操作
            if re.search(r"rm\s+-rf\s+/", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.95))

            # 检测权限修改
            if re.search(r"chmod\s+777|chmod\s+a+rwx", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.85))

            # 检测修改系统文件
            if re.search(r">\s+/(etc|usr|bin|sbin)/", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.90))

        return matches
