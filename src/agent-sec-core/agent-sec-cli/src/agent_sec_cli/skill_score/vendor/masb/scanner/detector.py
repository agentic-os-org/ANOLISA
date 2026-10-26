"""
安全检测器 - 整合解析和分析流程
"""

from typing import Any, Dict, List

from agent_sec_cli.skill_score.vendor.masb.scanner.analyzer import SkillAnalyzer
from agent_sec_cli.skill_score.vendor.masb.scanner.parser import SkillParser


class SecurityDetector:
    """安全检测器 - 主控制器"""

    def __init__(self, rules: List[Any], whitelist: List[str] = None):
        """
        初始化检测器

        Args:
            rules: 安全规则列表
            whitelist: 白名单规则ID列表
        """
        self.rules = rules
        self.whitelist = whitelist or []
        self.parser = None
        self.analyzer = SkillAnalyzer(rules, whitelist)

    def scan(self, skill_path: str) -> Dict[str, Any]:
        """
        扫描 Skill 目录

        Args:
            skill_path: Skill 路径

        Returns:
            Dict: 扫描报告
        """
        # 解析 Skill 结构
        self.parser = SkillParser(skill_path)
        parsed_data = self.parser.parse()

        # 分析安全风险并将结果保存到 analyzer
        findings = self.analyzer.analyze_files(parsed_data["files"])
        self.analyzer.findings = findings  # 保存 findings 以便 generate_report 使用

        # 生成报告
        report = self.analyzer.generate_report()

        # 添加元数据
        report.update(
            {
                "skill_path": skill_path,
                "skill_name": parsed_data["metadata"]["name"],
                "total_files": len(parsed_data["files"]),
                "has_skill_md": parsed_data["metadata"]["has_skill_md"],
            }
        )

        return report

    def scan_multiple(self, skill_paths: List[str]) -> List[Dict[str, Any]]:
        """
        扫描多个 Skills

        Args:
            skill_paths: Skill 路径列表

        Returns:
            List[Dict]: 扫描报告列表
        """
        reports = []

        for path in skill_paths:
            try:
                report = self.scan(path)
                reports.append(report)
            except Exception as e:
                print(f"Error scanning {path}: {e}")
                continue

        return reports
