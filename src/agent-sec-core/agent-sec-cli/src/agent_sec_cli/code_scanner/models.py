from enum import Enum
from typing import List, Optional

from agent_sec_cli import __version__
from pydantic import BaseModel


class Language(str, Enum):
    BASH = "bash"
    PYTHON = "python"


class Severity(str, Enum):
    WARN = "warn"
    DENY = "deny"


class Verdict(str, Enum):
    PASS = "pass"
    WARN = "warn"
    DENY = "deny"
    ERROR = "error"


class RuleDefinition(BaseModel):
    rule_id: str
    cwe_id: str
    desc_en: str
    desc_zh: str
    regex: str
    severity: Severity
    target_regexes: Optional[List[str]] = None


class Finding(BaseModel):
    rule_id: str
    severity: Severity
    desc_zh: str
    desc_en: str
    evidence: List[str]


class ScanResult(BaseModel):
    ok: bool
    verdict: Verdict
    summary: str
    findings: List[Finding]
    language: Language
    engine_version: str = __version__
    elapsed_ms: int
