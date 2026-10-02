"""Unit tests for custom PII rule loading."""

from pathlib import Path

import pytest
from agent_sec_cli.pii_checker.custom_rules import (
    MAX_CUSTOM_RULES,
    MAX_REGEX_GROUP_DEPTH,
    MAX_REGEX_LENGTH,
    MAX_RULES_FILE_BYTES,
    CustomRuleStatus,
    default_custom_rules_path,
    load_custom_rules,
)


def _write_rules(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def test_default_path_uses_fixed_home_location(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "xdg"))
    monkeypatch.setenv("AGENT_SEC_PII_RULES_FILE", str(tmp_path / "override.yaml"))

    assert default_custom_rules_path() == (
        tmp_path / ".config" / "agent-sec" / "pii-checker" / "rules.yaml"
    )


def test_absent_and_empty_rule_files(tmp_path: Path) -> None:
    path = tmp_path / "rules.yaml"

    absent = load_custom_rules(path)
    _write_rules(path, "[]\n")
    loaded = load_custom_rules(path)

    assert absent.status is CustomRuleStatus.ABSENT
    assert loaded.status is CustomRuleStatus.LOADED
    assert loaded.rules == ()
    assert loaded.ruleset_sha256 is not None


def test_valid_rules_are_compiled_and_cached(tmp_path: Path) -> None:
    path = tmp_path / "rules.yaml"
    _write_rules(
        path,
        """
- type: dogfood_order_no
  regex: 'DFT-[A-Z0-9]{8}'
  severity: warn
""".lstrip(),
    )

    first = load_custom_rules(path)
    second = load_custom_rules(path)

    assert first.status is CustomRuleStatus.LOADED
    assert first is second
    assert len(first.rules) == 1
    assert first.rules[0].pii_type == "dogfood_order_no"
    assert first.rules[0].severity == "warn"
    assert first.rules[0].pattern.fullmatch("DFT-ABC12345") is not None


def test_regex_group_depth_boundary_is_enforced(tmp_path: Path) -> None:
    allowed = tmp_path / "allowed.yaml"
    rejected = tmp_path / "rejected.yaml"
    allowed_regex = "(" * MAX_REGEX_GROUP_DEPTH + "token" + ")" * MAX_REGEX_GROUP_DEPTH
    rejected_regex = (
        "(" * (MAX_REGEX_GROUP_DEPTH + 1) + "token" + ")" * (MAX_REGEX_GROUP_DEPTH + 1)
    )
    _write_rules(allowed, f"- type: allowed_token\n  regex: '{allowed_regex}'\n")
    _write_rules(rejected, f"- type: rejected_token\n  regex: '{rejected_regex}'\n")

    allowed_ruleset = load_custom_rules(allowed)
    rejected_ruleset = load_custom_rules(rejected)

    assert allowed_ruleset.status is CustomRuleStatus.LOADED
    assert allowed_ruleset.rules[0].pattern.fullmatch("token") is not None
    assert rejected_ruleset.status is CustomRuleStatus.INVALID
    assert rejected_ruleset.error_code == "invalid_regex"


def test_escaped_and_character_class_parentheses_do_not_count_as_groups(
    tmp_path: Path,
) -> None:
    path = tmp_path / "rules.yaml"
    regex = (r"\(\)" * (MAX_REGEX_GROUP_DEPTH + 1)) + (
        r"[()]" * (MAX_REGEX_GROUP_DEPTH + 1)
    )
    _write_rules(path, f"- type: literal_parentheses\n  regex: '{regex}'\n")

    ruleset = load_custom_rules(path)

    assert ruleset.status is CustomRuleStatus.LOADED


@pytest.mark.parametrize(
    "compile_error",
    [
        RecursionError("nested groups"),
        OverflowError("repeat overflow"),
        ValueError("invalid value"),
        KeyError("invalid flag combination"),
        MemoryError("pattern allocation"),
    ],
)
def test_compile_failures_are_invalid_and_cached(
    monkeypatch, tmp_path: Path, compile_error: Exception
) -> None:
    path = tmp_path / "rules.yaml"
    _write_rules(path, "- type: dogfood_token\n  regex: token\n")
    compile_calls = 0

    def fail_compile(pattern: str):
        nonlocal compile_calls
        compile_calls += 1
        raise compile_error

    monkeypatch.setattr(
        "agent_sec_cli.pii_checker.custom_rules.regex_engine.compile", fail_compile
    )

    first = load_custom_rules(path)
    second = load_custom_rules(path)

    assert first.status is CustomRuleStatus.INVALID
    assert first.error_code == "invalid_regex"
    assert second is first
    assert compile_calls == 1


