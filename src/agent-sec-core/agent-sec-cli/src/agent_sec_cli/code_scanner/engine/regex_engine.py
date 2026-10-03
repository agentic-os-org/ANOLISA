import re
from typing import List

from agent_sec_cli.code_scanner.errors import ErrRegexCompile
from agent_sec_cli.code_scanner.models import Finding, Language, RuleDefinition

_SEGMENT_SPLIT = re.compile(r"[;\n|]|&&")


def _normalize_python_parens(code: str) -> str:
    """Collapse newlines inside parentheses into spaces.

    Python allows implicit line continuation inside ``()``.  Multi-line
    calls such as ``open(\n    '/etc/shadow'\n)`` must be treated as a
    single logical line so that segment-level matching can see both the
    function call and its arguments together.
    """
    result, depth = [], 0
    for ch in code:
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth = max(depth - 1, 0)
        if ch == "\n" and depth > 0:
            result.append(" ")
        else:
            result.append(ch)
    return "".join(result)


def _match_with_targets(
    code: str, rule: RuleDefinition, language: Language
) -> list[str]:
    """Segment-level matching for rules with *target_regexes*.

    Splits *code* by command separators (``;``, ``\n``, ``|``, ``&&``),
    then checks each segment for **both** the main regex and at least one
    target regex.  Returns a list of matched segments (empty = no match).

    For Python code, parenthesised newlines are first collapsed so that
    multi-line function calls stay within a single segment.
    """
    if language == Language.PYTHON:
        code = _normalize_python_parens(code)
    segments = _SEGMENT_SPLIT.split(code)
    try:
        main_pat = re.compile(rule.regex)
        target_pats = [re.compile(t) for t in rule.target_regexes]  # type: ignore[union-attr]
    except re.error:
        raise ErrRegexCompile(rule.rule_id)
    evidence: list[str] = []
    for seg in segments:
        m = main_pat.search(seg)
        if m and any(
            t.start() > m.start() for tp in target_pats if (t := tp.search(seg))
        ):
            evidence.append(seg.strip())
    return evidence


def run_regex_rules(
    code: str, rules: List[RuleDefinition], language: Language
) -> List[Finding]:
    """Run all regex rules against *code* and return findings.

    Each rule that matches produces exactly one :class:`Finding` whose
    ``evidence`` list contains every matched substring.
    """
    findings: List[Finding] = []
    for rule in rules:
        if rule.target_regexes:
            evidence = _match_with_targets(code, rule, language)
            if not evidence:
                continue
            findings.append(
                Finding(
                    rule_id=rule.rule_id,
                    severity=rule.severity,
                    desc_zh=rule.desc_zh,
                    desc_en=rule.desc_en,
                    evidence=evidence,
                )
            )
        else:
            try:
                pattern = re.compile(rule.regex)
            except re.error:
                raise ErrRegexCompile(rule.rule_id)
            matches = list(pattern.finditer(code))
            if not matches:
                continue
            findings.append(
                Finding(
                    rule_id=rule.rule_id,
                    severity=rule.severity,
                    desc_zh=rule.desc_zh,
                    desc_en=rule.desc_en,
                    evidence=[m.group() for m in matches],
                )
            )
    return findings
