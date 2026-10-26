"""
安全规则基类
"""

import re
from abc import ABC, abstractmethod
from typing import Any, Dict, List


class Match:
    """规则匹配结果"""

    def __init__(self, line: int, pattern: str, confidence: float = 0.8):
        self.line = line
        self.pattern = pattern
        self.confidence = confidence


class SecurityRule(ABC):
    """安全规则基类"""

    def __init__(self, config: Dict[str, Any]):
        """
        初始化规则

        Args:
            config: 规则配置
        """
        self.id = config["id"]
        self.name = config["name"]
        self.severity = config["severity"]
        self.patterns = config["patterns"]
        self.description = config["description"]

        # 编译正则表达式
        self.compiled_patterns = [re.compile(pattern, re.IGNORECASE) for pattern in self.patterns]

    @abstractmethod
    def match(self, content: str) -> List[Match]:
        """
        匹配规则

        Args:
            content: 文件内容

        Returns:
            List[Match]: 匹配结果列表
        """
        pass

    def _find_matches(self, content: str) -> List[Match]:
        """
        在内容中查找匹配

        Args:
            content: 文件内容

        Returns:
            List[Match]: 匹配结果
        """
        matches = []
        lines = content.split("\n")

        for i, line in enumerate(lines, 1):
            for pattern in self.compiled_patterns:
                if pattern.search(line):
                    confidence = self._calculate_confidence(line)
                    matches.append(Match(line=i, pattern=line.strip(), confidence=confidence))
                    break  # 一个行只匹配一次

        return matches

    def _calculate_confidence(self, line: str) -> float:
        """
        计算置信度

        Args:
            line: 匹配的行

        Returns:
            float: 置信度 (0-1)
        """
        # 基础置信度
        confidence = 0.7

        # 如果包含注释，降低置信度
        if "#" in line or "//" in line:
            confidence -= 0.2

        # 如果包含明显的恶意关键词，提高置信度
        suspicious_keywords = [
            "password",
            "secret",
            "token",
            "key",
            "credential",
            "eval",
            "exec",
            "system",
            "shell",
            "bash",
            "curl",
            "wget",
            "nc ",
            "netcat",
        ]

        if any(kw in line.lower() for kw in suspicious_keywords):
            confidence += 0.2

        return min(max(confidence, 0.0), 1.0)