def test_regex_extensions_remain_supported(tmp_path: Path) -> None:
    path = tmp_path / "rules.yaml"
    _write_rules(
        path,
        """
- type: fuzzy_token
  regex: '(?:target){e<=1}'
- type: dotall_token
  regex: '(?V0)(?s)token.+'
""".lstrip(),
    )

    ruleset = load_custom_rules(path)

    assert ruleset.status is CustomRuleStatus.LOADED
    assert ruleset.rules[0].pattern.fullmatch("targot") is not None
    assert ruleset.rules[1].pattern.fullmatch("token\nvalue") is not None


@pytest.mark.parametrize(
    ("content", "error_code"),
    [
        ("type: dogfood_token\nregex: token\n", "top_level_not_list"),
        ("- type: dogfood_token\n", "invalid_rule_schema"),
        (
            "- type: dogfood_token\n  regex: token\n  enabled: true\n",
            "invalid_rule_schema",
        ),
        ("- type: DogfoodToken\n  regex: token\n", "invalid_rule_type"),
        (
            "- type: dogfood_token\n  regex: token\n  severity: block\n",
            "invalid_rule_schema",
        ),
        ("- type: dogfood_token\n  regex: '[invalid'\n", "invalid_regex"),
        ("- type: dogfood_token\n  regex: '.*'\n", "regex_matches_empty_text"),
        ("- type: email\n  regex: example\\.com\n", "reserved_rule_type"),
        (
            "- type: dogfood_token\n  regex: token\n"
            "- type: dogfood_token\n  regex: secret\n",
            "duplicate_rule_type",
        ),
    ],
)
def test_invalid_schema_disables_entire_ruleset(
    tmp_path: Path, content: str, error_code: str
) -> None:
    path = tmp_path / "rules.yaml"
    _write_rules(path, content)

    ruleset = load_custom_rules(path)

    assert ruleset.status is CustomRuleStatus.INVALID
    assert ruleset.rules == ()
    assert ruleset.error_code == error_code


def test_invalid_yaml_and_unsafe_tag_are_rejected(tmp_path: Path) -> None:
    invalid_yaml = tmp_path / "invalid.yaml"
    unsafe_yaml = tmp_path / "unsafe.yaml"
    _write_rules(invalid_yaml, "- type: dogfood_token\n  regex: '[unterminated\n")
    _write_rules(unsafe_yaml, "!!python/object/apply:os.system ['echo unsafe']\n")

    invalid = load_custom_rules(invalid_yaml)
    unsafe = load_custom_rules(unsafe_yaml)

    assert invalid.status is CustomRuleStatus.INVALID
    assert invalid.error_code == "invalid_yaml"
    assert unsafe.status is CustomRuleStatus.INVALID
    assert unsafe.error_code == "invalid_yaml"


def test_file_and_collection_limits_are_enforced(tmp_path: Path) -> None:
    oversized = tmp_path / "oversized.yaml"
    too_many = tmp_path / "too-many.yaml"
    long_regex = tmp_path / "long-regex.yaml"
    oversized.write_bytes(b"x" * (MAX_RULES_FILE_BYTES + 1))
    _write_rules(
        too_many,
        "".join(
            f"- type: custom_{index}\n  regex: value_{index}\n"
            for index in range(MAX_CUSTOM_RULES + 1)
        ),
    )
    _write_rules(
        long_regex,
        f"- type: dogfood_token\n  regex: '{'x' * (MAX_REGEX_LENGTH + 1)}'\n",
    )

    assert load_custom_rules(oversized).error_code == "file_too_large"
    assert load_custom_rules(too_many).error_code == "too_many_rules"
    assert load_custom_rules(long_regex).error_code == "invalid_rule_schema"


def test_invalid_update_does_not_reuse_last_valid_rules(tmp_path: Path) -> None:
    path = tmp_path / "rules.yaml"
    valid_content = "- type: dogfood_token\n  regex: DFT-[A-Z0-9]{8}\n"
    _write_rules(path, valid_content)
    valid = load_custom_rules(path)

    _write_rules(path, "- type: dogfood_token\n  regex: '[invalid'\n")
    invalid = load_custom_rules(path)

    _write_rules(path, valid_content)
    repaired = load_custom_rules(path)

    path.unlink()
    absent = load_custom_rules(path)

    assert valid.status is CustomRuleStatus.LOADED
    assert invalid.status is CustomRuleStatus.INVALID
    assert invalid.rules == ()
    assert repaired.status is CustomRuleStatus.LOADED
    assert absent.status is CustomRuleStatus.ABSENT
