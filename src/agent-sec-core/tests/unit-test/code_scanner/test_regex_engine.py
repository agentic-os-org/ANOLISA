"""Direct unit tests for regex_engine internals.

These tests target _normalize_python_parens and _match_with_targets
directly, complementing the parametrized conftest.py-based tests that
exercise them indirectly through the scanner.
"""

import pytest
from agent_sec_cli.code_scanner.engine.regex_engine import (
    _match_with_targets,
    _normalize_python_parens,
    run_regex_rules,
)
from agent_sec_cli.code_scanner.errors import ErrRegexCompile
from agent_sec_cli.code_scanner.models import (
    Finding,
    Language,
    RuleDefinition,
    Severity,
)

# ---------------------------------------------------------------------------
# _normalize_python_parens
# ---------------------------------------------------------------------------


class TestNormalizePythonParens:
    """Tests for collapsing newlines inside parentheses."""

    def test_single_level_paren(self) -> None:
        """Newlines inside single-level parens become spaces."""
        assert (
            _normalize_python_parens("open(\n'/etc/shadow'\n)")
            == "open( '/etc/shadow' )"
        )

    def test_nested_parens(self) -> None:
        """Nested parens: all newlines inside any paren are collapsed."""
        result = _normalize_python_parens("func(a, (b,\nc))")
        assert result == "func(a, (b, c))"

    def test_deep_nesting(self) -> None:
        """3-level deep nesting should still work."""
        code = "f(\n  g(\n    h(\n      x\n    )\n  )\n)"
        result = _normalize_python_parens(code)
        assert "\n" not in result
        assert "f(" in result
        assert "g(" in result

    def test_newline_outside_parens_preserved(self) -> None:
        """Newlines outside parentheses are NOT collapsed."""
        code = "line1\nline2\nline3"
        assert _normalize_python_parens(code) == code

    def test_mixed_inside_outside(self) -> None:
        """Only newlines inside parens are collapsed; outside preserved."""
        code = "a = 1\nb = func(\n  x\n)\nc = 2"
        result = _normalize_python_parens(code)
        assert "func(   x )" in result
        assert result.startswith("a = 1\nb = func")
        assert result.endswith(")\nc = 2")

    def test_unmatched_close_paren_no_crash(self) -> None:
        """Extra ) should not crash or go negative depth."""
        result = _normalize_python_parens("a)\nb")
        # depth clamps to 0, so the newline outside is preserved
        assert result == "a)\nb"

    def test_empty_string(self) -> None:
        assert _normalize_python_parens("") == ""

    def test_no_parens(self) -> None:
        assert _normalize_python_parens("x = 1") == "x = 1"


# ---------------------------------------------------------------------------
# _match_with_targets
# ---------------------------------------------------------------------------


def _make_rule(
    regex: str, target_regexes: list[str], rule_id: str = "test-rule"
) -> RuleDefinition:
    """Helper to build a RuleDefinition with target_regexes."""
    return RuleDefinition(
        rule_id=rule_id,
        cwe_id="CWE-000",
        desc_en="test",
        desc_zh="测试",
        regex=regex,
        severity=Severity.WARN,
        target_regexes=target_regexes,
    )


class TestMatchWithTargets:
    """Tests for segment-level matching with target_regexes."""

    def test_target_after_main_matches(self) -> None:
        """When target appears after main regex in the same segment, it matches."""
        rule = _make_rule(r"\bcat\b", [r"/etc/shadow"])
        evidence = _match_with_targets("cat /etc/shadow", rule, Language.BASH)
        assert len(evidence) == 1
        assert "/etc/shadow" in evidence[0]

    def test_target_before_main_no_match(self) -> None:
        """When target appears before main regex, it should NOT match."""
        rule = _make_rule(r"\bcat\b", [r"/etc/shadow"])
        evidence = _match_with_targets("/etc/shadow cat", rule, Language.BASH)
        # target.start() must be > main.start(); here /etc/shadow is at 0, cat at 12
        # so target (0) is NOT > main (12) — no match
        assert len(evidence) == 0

    def test_multi_segment_only_matching_included(self) -> None:
        """Only segments where both main + target match are in evidence."""
        rule = _make_rule(r"\bcat\b", [r"/etc/shadow"])
        code = "cat /etc/shadow; echo hello; cat /var/log/syslog"
        evidence = _match_with_targets(code, rule, Language.BASH)
        assert len(evidence) == 1
        assert "/etc/shadow" in evidence[0]

    def test_empty_code(self) -> None:
        """Empty code should return empty evidence."""
        rule = _make_rule(r"\bcat\b", [r"/etc/shadow"])
        evidence = _match_with_targets("", rule, Language.BASH)
        assert evidence == []

    def test_python_paren_normalization(self) -> None:
        """For Python, newlines inside parens should be collapsed before matching."""
        rule = _make_rule(r"\bopen\b", [r"/etc/shadow"], rule_id="py-test")
        code = "open(\n    '/etc/shadow'\n)"
        evidence = _match_with_targets(code, rule, Language.PYTHON)
        assert len(evidence) == 1

    def test_no_main_match(self) -> None:
        """No match when main regex doesn't match."""
        rule = _make_rule(r"\bcat\b", [r"/etc/shadow"])
        evidence = _match_with_targets("echo /etc/shadow", rule, Language.BASH)
        assert evidence == []

    def test_no_target_match(self) -> None:
        """No match when main matches but no target matches."""
        rule = _make_rule(r"\bcat\b", [r"/etc/shadow"])
        evidence = _match_with_targets("cat /tmp/file.txt", rule, Language.BASH)
        assert evidence == []


# ---------------------------------------------------------------------------
# run_regex_rules — ErrRegexCompile path
# ---------------------------------------------------------------------------


class TestRunRegexRulesErrors:
    """Tests for error paths in run_regex_rules."""

    def test_invalid_regex_in_target_raises(self) -> None:
        """Invalid regex in target_regexes should raise ErrRegexCompile."""
        rule = _make_rule(r"\bcat\b", [r"[invalid"])
        with pytest.raises(ErrRegexCompile):
            _match_with_targets("cat /etc/shadow", rule, Language.BASH)

    def test_invalid_main_regex_raises(self) -> None:
        """Invalid main regex should raise ErrRegexCompile."""
        rule = _make_rule(r"[invalid", [r"/etc/shadow"])
        with pytest.raises(ErrRegexCompile):
            _match_with_targets("cat /etc/shadow", rule, Language.BASH)

    def test_invalid_regex_no_targets_raises(self) -> None:
        """Invalid regex in a rule without target_regexes should raise ErrRegexCompile."""
        rule = RuleDefinition(
            rule_id="bad-regex",
            cwe_id="CWE-000",
            desc_en="test",
            desc_zh="测试",
            regex=r"[invalid",
            severity=Severity.WARN,
        )
        with pytest.raises(ErrRegexCompile):
            run_regex_rules("some code", [rule], Language.BASH)
