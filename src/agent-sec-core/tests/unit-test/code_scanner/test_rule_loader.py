import pytest
from agent_sec_cli.code_scanner.models import Language, Severity
from agent_sec_cli.code_scanner.errors import ErrRuleRefResolve, ErrRuleValidation
from agent_sec_cli.code_scanner.rules import rule_loader
from agent_sec_cli.code_scanner.rules.rule_loader import load_rules


def test_load_bash_rules() -> None:
    """Should load at least one rule for bash."""
    rules = load_rules(Language.BASH)
    assert len(rules) > 0


def test_bash_rule_fields() -> None:
    """Each loaded rule must have all required fields populated."""
    rules = load_rules(Language.BASH)
    for rule in rules:
        assert rule.rule_id
        assert rule.cwe_id
        assert rule.desc_en
        assert rule.desc_zh
        assert rule.regex
        assert isinstance(rule.severity, Severity)


def test_load_python_rules() -> None:
    """Should load Python rules."""
    rules = load_rules(Language.PYTHON)
    assert len(rules) > 0


def test_python_rule_fields() -> None:
    """Each loaded Python rule must have all required fields populated."""
    rules = load_rules(Language.PYTHON)
    for rule in rules:
        assert rule.rule_id
        assert rule.cwe_id
        assert rule.desc_en
        assert rule.desc_zh
        assert rule.regex
        assert isinstance(rule.severity, Severity)


def test_shell_recursive_delete_rule_exists() -> None:
    """The example shell-recursive-delete rule should be loadable."""
    rules = load_rules(Language.BASH)
    rule_ids = {r.rule_id for r in rules}
    assert "shell-recursive-delete" in rule_ids


@pytest.mark.parametrize(
    "content",
    [
        "- not: a rule\n",
        "rule_id: missing-regex\nseverity: warn\n",
    ],
)
def test_malformed_rule_shapes_raise_validation_error(tmp_path, monkeypatch, content) -> None:
    language_dir = tmp_path / Language.BASH.value
    language_dir.mkdir()
    (language_dir / "broken.yaml").write_text(content, encoding="utf-8")
    monkeypatch.setattr(rule_loader, "_RULES_DIR", tmp_path)

    with pytest.raises(ErrRuleValidation, match="broken"):
        load_rules(Language.BASH)


def test_non_string_rule_reference_raises_reference_error(tmp_path, monkeypatch) -> None:
    language_dir = tmp_path / Language.BASH.value
    language_dir.mkdir()
    (language_dir / "_shared.yaml").write_text("shared: ['x']\n", encoding="utf-8")
    (language_dir / "broken.yaml").write_text(
        "rule_id: broken\n"
        "cwe_id: CWE-1\n"
        "desc_en: broken\n"
        "desc_zh: broken\n"
        "regex: x\n"
        "severity: warn\n"
        "target_regexes_ref: [shared]\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(rule_loader, "_RULES_DIR", tmp_path)

    with pytest.raises(ErrRuleRefResolve, match="broken"):
        load_rules(Language.BASH)
