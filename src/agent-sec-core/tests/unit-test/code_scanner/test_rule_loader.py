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


def _load_from_dir(monkeypatch, tmp_path, files: dict) -> list:
    """Point rule_loader at a temp rules dir containing *files* and load bash rules."""
    lang_dir = tmp_path / "bash"
    lang_dir.mkdir()
    for name, content in files.items():
        (lang_dir / name).write_text(content, encoding="utf-8")
    monkeypatch.setattr(rule_loader, "_RULES_DIR", tmp_path)
    return rule_loader.load_rules(Language.BASH)


def test_rule_without_regex_raises_typed_error(monkeypatch, tmp_path) -> None:
    """A rule mapping without ``regex`` must fail as ErrRuleValidation, not KeyError."""
    with pytest.raises(ErrRuleValidation):
        _load_from_dir(
            monkeypatch, tmp_path, {"no-regex.yaml": "rule_id: x\ncwe_id: CWE-1\n"}
        )


def test_non_mapping_rule_file_raises_typed_error(monkeypatch, tmp_path) -> None:
    """A top-level list rule file must fail as ErrRuleValidation, not TypeError."""
    with pytest.raises(ErrRuleValidation):
        _load_from_dir(monkeypatch, tmp_path, {"list-root.yaml": "- a\n- b\n"})


def test_null_regex_rule_raises_typed_error(monkeypatch, tmp_path) -> None:
    """``regex: ~`` must fail as ErrRuleValidation, not AttributeError."""
    with pytest.raises(ErrRuleValidation):
        _load_from_dir(
            monkeypatch, tmp_path, {"null-regex.yaml": "rule_id: x\nregex: ~\n"}
        )


def test_empty_rule_file_is_skipped(monkeypatch, tmp_path) -> None:
    """An empty rule file still yields no rules and no error."""
    assert _load_from_dir(monkeypatch, tmp_path, {"empty.yaml": ""}) == []
