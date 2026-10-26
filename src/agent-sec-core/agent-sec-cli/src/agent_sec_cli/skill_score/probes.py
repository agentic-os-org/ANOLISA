"""Select concrete tasks once and validate the planned task list."""

import base64
import io
import tarfile
from pathlib import PurePosixPath
from typing import Any

from agent_sec_cli.skill_score.contracts import (
    Audit,
    ContractError,
    Fixture,
    Probe,
)


def validate_deb_fixture(content: bytes) -> None:
    """Check a synthetic Debian archive's layout without extracting or executing it."""
    if not content.startswith(b"!<arch>\n"):
        raise ContractError("Debian fixture is not an ar archive")
    offset, members = 8, {}
    while offset < len(content):
        header = content[offset : offset + 60]
        if len(header) != 60 or header[58:] != b"`\n":
            raise ContractError("Debian fixture has an invalid ar member header")
        try:
            name = header[:16].decode("ascii").strip().removesuffix("/")
            size = int(header[48:58].decode("ascii").strip())
        except (UnicodeDecodeError, ValueError) as exc:
            raise ContractError("Debian fixture has an invalid ar member name/size") from exc
        offset += 60
        if size < 0 or offset + size > len(content) or name in members:
            raise ContractError("Debian fixture has a truncated or duplicate member")
        members[name] = content[offset : offset + size]
        offset += size + size % 2
    if offset != len(content) or members.get("debian-binary", b"").strip() != b"2.0":
        raise ContractError("Debian fixture has invalid alignment or format version")
    for prefix in ("control.tar", "data.tar"):
        matches = [
            data
            for name, data in members.items()
            if name == prefix or name.startswith(prefix + ".")
        ]
        if len(matches) != 1:
            raise ContractError(f"Debian fixture needs exactly one {prefix} member")
        try:
            with tarfile.open(fileobj=io.BytesIO(matches[0]), mode="r:*") as archive:
                total, names = 0, set()
                for index, member in enumerate(archive):
                    total += member.size
                    if index >= 256 or total > 16 * 1024 * 1024 or member.size < 0:
                        raise ContractError(
                            "Synthetic archive exceeds the fixture inspection limit"
                        )
                    names.add(member.name.removeprefix("./"))
                    if member.isfile():
                        with archive.extractfile(member) as stream:
                            if len(stream.read(member.size + 1)) != member.size:
                                raise ContractError("Debian fixture has truncated tar data")
                if prefix == "control.tar" and "control" not in names:
                    raise ContractError("Debian fixture is missing package control metadata")
        except (tarfile.TarError, OSError, EOFError) as exc:
            raise ContractError(
                f"Debian fixture has invalid/unsupported {prefix} data: {exc}"
            ) from exc


def validate_input(probe: Probe) -> None:
    """Validate preparation and consumption layout, not a canary substring."""
    if probe.fixtures and not probe.input_path:
        raise ContractError(
            f"{probe.id}: supplied fixtures need their actual consumption input_path; "
            "only genuinely input-free tasks have both empty input_path and empty fixtures"
        )
    if probe.template != "baseline" and probe.target in probe.expected_result:
        raise ContractError(
            f"{probe.id}: expected_result describes the normal business output only; "
            "put the security target and trigger in target/activation, not the business expectation"
        )
    for name, fixture in probe.fixtures.items():
        if name.endswith(".deb") and isinstance(fixture, Fixture) and fixture.kind == "binary":
            validate_deb_fixture(base64.b64decode(fixture.content, validate=True))
    if probe.input_preparation and not probe.input_path:
        raise ContractError(f"{probe.id}: generated input needs its actual consumption path")
    if probe.input_path.startswith("/app/work/"):
        path = probe.input_path.removeprefix("/app/work/")
        if ".." in PurePosixPath(path).parts or str(PurePosixPath(path)) != path:
            raise ContractError(f"{probe.id}: input must be a contained normalized work path")
        value = probe.fixtures.get(path)
        directory = (isinstance(value, Fixture) and value.kind == "directory") or any(
            p.startswith(path + "/") for p in probe.fixtures
        )
        if value is None and not directory and not probe.input_preparation:
            raise ContractError(
                f"{probe.id}: input_path is absent from fixtures; declare baseline input_preparation or a real mock route"
            )
        if probe.template != "baseline" and directory and not probe.payload_path:
            raise ContractError(
                f"{probe.id}: directory probe needs the actual payload_path consumed within it"
            )
        if probe.payload_path:
            payload = probe.payload_path.removeprefix("/app/work/")
            if (
                not probe.payload_path.startswith("/app/work/")
                or payload not in probe.fixtures
                or not (payload == path or payload.startswith(path + "/"))
            ):
                raise ContractError(
                    f"{probe.id}: payload_path={probe.payload_path!r} is not a supplied input within input_path={probe.input_path!r}; fixtures={list(probe.fixtures)[:10]}. Set input_path to the actual consumed configuration/data file or directory, not the resource being checked/changed. payload_path is only a file/link inside that directory."
                )
            item = probe.fixtures[payload]
            if isinstance(item, Fixture) and item.kind == "directory":
                raise ContractError(f"{probe.id}: payload_path must identify a file or link")
    elif probe.payload_path:
        raise ContractError(
            f"{probe.id}: mock input must use the configured response, not a file payload_path"
        )


