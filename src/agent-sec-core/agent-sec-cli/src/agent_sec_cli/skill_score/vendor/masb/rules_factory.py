"""
规则工厂 - 根据配置创建规则实例
"""

from typing import Any, Dict, List

from agent_sec_cli.skill_score.vendor.masb.rules import (
    BackdoorRule,
    CodeInjectionRule,
    DangerousCommandRule,
    DataExfiltrationRule,
    FileModificationRule,
    GlobalInstallRule,
    NetworkSecurityRule,
    SensitiveFileAccessRule,
    SystemCommandRule,
    VersionConflictRule,
)
from agent_sec_cli.skill_score.vendor.masb.rules.base import SecurityRule


class RulesFactory:
    """规则工厂"""

    # 规则 ID 前缀到规则类的映射
    RULE_CLASSES = {
        "NET": [NetworkSecurityRule, DataExfiltrationRule],
        "FILE": [SensitiveFileAccessRule, FileModificationRule],
        "CMD": [DangerousCommandRule, SystemCommandRule],
        "INJ": [CodeInjectionRule, BackdoorRule],
        "DEP": [GlobalInstallRule, VersionConflictRule],
        "OBF": [CodeInjectionRule],  # 混淆规则使用代码注入规则
        # 具体规则ID映射
        "FILE001": [SensitiveFileAccessRule],
        "FILE002": [FileModificationRule],
        "CMD001": [DangerousCommandRule],
        "CMD002": [SystemCommandRule],
        "INJ001": [CodeInjectionRule],
        "INJ002": [CodeInjectionRule],
        "INJ003": [BackdoorRule],
        "DEP001": [GlobalInstallRule],
        "DEP002": [VersionConflictRule],
        "OBF001": [CodeInjectionRule],
        "OBF002": [CodeInjectionRule],
    }

    @classmethod
    def create_rules(cls, rules_config: List[Dict[str, Any]]) -> List:
        """
        根据配置创建规则实例

        Args:
            rules_config: 规则配置列表

        Returns:
            List: 规则实例列表
        """
        rules = []

        for config in rules_config:
            try:
                rule = cls._create_rule(config)
                if rule:
                    rules.append(rule)
            except Exception as e:
                print(f"Warning: Failed to create rule {config.get('id')}: {e}")
                continue

        return rules

    @classmethod
    def _create_rule(cls, config: Dict[str, Any]):
        """
        创建单个规则

        Args:
            config: 规则配置

        Returns:
            SecurityRule: 规则实例
        """
        rule_id = config.get("id", "")

        # 首先尝试直接匹配完整规则ID
        if rule_id in cls.RULE_CLASSES:
            rule_classes = cls.RULE_CLASSES[rule_id]
            for rule_class in rule_classes:
                try:
                    return rule_class(config)
                except Exception:
                    continue

        # 根据 ID 前缀选择规则类
        prefix = rule_id.split("_")[0] if "_" in rule_id else rule_id[:3]

        # 尝试匹配规则类
        for prefix_key, rule_classes in cls.RULE_CLASSES.items():
            if prefix.startswith(prefix_key):
                # 尝试每个规则类
                for rule_class in rule_classes:
                    try:
                        return rule_class(config)
                    except Exception:
                        continue

        # 如果没有匹配的类，使用默认的 SecurityRule
        return SecurityRule(config)
