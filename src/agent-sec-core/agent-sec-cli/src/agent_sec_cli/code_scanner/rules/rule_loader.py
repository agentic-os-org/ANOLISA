import pathlib
from typing import Any, Dict, List

import yaml
from agent_sec_cli.code_scanner.errors import (
    ErrRuleRefResolve,
    ErrRuleValidation,
    ErrRuleYamlParse,
)
from agent_sec_cli.code_scanner.models import Language, RuleDefinition
from pydantic import ValidationError

_RULES_DIR = pathlib.Path(__file__).parent


def _load_shared_defs(lang_dir: pathlib.Path) -> Dict[str, Any]:
    """Load shared definitions from ``_shared.yaml`` in *lang_dir*.

    Files starting with ``_`` are treated as shared data, not rules.
    Returns a mapping of definition-name to its value.
    """
    shared_file = lang_dir / "_shared.yaml"
    if not shared_file.is_file():
        return {}
    with open(shared_file, "r", encoding="utf-8") as fh:
        data = yaml.safe_load(fh)
    return data if isinstance(data, dict) else {}


def _resolve_refs(data: Dict[str, Any], shared: Dict[str, Any], rule_name: str) -> None:
    """Resolve ``target_regexes_ref`` in *data* using *shared* definitions.

    If *data* contains a ``target_regexes_ref`` key, look it up in *shared*,
    copy the list into ``target_regexes``, and remove the ref key.
    """
    ref_key = data.pop("target_regexes_ref", None)
    if ref_key is None:
        return
    if ref_key not in shared:
        raise ErrRuleRefResolve(rule_name)
    data["target_regexes"] = shared[ref_key]


def load_rules(language: Language) -> List[RuleDefinition]:
    """Load all YAML rule files for the given language.

    Scans ``rules/{language.value}/`` for ``*.yaml`` files and parses each
    into a :class:`RuleDefinition`.  Files starting with ``_`` are treated as
    shared definitions and skipped.  Returns an empty list when the language
    directory does not exist or contains no rule files.
    """
    lang_dir = _RULES_DIR / language.value
    if not lang_dir.is_dir():
        return []

    shared = _load_shared_defs(lang_dir)

    rules: List[RuleDefinition] = []
    for yaml_file in sorted(lang_dir.glob("*.yaml")):
        if yaml_file.name.startswith("_"):
            continue
        with open(yaml_file, "r", encoding="utf-8") as fh:
            try:
                data = yaml.safe_load(fh)
            except yaml.YAMLError:
                raise ErrRuleYamlParse(yaml_file.stem)
        if data is None:
            continue
        data["regex"] = data["regex"].replace("\n", "")
        _resolve_refs(data, shared, yaml_file.stem)
        try:
            rules.append(RuleDefinition(**data))
        except ValidationError:
            raise ErrRuleValidation(yaml_file.stem)
    return rules
