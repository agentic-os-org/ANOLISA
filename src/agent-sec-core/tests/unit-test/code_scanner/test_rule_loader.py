import pytest

from agent_sec_cli.code_scanner.errors import ErrRuleValidation
from agent_sec_cli.code_scanner.models import Language, Severity
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


class TestLoadRulesMalformedYaml:
    """Malformed YAML rule files must raise ErrRuleValidation, not unhandled exceptions."""

    def test_missing_regex_key(self, tmp_path, monkeypatch) -> None:
        lang_dir = tmp_path / "bash"
        lang_dir.mkdir()
        (lang_dir / "bad.yaml").write_text(
            "rule_id: test\ncwe_id: CWE-1\ndesc_en: d\ndesc_zh: d\nseverity: warn\n",
            encoding="utf-8",
        )
        monkeypatch.setattr(rule_loader, "_RULES_DIR", tmp_path)
        with pytest.raises(ErrRuleValidation):
            load_rules(Language.BASH)

    def test_non_dict_yaml(self, tmp_path, monkeypatch) -> None:
        lang_dir = tmp_path / "bash"
        lang_dir.mkdir()
        (lang_dir / "bad.yaml").write_text("- 1\n- 2\n", encoding="utf-8")
        monkeypatch.setattr(rule_loader, "_RULES_DIR", tmp_path)
        with pytest.raises(ErrRuleValidation):
            load_rules(Language.BASH)

    def test_regex_not_string(self, tmp_path, monkeypatch) -> None:
        lang_dir = tmp_path / "bash"
        lang_dir.mkdir()
        (lang_dir / "bad.yaml").write_text(
            "rule_id: test\ncwe_id: CWE-1\ndesc_en: d\ndesc_zh: d\nregex: 123\n"
            "severity: warn\n",
            encoding="utf-8",
        )
        monkeypatch.setattr(rule_loader, "_RULES_DIR", tmp_path)
        with pytest.raises(ErrRuleValidation):
            load_rules(Language.BASH)
