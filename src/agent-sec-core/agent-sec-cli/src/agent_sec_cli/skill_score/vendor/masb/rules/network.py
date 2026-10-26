"""
网络安全规则
"""

import re
from typing import List

from agent_sec_cli.skill_score.vendor.masb.rules.base import Match, SecurityRule


class NetworkSecurityRule(SecurityRule):
    """网络安全规则 - 检测外部网络请求"""

    def __init__(self, config: dict):
        super().__init__(config)
        # 允许的域名白名单
        self.allowed_domains = set(config.get("allowed_domains", []))

    def match(self, content: str) -> List[Match]:
        """
        匹配网络请求模式

        Args:
            content: 文件内容

        Returns:
            List[Match]: 匹配结果
        """
        matches = self._find_matches(content)

        # 过滤掉允许的域名
        filtered_matches = []
        for match in matches:
            if not self._is_allowed_domain(match.pattern):
                filtered_matches.append(match)

        return filtered_matches

    def _is_allowed_domain(self, pattern: str) -> bool:
        """
        检查是否为允许的域名

        Args:
            pattern: 匹配的模式

        Returns:
            bool: 是否允许
        """
        for domain in self.allowed_domains:
            if domain in pattern.lower():
                return True
        return False


class DataExfiltrationRule(SecurityRule):
    """数据外传规则 - 检测数据外传模式"""

    def match(self, content: str) -> List[Match]:
        """
        匹配数据外传模式

        Args:
            content: 文件内容

        Returns:
            List[Match]: 匹配结果
        """
        matches = []
        lines = content.split("\n")

        for i, line in enumerate(lines, 1):
            # 检测 curl/wget 带 -d (POST data) 参数
            if re.search(r"curl|wget", line, re.IGNORECASE):
                if re.search(r"-d|--data|--post", line, re.IGNORECASE):
                    # 检查是否发送敏感文件
                    if re.search(r"@.*\.ssh|@.*\.env|@.*credential|@.*secret", line, re.IGNORECASE):
                        matches.append(Match(line=i, pattern=line.strip(), confidence=0.95))

        return matches
