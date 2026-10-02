"""Load and validate user-defined PII regex rules."""

import hashlib
import logging
import re
import threading
from dataclasses import dataclass
from enum import StrEnum
from pathlib import Path
from typing import Any

import regex as regex_engine
import yaml
from agent_sec_cli.pii_checker.detectors.regex import BUILTIN_PII_TYPES
from agent_sec_cli.pii_checker.models import PiiSeverity
from pydantic import BaseModel, ConfigDict, Field, ValidationError

MAX_RULES_FILE_BYTES = 256 * 1024
MAX_CUSTOM_RULES = 100
MAX_REGEX_LENGTH = 2_048
MAX_REGEX_GROUP_DEPTH = 64
RULE_MATCH_TIMEOUT_SECONDS = 0.020

_TYPE_PATTERN = re.compile(r"^[a-z][a-z0-9_]{0,63}$")
_LOGGER = logging.getLogger(__name__)
_CACHE_LOCK = threading.Lock()
_CACHE_KEY: tuple[Path, str] | None = None
_CACHE_VALUE: "CustomPiiRuleSet | None" = None
_REGEX_VALIDATION_ERRORS = (
    regex_engine.error,
    TimeoutError,
    RecursionError,
    OverflowError,
    ValueError,
    KeyError,
    MemoryError,
)


class CustomRuleStatus(StrEnum):
    """Configuration state exposed in PII scan summaries."""

    ABSENT = "absent"
    LOADED = "loaded"
    INVALID = "invalid"


class _CustomPiiRuleConfig(BaseModel):
    """Strict schema for one YAML rule before regex compilation."""

    model_config = ConfigDict(extra="forbid", populate_by_name=True)

    pii_type: str = Field(alias="type", strict=True, min_length=1, max_length=64)
    regex: str = Field(strict=True, min_length=1, max_length=MAX_REGEX_LENGTH)
    severity: PiiSeverity = PiiSeverity.DENY


@dataclass(frozen=True, repr=False)
class CustomPiiRule:
    """Validated custom rule with a precompiled regex."""

    pii_type: str
    regex: str
    severity: str
    pattern: Any


@dataclass(frozen=True)
class CustomPiiRuleSet:
    """All-or-nothing result of loading the custom rules file."""

    status: CustomRuleStatus
    rules: tuple[CustomPiiRule, ...] = ()
    ruleset_sha256: str | None = None
    error_code: str | None = None


class _RuleLoadError(Exception):
    """Internal sanitized rule loading failure."""

    def __init__(self, code: str) -> None:
        super().__init__(code)
        self.code = code


def default_custom_rules_path() -> Path:
    """Return the fixed per-user custom PII rules path."""
    return Path.home() / ".config" / "agent-sec" / "pii-checker" / "rules.yaml"


def _regex_group_depth(pattern: str) -> int:
    """Return syntactic group depth, ignoring escapes and character classes."""
    depth = 0
    maximum = 0
    escaped = False
    in_character_class = False
    character_class_can_close = False

    for character in pattern:
        if escaped:
            escaped = False
            if in_character_class:
                character_class_can_close = True
            continue
        if character == "\\":
            escaped = True
            continue
        if in_character_class:
            if character == "]" and character_class_can_close:
                in_character_class = False
            elif character != "^" or character_class_can_close:
                character_class_can_close = True
            continue
        if character == "[":
            in_character_class = True
            character_class_can_close = False
        elif character == "(":
            depth += 1
            maximum = max(maximum, depth)
        elif character == ")" and depth > 0:
            depth -= 1

    return maximum


def _invalid_ruleset(
    *, digest: str | None, error_code: str, log_warning: bool = True
) -> CustomPiiRuleSet:
    """Build an invalid result without retaining parser or regex details."""
    if log_warning:
        _LOGGER.warning("PII custom rules disabled: %s", error_code)
    return CustomPiiRuleSet(
        status=CustomRuleStatus.INVALID,
        ruleset_sha256=digest,
        error_code=error_code,
    )


def _compile_rules(data: object) -> tuple[CustomPiiRule, ...]:
    """Validate and compile a parsed YAML rule list."""
    if not isinstance(data, list):
        raise _RuleLoadError("top_level_not_list")
    if len(data) > MAX_CUSTOM_RULES:
        raise _RuleLoadError("too_many_rules")

    rules: list[CustomPiiRule] = []
    seen_types: set[str] = set()
    for item in data:
        try:
            config = _CustomPiiRuleConfig.model_validate(item)
        except ValidationError as exc:
            raise _RuleLoadError("invalid_rule_schema") from exc

        if _TYPE_PATTERN.fullmatch(config.pii_type) is None:
            raise _RuleLoadError("invalid_rule_type")
        if config.pii_type in seen_types:
            raise _RuleLoadError("duplicate_rule_type")
        if config.pii_type in BUILTIN_PII_TYPES:
            raise _RuleLoadError("reserved_rule_type")
        if _regex_group_depth(config.regex) > MAX_REGEX_GROUP_DEPTH:
            raise _RuleLoadError("invalid_regex")

        try:
            pattern = regex_engine.compile(config.regex)
            empty_match = pattern.search("", timeout=RULE_MATCH_TIMEOUT_SECONDS)
        except _REGEX_VALIDATION_ERRORS as exc:
            raise _RuleLoadError("invalid_regex") from exc
        if empty_match is not None and empty_match.start() == empty_match.end():
            raise _RuleLoadError("regex_matches_empty_text")

        seen_types.add(config.pii_type)
        rules.append(
            CustomPiiRule(
                pii_type=config.pii_type,
                regex=config.regex,
                severity=config.severity.value,
                pattern=pattern,
            )
        )

    return tuple(rules)


def _load_content(content: bytes, digest: str) -> CustomPiiRuleSet:
    """Parse and compile one rules file content blob."""
    try:
        text = content.decode("utf-8")
        data = yaml.safe_load(text)
        rules = _compile_rules(data)
    except UnicodeDecodeError:
        return _invalid_ruleset(digest=digest, error_code="invalid_utf8")
    except yaml.YAMLError:
        return _invalid_ruleset(digest=digest, error_code="invalid_yaml")
    except _RuleLoadError as exc:
        return _invalid_ruleset(digest=digest, error_code=exc.code)

    return CustomPiiRuleSet(
        status=CustomRuleStatus.LOADED,
        rules=rules,
        ruleset_sha256=digest,
    )


def load_custom_rules(path: Path | None = None) -> CustomPiiRuleSet:
    """Load the fixed custom rules file, reusing compiled rules by content hash."""
    resolved_path = path if path is not None else default_custom_rules_path()
    try:
        with resolved_path.open("rb") as handle:
            content = handle.read(MAX_RULES_FILE_BYTES + 1)
    except FileNotFoundError:
        return CustomPiiRuleSet(status=CustomRuleStatus.ABSENT)
    except OSError:
        return _invalid_ruleset(digest=None, error_code="read_error")

    if len(content) > MAX_RULES_FILE_BYTES:
        return _invalid_ruleset(digest=None, error_code="file_too_large")

    digest = hashlib.sha256(content).hexdigest()
    cache_key = (resolved_path, digest)
    global _CACHE_KEY, _CACHE_VALUE
    with _CACHE_LOCK:
        if _CACHE_KEY == cache_key and _CACHE_VALUE is not None:
            return _CACHE_VALUE
        ruleset = _load_content(content, digest)
        _CACHE_KEY = cache_key
        _CACHE_VALUE = ruleset
        return ruleset
