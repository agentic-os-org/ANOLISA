"""
安全规则模块
"""

from agent_sec_cli.skill_score.vendor.masb.rules.base import Match, SecurityRule
from agent_sec_cli.skill_score.vendor.masb.rules.command import (
    DangerousCommandRule,
    SystemCommandRule,
)
from agent_sec_cli.skill_score.vendor.masb.rules.dependencies import (
    GlobalInstallRule,
    VersionConflictRule,
)
from agent_sec_cli.skill_score.vendor.masb.rules.fileops import (
    FileModificationRule,
    SensitiveFileAccessRule,
)
from agent_sec_cli.skill_score.vendor.masb.rules.injection import (
    BackdoorRule,
    CodeInjectionRule,
)
from agent_sec_cli.skill_score.vendor.masb.rules.network import (
    DataExfiltrationRule,
    NetworkSecurityRule,
)

__all__ = [
    "SecurityRule",
    "Match",
    "NetworkSecurityRule",
    "DataExfiltrationRule",
    "SensitiveFileAccessRule",
    "FileModificationRule",
    "CodeInjectionRule",
    "BackdoorRule",
    "GlobalInstallRule",
    "VersionConflictRule",
    "DangerousCommandRule",
    "SystemCommandRule",
]
