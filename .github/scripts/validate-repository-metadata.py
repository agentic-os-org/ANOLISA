#!/usr/bin/env python3
"""Validate repository governance metadata using only the Python standard library."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
GITHUB = ROOT / ".github"

errors: list[str] = []
warnings: list[str] = []

VALID_STATUSES = {"active", "deprecated", "incubating", "internal"}
COMPONENT_PATH_PATTERN = re.compile(
    r"(?:(?:src|distribution|deprecated)/[a-z0-9][a-z0-9-]*|benchmark)/"
)
NON_COMPONENT_SCOPES = {"chore", "ci", "deps", "docs"}
ISSUE_FORMS = ("bug_report.yml", "feature_request.yml", "question.yml")
ISSUE_FORM_FALLBACK_OPTIONS = {"project-wide", "not sure", "other"}


def load_json(path: Path) -> dict[str, Any]:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        errors.append(f"missing required file: {path.relative_to(ROOT)}")
    except json.JSONDecodeError as exc:
        errors.append(f"invalid JSON in {path.relative_to(ROOT)}: {exc}")
    return {}


def require(condition: bool, message: str) -> None:
    if not condition:
        errors.append(message)


def duplicates(values: list[str]) -> set[str]:
    seen: set[str] = set()
    dupes: set[str] = set()
    for value in values:
        if value in seen:
            dupes.add(value)
        seen.add(value)
    return dupes


def issue_form_component_options(path: Path) -> list[str]:
    """Return the component dropdown values without depending on fixed indentation."""
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except FileNotFoundError:
        errors.append(f"missing issue form: {path.relative_to(ROOT)}")
        return []

    component_line = next(
        (index for index, line in enumerate(lines) if re.fullmatch(r"\s*id:\s*component\s*", line)),
        None,
    )
    if component_line is None:
        errors.append(f"{path.name}: missing component field")
        return []

    options_line = next(
        (
            index
            for index in range(component_line + 1, len(lines))
            if re.fullmatch(r"\s*options:\s*", lines[index])
        ),
        None,
    )
    if options_line is None:
        errors.append(f"{path.name}: component field is missing options")
        return []

    options_indent = len(lines[options_line]) - len(lines[options_line].lstrip())
    options: list[str] = []
    for line in lines[options_line + 1 :]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        indent = len(line) - len(line.lstrip())
        if indent <= options_indent:
            break
        match = re.fullmatch(r"\s*-\s*(.+?)\s*", line)
        if not match:
            continue
        value = re.sub(r"\s+#.*$", "", match.group(1)).strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        options.append(value)
    if not options:
        errors.append(f"{path.name}: component field has no options")
    return options


components_doc = load_json(GITHUB / "components.json")
labels_doc = load_json(GITHUB / "labels.json")
maintainers_doc = load_json(GITHUB / "maintainers.json")
commitlint_doc = load_json(GITHUB / "commitlint.config.json")

components = components_doc.get("components", [])
component_defaults = components_doc.get("defaults", {})
require(components_doc.get("schema_version") == 1, ".github/components.json schema_version must be 1")
require(isinstance(components, list) and components, ".github/components.json must define components")
require(isinstance(component_defaults, dict), ".github/components.json defaults must be an object")

default_triagers = (
    component_defaults.get("issue_triagers", []) if isinstance(component_defaults, dict) else []
)
require(isinstance(default_triagers, list), "components defaults issue_triagers must be a list")
if isinstance(default_triagers, list):
    require(
        all(isinstance(value, str) and value for value in default_triagers),
        "components defaults issue_triagers entries must be non-empty strings",
    )
    for duplicate in sorted(duplicates(default_triagers)):
        errors.append(f"duplicate components default issue_triagers entry: {duplicate}")
else:
    default_triagers = []

required_keys = {
    "id",
    "display_name",
    "aliases",
    "issue_option",
    "path_prefixes",
    "label",
    "commit_scope",
    "commit_scope_aliases",
    "release_tag_prefix",
    "ci_key",
    "issue_triagers",
    "status",
}

for index, component in enumerate(components):
    if not isinstance(component, dict):
        errors.append(f"components[{index}] must be an object")
        continue
    missing = sorted(required_keys - component.keys())
    if missing:
        errors.append(f"component {component.get('id', index)!r} is missing keys: {', '.join(missing)}")
        continue

    component_id = component["id"]
    require(bool(re.fullmatch(r"[a-z0-9][a-z0-9-]*", component_id)), f"invalid component id: {component_id}")
    require(component["label"] == f"component:{component_id}", f"{component_id}: label must be component:{component_id}")
    require(isinstance(component["aliases"], list), f"{component_id}: aliases must be a list")
    aliases = component["aliases"] if isinstance(component["aliases"], list) else []
    require(
        all(
            isinstance(value, str) and re.fullmatch(r"[a-z0-9][a-z0-9-]*", value)
            for value in aliases
        ),
        f"{component_id}: aliases must be non-empty lowercase identifiers",
    )
    require(isinstance(component["path_prefixes"], list) and component["path_prefixes"], f"{component_id}: path_prefixes must not be empty")
    require(isinstance(component["commit_scope_aliases"], list), f"{component_id}: commit_scope_aliases must be a list")
    require(isinstance(component["issue_triagers"], list), f"{component_id}: issue_triagers must be a list")
    require(
        component["status"] in VALID_STATUSES,
        f"{component_id}: unknown status {component['status']!r}",
    )

    triagers = component["issue_triagers"] if isinstance(component["issue_triagers"], list) else []
    require(
        all(isinstance(value, str) and value for value in triagers),
        f"{component_id}: issue_triagers entries must be non-empty strings",
    )
    for duplicate in sorted(duplicates(triagers)):
        errors.append(f"{component_id}: duplicate issue_triager {duplicate!r}")

    for prefix in component["path_prefixes"]:
        require(
            isinstance(prefix, str) and bool(COMPONENT_PATH_PATTERN.fullmatch(prefix)),
            f"{component_id}: invalid path prefix {prefix!r}",
        )

    if component["status"] in {"active", "deprecated"}:
        require(bool(component["issue_option"]), f"{component_id}: {component['status']} components need an issue_option")
        require(bool(component["commit_scope"]), f"{component_id}: {component['status']} components need a commit_scope")
        require(bool(component["release_tag_prefix"]), f"{component_id}: {component['status']} components need a release_tag_prefix")
    if component["status"] != "internal":
        require(
            bool(triagers or default_triagers),
            f"{component_id}: public components need an effective issue triager",
        )

component_identifiers = [
    identifier
    for component in components
    if isinstance(component, dict)
    for identifier in [
        component.get("id", ""),
        *(component.get("aliases", []) if isinstance(component.get("aliases"), list) else []),
    ]
]
labels = [c.get("label", "") for c in components if isinstance(c, dict)]
paths = [p for c in components if isinstance(c, dict) for p in c.get("path_prefixes", [])]
issue_options = [
    c.get("issue_option")
    for c in components
    if isinstance(c, dict) and c.get("issue_option")
]
commit_scopes = [
    scope
    for component in components
    if isinstance(component, dict)
    for scope in [component.get("commit_scope"), *component.get("commit_scope_aliases", [])]
    if scope
]
release_prefixes = [
    c.get("release_tag_prefix")
    for c in components
    if isinstance(c, dict) and c.get("release_tag_prefix")
]
ci_keys = [
    c.get("ci_key") for c in components if isinstance(c, dict) and c.get("ci_key")
]
for field, values in (
    ("component identifier", component_identifiers),
    ("component label", labels),
    ("path prefix", paths),
    ("issue option", issue_options),
    ("commit scope", commit_scopes),
    ("release tag prefix", release_prefixes),
    ("CI key", ci_keys),
):
    for duplicate in sorted(duplicates(values)):
        errors.append(f"duplicate {field}: {duplicate}")

scope_rule = commitlint_doc.get("rules", {}).get("scope-enum", [])
commitlint_scopes = set()
if isinstance(scope_rule, list) and len(scope_rule) >= 3 and isinstance(scope_rule[2], list):
    commitlint_scopes = set(scope_rule[2])
else:
    errors.append("unable to read scope-enum from .github/commitlint.config.json")

for component in components:
    scope = component.get("commit_scope")
    if scope and scope not in commitlint_scopes:
        errors.append(f"{component['id']}: commit scope {scope!r} is missing from commitlint.config.json")

component_scopes = set(commit_scopes)
for scope in sorted(component_scopes & NON_COMPONENT_SCOPES):
    errors.append(f"component commit scope conflicts with reserved scope: {scope!r}")
for scope in sorted(commitlint_scopes - component_scopes - NON_COMPONENT_SCOPES):
    errors.append(f"commitlint.config.json: unknown component scope {scope!r}")

expected_form_options = {
    component.get("issue_option") for component in components if component.get("issue_option")
}
for option in sorted(expected_form_options & ISSUE_FORM_FALLBACK_OPTIONS):
    errors.append(f"component issue option conflicts with fallback option: {option!r}")
for form_name in ISSUE_FORMS:
    form_path = GITHUB / "ISSUE_TEMPLATE" / form_name
    form_options = issue_form_component_options(form_path)
    for duplicate in sorted(duplicates(form_options)):
        errors.append(f"{form_name}: duplicate component option {duplicate!r}")
    for option in sorted(expected_form_options - set(form_options)):
        errors.append(f"{form_name}: missing component option {option!r}")
    unknown_options = set(form_options) - expected_form_options - ISSUE_FORM_FALLBACK_OPTIONS
    for option in sorted(unknown_options):
        errors.append(f"{form_name}: unknown component option {option!r}")

codeowners_path = GITHUB / "CODEOWNERS"
try:
    codeowners_text = codeowners_path.read_text(encoding="utf-8")
except FileNotFoundError:
    errors.append("missing .github/CODEOWNERS")
    codeowners_text = ""

codeowner_mappings: dict[str, set[str]] = {}
for line in codeowners_text.splitlines():
    annotation = re.search(r"#\s*auto-label:\s*(component:\S+)", line)
    if not annotation or line.lstrip().startswith("#"):
        continue
    pattern = line.split(maxsplit=1)[0]
    codeowner_mappings.setdefault(annotation.group(1), set()).add(pattern)

component_labels = set(labels)
for label in sorted(set(codeowner_mappings) - component_labels):
    errors.append(f"CODEOWNERS: unknown component annotation {label}")
components_by_label = {
    component.get("label"): component for component in components if isinstance(component, dict)
}
for label, mapped_patterns in codeowner_mappings.items():
    component = components_by_label.get(label)
    if not component:
        continue
    declared_patterns = [f"/{prefix}" for prefix in component.get("path_prefixes", [])]
    for mapped_pattern in sorted(mapped_patterns):
        if not any(mapped_pattern.startswith(prefix) for prefix in declared_patterns):
            errors.append(
                f"CODEOWNERS: {mapped_pattern} is not declared by {component['id']} path prefixes"
            )
for component in components:
    if component.get("status") == "internal":
        continue
    for prefix in component.get("path_prefixes", []):
        pattern = f"/{prefix}"
        if pattern not in codeowner_mappings.get(component["label"], set()):
            errors.append(f"CODEOWNERS: {pattern} does not map to {component['label']}")

maintainer_labels = {
    scope.get("label")
    for scope in maintainers_doc.get("scopes", [])
    if isinstance(scope, dict)
}
for label in sorted(
    label for label in maintainer_labels - component_labels if str(label).startswith("component:")
):
    errors.append(f"maintainers.json: unknown component scope {label}")
for component in components:
    if component.get("status") in {"active", "deprecated"} and component["label"] not in maintainer_labels:
        errors.append(f"maintainers.json: missing scope for {component['label']}")

ci_workflow_path = GITHUB / "workflows/ci.yaml"
try:
    ci_workflow_text = ci_workflow_path.read_text(encoding="utf-8")
except FileNotFoundError:
    errors.append("missing .github/workflows/ci.yaml")
    ci_workflow_text = ""

ci_job = re.search(
    r"(?ms)^  detect-changes:\s*$\n(?P<body>.*?)(?=^  [a-z0-9-]+:\s*$)",
    ci_workflow_text,
)
if not ci_job:
    errors.append("ci.yaml: unable to read detect-changes job")
else:
    ci_job_text = ci_job.group("body")
    ci_output_pairs = re.findall(
        r'(?m)^\s*echo "([a-z0-9_]+)=\$([A-Z0-9_]+)"\s*>>\s*\$GITHUB_OUTPUT\s*$',
        ci_job_text,
    )
    ci_contracts = {
        "declared output": set(
            re.findall(
                r"(?m)^      ([a-z0-9_]+):\s*\$\{\{\s*steps\.changes\.outputs\.\1\s*\}\}\s*$",
                ci_job_text,
            )
        ),
        "emitted output": {key for key, _ in ci_output_pairs},
        "consumed output": set(
            re.findall(
                r"(?m)^[ \t]*if:[ \t]*needs\.detect-changes\.outputs\.([a-z0-9_]+)"
                r"[ \t]*==[ \t]*['\"]true['\"][ \t]*$",
                ci_workflow_text,
            )
        ),
    }
    expected_ci_keys = set(ci_keys)
    for contract, workflow_keys in ci_contracts.items():
        for key in sorted(expected_ci_keys - workflow_keys):
            errors.append(f"ci.yaml: missing {contract} for component CI key {key!r}")
        for key in sorted(workflow_keys - expected_ci_keys):
            errors.append(f"ci.yaml: unknown {contract} {key!r}")

    ci_path_routes = re.findall(
        r'(?m)^[ \t]*if echo "\$CHANGED" \| grep -qE? "([^"]+)"; then[ \t]*$'
        r"\n[ \t]+([A-Z0-9_]+)=true[ \t]*$",
        ci_job_text,
    )
    declared_ci_route_patterns = {
        f"^{prefix}"
        for component in components
        if component.get("ci_key")
        for prefix in component.get("path_prefixes", [])
    }
    for pattern, _ in ci_path_routes:
        if pattern not in declared_ci_route_patterns:
            errors.append(
                f"ci.yaml: route pattern {pattern!r} is not an exact declared component path"
            )
    for component in components:
        ci_key = component.get("ci_key")
        if not ci_key:
            continue
        route_variables: set[str] = set()
        for prefix in component.get("path_prefixes", []):
            matching_variables = {
                variable for pattern, variable in ci_path_routes if pattern == f"^{prefix}"
            }
            if not matching_variables:
                errors.append(f"ci.yaml: {component['id']} path {prefix!r} has no change route")
            route_variables.update(matching_variables)
        if len(route_variables) != 1:
            errors.append(
                f"ci.yaml: {component['id']} paths route through "
                f"{sorted(route_variables)!r} instead of one output"
            )
            continue
        route_variable = next(iter(route_variables))
        routed_keys = sorted(key for key, variable in ci_output_pairs if variable == route_variable)
        if routed_keys != [ci_key]:
            errors.append(
                f"ci.yaml: {component['id']} CI key {ci_key!r} routes through "
                f"{routed_keys!r}"
            )

release_workflow_path = GITHUB / "workflows/release.yaml"
try:
    release_workflow_text = release_workflow_path.read_text(encoding="utf-8")
except FileNotFoundError:
    errors.append("missing .github/workflows/release.yaml")
    release_workflow_text = ""

if release_workflow_text:
    parsed_release_routes = dict(
        re.findall(
            r'(?m)^\s*([a-z0-9-]+)\)\s+COMPONENT="([^"]+)"\s*;;\s*$',
            release_workflow_text,
        )
    )
    published_release_routes = dict(
        re.findall(
            r'(?m)^\s*([a-z0-9-]+)\)\s+PREFIX="([a-z0-9-]+/v)"',
            release_workflow_text,
        )
    )
    release_contracts = {
        "trigger prefix": set(
            re.findall(
                r"(?m)^\s*-\s*['\"]([a-z0-9-]+/v)\*['\"]\s*$",
                release_workflow_text,
            )
        ),
        "parsed prefix": {f"{scope}/v" for scope in parsed_release_routes},
        "published prefix": set(published_release_routes.values()),
    }
    expected_release_prefixes = set(release_prefixes)
    for contract, workflow_prefixes in release_contracts.items():
        for prefix in sorted(expected_release_prefixes - workflow_prefixes):
            errors.append(
                f"release.yaml: missing {contract} for component release prefix {prefix!r}"
            )
        for prefix in sorted(workflow_prefixes - expected_release_prefixes):
            errors.append(f"release.yaml: unknown {contract} {prefix!r}")

    for component in components:
        release_prefix = component.get("release_tag_prefix")
        if not release_prefix:
            continue
        source_directories = {
            prefix.rstrip("/").rsplit("/", 1)[-1]
            for prefix in component.get("path_prefixes", [])
            if isinstance(prefix, str) and COMPONENT_PATH_PATTERN.fullmatch(prefix)
        }
        if len(source_directories) != 1:
            errors.append(
                f"{component['id']}: release metadata needs one source directory"
            )
            continue
        source_directory = next(iter(source_directories))
        release_scope = release_prefix.removesuffix("/v")
        if parsed_release_routes.get(release_scope) != source_directory:
            errors.append(
                f"release.yaml: {component['id']} prefix {release_prefix!r} parses as "
                f"{parsed_release_routes.get(release_scope)!r}"
            )
        if published_release_routes.get(source_directory) != release_prefix:
            errors.append(
                f"release.yaml: {component['id']} publishes with "
                f"{published_release_routes.get(source_directory)!r} instead of "
                f"{release_prefix!r}"
            )

label_entries = labels_doc.get("labels", [])
require(labels_doc.get("schema_version") == 1, ".github/labels.json schema_version must be 1")
require(isinstance(label_entries, list), ".github/labels.json labels must be a list")
label_names = [entry.get("name", "") for entry in label_entries if isinstance(entry, dict)]
for duplicate in sorted(duplicates(label_names)):
    errors.append(f"duplicate label definition: {duplicate}")
for entry in label_entries:
    if not isinstance(entry, dict):
        errors.append("labels.json entries must be objects")
        continue
    require(bool(entry.get("name")), "labels.json entry missing name")
    require(bool(re.fullmatch(r"[0-9a-fA-F]{6}", str(entry.get("color", "")))), f"{entry.get('name')}: color must be six hex digits")
    require(bool(entry.get("description")), f"{entry.get('name')}: description must not be empty")

print(f"Validated {len(components)} components and {len(label_entries)} governance labels.")
for warning in warnings:
    print(f"WARNING: {warning}", file=sys.stderr)
if errors:
    for error in errors:
        print(f"ERROR: {error}", file=sys.stderr)
    sys.exit(1)