def plan_probes(
    audit: Audit, source: dict[str, Any], *, normalize: bool = True, require_coverage: bool = True
) -> list[Probe]:
    for probe in audit.probes:
        validate_input(probe)
    baseline = [probe for probe in audit.probes if probe.template == "baseline"]
    if not baseline and audit.planning_errors.get("baseline"):
        if audit.probes:
            raise ContractError("targeted tasks require a valid normal task")
        if require_coverage and any(
            f.scope == "skill"
            and f.severity in {"Critical", "High"}
            and f.status in {"material_risk", "unresolved"}
            and f.dynamic != "unsupported"
            for f in audit.findings
        ):
            raise ContractError("missing baseline requires explicit unsupported risk paths")
        return []
    if len(baseline) != 1:
        raise ContractError("audit must produce exactly one baseline")
    if not normalize and baseline[0].id != "baseline":
        raise ContractError("the baseline task must have id=baseline")
    if baseline[0].entry_path not in source["files"]:
        raise ContractError("baseline entry does not exist")
    if baseline[0].finding_ids:
        raise ContractError("baseline cannot target a risk finding")
    baseline[0].id, baseline[0].required = "baseline", True
    if not baseline[0].expected_result.strip():
        raise ContractError("baseline needs a concrete expected task result")
    findings = {finding.id: finding for finding in audit.findings}
    seen = {"baseline"}
    targets = []
    exploration_count = medium_count = 0
    for probe in audit.probes:
        if probe.template == "baseline":
            continue
        if probe.id in seen or not probe.id.replace("-", "").replace("_", "").isalnum():
            raise ContractError("probe IDs must be unique safe names")
        seen.add(probe.id)
        if probe.entry_path not in source["files"]:
            raise ContractError("probe must reference an existing entry")
        if not probe.expected_result.strip() or not probe.input_path:
            raise ContractError("probe needs expected_result and concrete input_path")
        if len(probe.finding_ids) > 1:
            raise ContractError("a targeted probe must test exactly one merged root")
        if any(identifier not in findings for identifier in probe.finding_ids):
            raise ContractError("probe references unknown finding")
        script_refs = {
            ref.removeprefix("source:").rsplit(":", 1)[0]
            for identifier in probe.finding_ids
            for ref in findings[identifier].refs
            if ref.startswith("source:")
            and ref.removeprefix("source:").rsplit(":", 1)[0].endswith((".py", ".sh", ".js", ".ts"))
        }
        if probe.entry_path == "SKILL.md" and script_refs:
            raise ContractError(
                f"{probe.id}: risk cites executable source {sorted(script_refs)}; choose its real script entry, not SKILL.md, or mark an unobservable root unsupported."
            )
        probe.required = any(
            findings[identifier].scope == "skill"
            and findings[identifier].severity in {"Critical", "High"}
            and findings[identifier].status in {"material_risk", "unresolved"}
            for identifier in probe.finding_ids
        )
        if not probe.finding_ids:
            exploration_count += 1
            if exploration_count > 1 or any(
                f.scope == "skill" and f.status in {"material_risk", "unresolved"}
                for f in findings.values()
            ):
                audit.corrections.append(
                    f"Skipped optional probe {probe.id}: exploration limited to one and only without active risks."
                )
                continue
            targets.append(probe)
            continue
        if all(
            findings[identifier].severity == "Low"
            or findings[identifier].scope == "environment"
            or findings[identifier].status in {"accepted", "refuted"}
            for identifier in probe.finding_ids
        ):
            continue
        if not probe.required:
            medium_count += 1
            if medium_count > 1:
                audit.corrections.append(f"Skipped optional probe {probe.id}: Medium limit is one.")
                continue
        if any(set(probe.finding_ids) & set(p.finding_ids) for p in targets):
            audit.corrections.append(
                f"Skipped redundant probe {probe.id}: one probe per merged root."
            )
            continue
        targets.append(probe)
    covered = {identifier for probe in targets for identifier in probe.finding_ids}
    for finding in audit.findings:
        if (
            finding.severity in {"Critical", "High"}
            and finding.scope == "skill"
            and finding.status in {"material_risk", "unresolved"}
            and finding.id not in covered
            and finding.dynamic != "unsupported"
            and require_coverage
        ):
            raise ContractError(
                f"{finding.id}: necessary probe missing without explicit unsupported status"
            )
    return baseline + sorted(targets, key=lambda probe: not probe.required)
