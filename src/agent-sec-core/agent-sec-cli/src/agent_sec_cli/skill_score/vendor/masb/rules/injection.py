"""
代码注入安全规则
"""

import re
from typing import List

from agent_sec_cli.skill_score.vendor.masb.rules.base import Match, SecurityRule


class CodeInjectionRule(SecurityRule):
    """代码注入规则"""

    def match(self, content: str) -> List[Match]:
        """
        匹配代码注入模式

        Args:
            content: 文件内容

        Returns:
            List[Match]: 匹配结果
        """
        matches = []
        lines = content.split("\n")

        for i, line in enumerate(lines, 1):
            # 检测 eval/exec
            if re.search(r"\beval\s*\(|\bexec\s*\(", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.80))

            # 检测 __import__ 动态导入
            if re.search(r"__import__\s*\(", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.75))

            # 检测 compile 函数
            if re.search(r"\bcompile\s*\(", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.70))

            # 检测 inject/insert into file 模式
            if re.search(r"(inject|insert|prepend).*into.*file", line, re.IGNORECASE):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.85))

        return matches


class BackdoorRule(SecurityRule):
    """后门规则 - 检测后门植入模式"""

    def match(self, content: str) -> List[Match]:
        """
        匹配后门植入模式

        Args:
            content: 文件内容

        Returns:
            List[Match]: 匹配结果
        """
        matches = []
        lines = content.split("\n")

        for i, line in enumerate(lines, 1):
            # 检测反向 shell
            if re.search(r"bash\s+-i\s+>&\s*/dev/tcp/", line):
                matches.append(Match(line=i, pattern=line.strip(), confidence=0.98))

            # 检测 netcat 连接
            if re.search(r"nc\s+-|netcat\s+-", line):
                if re.search(r"-e\s+/", line):
                    matches.append(Match(line=i, pattern=line.strip(), confidence=0.95))

            # 检测条件触发的后门
            if re.search(r"(if|when).*PROD.*then", line, re.IGNORECASE):
                if re.search(r"(system|exec|eval|shell)", line, re.IGNORECASE):
                    matches.append(Match(line=i, pattern=line.strip(), confidence=0.90))

        return matches
