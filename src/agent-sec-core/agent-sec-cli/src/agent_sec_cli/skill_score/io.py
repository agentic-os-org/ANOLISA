"""Ordinary JSON files, source references and a single initial content hash."""

import hashlib
import json
import os
import re
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_score.contracts import ContractError, Finding
from agent_sec_cli.skill_score.evidence.redact import redact_text, redact_value


def save(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(redact_value(value), ensure_ascii=False, indent=2) + "\n"
    )
    temporary.replace(path)


def read(path: Path) -> Any:
    return json.loads(path.read_text())


def scrub_configured_key(root: Path) -> int:
    """Scrub the actual Runtime credential, including extensionless syscall logs."""
    key = os.environ.get("ANTHROPIC_API_KEY", "")
    changed = 0
    if not key:
        return changed
    for path in root.rglob("*"):
        if not path.is_file() or path.is_symlink():
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeError:
            continue
        if key in text:
            path.write_text(text.replace(key, "[REDACTED]"), encoding="utf-8")
            changed += 1
    return changed


def load_source(skill: Path, *, hash_content: bool = True) -> dict[str, Any]:
    if not (skill / "SKILL.md").is_file():
        raise ContractError("SKILL.md is required")
    digest = hashlib.sha256()
    files: dict[str, str] = {}
    omitted: list[str] = []
    size = 0
    for path in sorted(skill.rglob("*")):
        rel = path.relative_to(skill).as_posix()
        if any(
            part in {".git", "__pycache__", ".venv", "node_modules"}
            for part in path.relative_to(skill).parts
        ):
            continue
        if path.is_symlink():
            if hash_content:
                digest.update((rel + "\0" + os.readlink(path)).encode())
            omitted.append(rel + " (symlink)")
            continue
        if not path.is_file():
            continue
        content = path.read_bytes()
        if hash_content:
            digest.update(
                rel.encode() + b"\0" + len(content).to_bytes(8, "big") + content
            )
        try:
            text = content.decode("utf-8")
            if "\0" in text:
                raise UnicodeError("binary")
        except UnicodeError:
            omitted.append(rel + " (binary)")
            continue
        size += len(text)
        if size > 600000:
            raise ContractError(
                "source exceeds the single-audit context limit; no silent truncation"
            )
        files[rel] = redact_text(text)
    return {
        "name": skill.name,
        "content_hash": digest.hexdigest() if hash_content else None,
        "files": files,
        "omitted": omitted,
    }


def source_catalog(source: dict[str, Any]) -> dict[str, str]:
    return {
        f"source:{path}:{number}": line
        for path, text in source["files"].items()
        for number, line in enumerate(text.splitlines(), 1)
    }


def missing_scripts(source: dict[str, Any]) -> list[str]:
    """Flag explicit local script references; let the audit decide materiality."""
    text = source["files"].get("SKILL.md", "")
    candidates = re.findall(
        r"(?:`|\]\()(?:\./)?(scripts/[A-Za-z0-9_./-]+\.(?:py|sh|js|ts))(?=[`\s)])", text
    )
    return sorted({path for path in candidates if path not in source["files"]})


def validate_refs(findings: list[Finding], catalog: dict[str, str]) -> None:
    ids: set[str] = set()
    for finding in findings:
        if not re.fullmatch(r"[A-Za-z0-9_-]{1,80}", finding.id) or finding.id in ids:
            raise ContractError("finding IDs must be unique safe identifiers")
        ids.add(finding.id)
        if any(ref not in catalog for ref in finding.refs + finding.control_refs):
            raise ContractError(f"{finding.id}: evidence reference does not exist")
        if (
            finding.status in {"material_risk", "unresolved"}
            and not finding.boundary.strip()
        ):
            raise ContractError(
                f"{finding.id}: a concrete security boundary is required"
            )
