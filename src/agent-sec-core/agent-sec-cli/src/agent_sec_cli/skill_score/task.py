"""The compact JSON handoff between planning and execution."""

import json
from pathlib import Path, PurePosixPath
from typing import Any, Literal

from agent_sec_cli.skill_score.contracts import Audit, ContractError
from agent_sec_cli.skill_score.evidence.redact import redact_value
from agent_sec_cli.skill_score.hygiene import validate_gaps
from agent_sec_cli.skill_score.io import validate_refs
from agent_sec_cli.skill_score.probes import plan_probes
from pydantic import BaseModel, ConfigDict, Field

SCHEMA_VERSION = "agent-sec-skill-score/task-v1"
FINDING_PRIVATE = {"critical_confirmed", "control_test_refs", "dynamic_target"}
PROBE_DERIVED = {"target", "required"}


class Task(BaseModel):
    model_config = ConfigDict(extra="forbid")
    schema_version: Literal["agent-sec-skill-score/task-v1"]
    skill_path: str = Field(min_length=1)
    content_hash: str = Field(pattern=r"^[a-f0-9]{64}$")
    audit: Audit


def task_json(skill: Path, content_hash: str, audit: Audit) -> dict[str, Any]:
    findings = []
    for finding in audit.findings:
        value = finding.model_dump(exclude=FINDING_PRIVATE, exclude_defaults=True)
        if value.get("dynamic") != "unsupported":
            value.pop("dynamic", None)
        findings.append(value)
    probes = []
    for probe in audit.probes:
        value = probe.model_dump(exclude=PROBE_DERIVED, exclude_defaults=True)
        probes.append(
            {key: item for key, item in value.items() if item not in ("", [], {})}
        )
    return redact_value(
        {
            "schema_version": SCHEMA_VERSION,
            "skill_path": str(skill.resolve()),
            "content_hash": content_hash,
            "audit": {
                "objective": audit.objective,
                "allowed_boundary": audit.allowed_boundary,
                "findings": findings,
                "probes": probes,
                **{
                    key: audit.model_dump()[key]
                    for key in (
                        "control_gaps",
                        "limitations",
                        "omitted_sources",
                        "planning_errors",
                        "missing_implementation",
                    )
                    if getattr(audit, key)
                },
            },
        }
    )


def parse_task(value: Any, base: Path) -> Task:
    if not isinstance(value, dict):
        raise ContractError("task input must be one JSON object")
    value = redact_value(value)
    audit = value.get("audit")
    required = {
        "objective",
        "allowed_boundary",
        "findings",
        "probes",
    }
    if (
        not isinstance(audit, dict)
        or not required <= audit.keys()
        or audit.keys()
        - required
        - {
            "control_gaps",
            "limitations",
            "omitted_sources",
            "planning_errors",
            "missing_implementation",
        }
    ):
        raise ContractError(
            "audit requires objective, allowed_boundary, findings and probes; only documented audit context may be added"
        )
    if not isinstance(audit["findings"], list) or not isinstance(audit["probes"], list):
        raise ContractError("findings and probes must be JSON arrays")
    for finding in audit.get("findings", []):
        if not isinstance(finding, dict) or FINDING_PRIVATE & finding.keys():
            raise ContractError(
                "task findings cannot supply dynamic evidence or Critical confirmation"
            )
        if finding.get("dynamic", "no_proof") not in {"no_proof", "unsupported"}:
            raise ContractError("task findings cannot supply a dynamic verdict")
    for probe in audit.get("probes", []):
        if not isinstance(probe, dict) or PROBE_DERIVED & probe.keys():
            raise ContractError(
                "task target and required are derived, not caller-supplied"
            )
        probe.setdefault("finding_ids", [])
        probe.setdefault("input_path", "")
    task = Task.model_validate(value)
    task.skill_path = str((base / task.skill_path).resolve())
    return task


def read_task_input(argument: str, stdin: str = "") -> Task:
    if argument == "-":
        text, base = stdin, Path.cwd()
    elif argument.lstrip().startswith(("{", "[")):
        text, base = argument, Path.cwd()
    else:
        path = Path(argument).resolve()
        text, base = path.read_text(), path.parent
    return parse_task(json.loads(text), base)


def validate_task(task: Task, source: dict[str, Any], catalog: dict[str, str]) -> None:
    validate_audit_context(task.audit, catalog)
    validate_refs(task.audit.findings, catalog)
    validate_gaps(task.audit.control_gaps, catalog, source_only=True)
    for finding in task.audit.findings:
        if any(
            not ref.startswith("source:") for ref in finding.refs + finding.control_refs
        ):
            raise ContractError("planning findings must reference source evidence only")
    selected = plan_probes(task.audit, source, normalize=False)
    if [p.id for p in selected] != [p.id for p in task.audit.probes]:
        raise ContractError(
            "task list is incomplete, redundant or not in execution order"
        )
    task.audit.probes = selected


def validate_audit_context(audit: Audit, catalog: dict[str, str]) -> None:
    """A score-blocking omission needs a specific absent implementation and source basis."""
    files = {ref[7:].rsplit(":", 1)[0] for ref in catalog if ref.startswith("source:")}
    # Topic uncertainty (for example a runtime-dependent effect) is not missing source.
    # Only the separately evidenced missing_implementation claim can block material completeness.
    for path, refs in audit.missing_implementation.items():
        if (
            not path
            or PurePosixPath(path).is_absolute()
            or ".." in PurePosixPath(path).parts
            or path in files
        ):
            raise ContractError(f"invalid missing implementation: {path}")
        if (
            not refs
            or any(not ref.startswith("source:") or ref not in catalog for ref in refs)
            or not any(path in catalog[ref] for ref in refs)
        ):
            raise ContractError(
                f"missing implementation {path} requires source references that actually name it"
            )
