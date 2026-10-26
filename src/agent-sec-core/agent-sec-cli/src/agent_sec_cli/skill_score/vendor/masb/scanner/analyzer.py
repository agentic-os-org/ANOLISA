"""
代码分析器 - 分析文件内容并检测安全风险
"""

from typing import Any, Dict, List


class Finding:
    """安全发现"""

    def __init__(
        self,
        rule_id: str,
        severity: str,
        file: str,
        line: int,
        pattern: str,
        description: str,
        confidence: float = 0.8,
    ):
        self.rule_id = rule_id
        self.severity = severity
        self.file = file
        self.line = line
        self.pattern = pattern
        self.description = description
        self.confidence = confidence

    def to_dict(self) -> dict:
        """转换为字典"""
        return {
            "rule_id": self.rule_id,
            "severity": self.severity,
            "file": self.file,
            "line": self.line,
            "pattern": self.pattern,
            "description": self.description,
            "confidence": self.confidence,
        }


class Match:
    """规则匹配结果"""

    def __init__(self, line: int, pattern: str, confidence: float = 0.8):
        self.line = line
        self.pattern = pattern
        self.confidence = confidence


class SkillAnalyzer:
    """Skill 安全分析器"""

    def __init__(self, rules: List[Any], whitelist: List[str] = None):
        """
        初始化分析器

        Args:
            rules: 安全规则列表
            whitelist: 白名单规则ID列表
        """
        self.rules = rules
        self.whitelist = whitelist or []
        self.findings: List[Finding] = []

    def analyze_file(self, file_path: str, content: str) -> List[Finding]:
        """
        分析单个文件

        Args:
            file_path: 文件路径
            content: 文件内容

        Returns:
            List[Finding]: 发现的安全问题列表
        """
        findings = []

        for rule in self.rules:
            # 跳过白名单中的规则
            if rule.id in self.whitelist:
                continue

            matches = rule.match(content)
            for match in matches:
                finding = Finding(
                    rule_id=rule.id,
                    severity=rule.severity,
                    file=file_path,
                    line=match.line,
                    pattern=match.pattern,
                    description=rule.description,
                    confidence=match.confidence,
                )
                findings.append(finding)

        return findings

    def analyze_files(self, files: List[Dict[str, Any]]) -> List[Finding]:
        """
        分析多个文件

        Args:
            files: 文件信息列表

        Returns:
            List[Finding]: 所有发现的安全问题
        """
        all_findings = []

        for file_info in files:
            file_path = file_info["path"]

            try:
                with open(file_path, "r", encoding="utf-8", errors="ignore") as f:
                    content = f.read()

                findings = self.analyze_file(file_path, content)
                all_findings.extend(findings)

            except Exception as e:
                print(f"Warning: Could not read file {file_path}: {e}")
                continue

        return all_findings

    def generate_report(self) -> Dict[str, Any]:
        """
        生成分析报告

        Returns:
            Dict: 报告数据
        """
        # 统计各级别问题数量
        summary = {"CRITICAL": 0, "HIGH": 0, "WARNING": 0, "INFO": 0}

        for finding in self.findings:
            severity = finding.severity
            if severity in summary:
                summary[severity] += 1

        # 计算风险分数
        risk_score = self._calculate_risk_score(self.findings)
        risk_level = self._get_risk_level(risk_score)

        return {
            "findings": [f.to_dict() for f in self.findings],
            "summary": summary,
            "risk_score": risk_score,
            "risk_level": risk_level,
            "total_issues": len(self.findings),
        }

    def _calculate_risk_score(self, findings: List[Finding]) -> float:
        """
        计算风险分数 (0-10)

        Args:
            findings: 发现列表

        Returns:
            float: 风险分数
        """
        weights = {"CRITICAL": 10, "HIGH": 7, "WARNING": 4, "INFO": 1}

        total_score = 0.0
        for finding in findings:
            weight = weights.get(finding.severity, 1)
            # 考虑置信度
            total_score += weight * finding.confidence

        # 归一化到 0-10
        # 假设最多 20 个问题达到最高分数
        max_score = 20 * 10 * 1.0
        normalized = (total_score / max_score) * 10

        return min(normalized, 10.0)

    def _get_risk_level(self, score: float) -> str:
        """
        根据分数返回风险等级

        Args:
            score: 风险分数

        Returns:
            str: 风险等级
        """
        if score >= 8:
            return "CRITICAL"
        elif score >= 6:
            return "HIGH"
        elif score >= 4:
            return "MEDIUM"
        elif score >= 2:
            return "LOW"
        else:
            return "SAFE"

    def get_recommendation(self) -> str:
        """
        获取安全建议

        Returns:
            str: 建议文本
        """
        critical_count = sum(1 for f in self.findings if f.severity == "CRITICAL")
        high_count = sum(1 for f in self.findings if f.severity == "HIGH")

        if critical_count > 0:
            return "DO_NOT_USE: 检测到严重安全问题，强烈建议不要使用此 Skill"
        elif high_count >= 3:
            return "NOT_RECOMMENDED: 检测到多个高风险问题，建议谨慎使用"
        elif high_count > 0:
            return "REVIEW_NEEDED: 检测到风险问题，请人工审查后使用"
        else:
            return "SAFE: 未检测到明显安全问题"
