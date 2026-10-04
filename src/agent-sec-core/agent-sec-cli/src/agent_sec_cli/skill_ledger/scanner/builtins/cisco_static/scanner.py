"""Static-only Skill scanner inspired by Cisco AI Defense skill-scanner.

This module intentionally implements only local static checks.  It does not
import cisco-ai-skill-scanner, YARA, LLM analyzers, remote services, or UI
dependencies.

Memory bounding: the scan streams one file at a time and never retains all
decoded files at once.  The tree walk enforces the shared file-count,
directory-depth, and aggregate-byte budgets from ``scanner.limits``; file
reads are size-checked first and capped at ``maxFileBytes + 1`` so an
oversized file is never fully buffered.  Cumulative decoded text and total
findings carry their own explicit caps.  Tripping any cap stops the scan and
emits one structured ``scanner_limit`` diagnostic finding instead of
scanning unbounded content.
"""

from __future__ import annotations

import os
import re
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path
from typing import Any, Iterator

import yaml

from agent_sec_cli.skill_ledger.scanner.limits import (
    MAX_DIRECTORY_DEPTH,
    MAX_FILES,
    MAX_TOTAL_BYTES,
)
from agent_sec_cli.skill_ledger.scanner.names import STATIC_SCANNER_NAME

SCANNER_NAME = STATIC_SCANNER_NAME
SCANNER_VERSION = "cisco-static-only-0.2.0"
SCANNER_SOURCE = "cisco-skill-scanner-static-only"

_SKILL_MANIFEST = "SKILL.md"
_DEFAULT_MAX_FILE_BYTES = 1_000_000
_MAX_TOTAL_TEXT_CHARS = 50_000_000
_MAX_FINDINGS = 10_000
_SKIP_DIRS = frozenset(
    {
        ".git",
        ".skill-meta",
        ".pytest_cache",
        "__pycache__",
        "build",
        "dist",
        "node_modules",
    }
)
_CODE_EXTENSIONS = frozenset(
    {
        ".bash",
        ".cjs",
        ".js",
        ".mjs",
        ".pl",
        ".ps1",
        ".py",
        ".rb",
        ".sh",
        ".ts",
        ".zsh",
    }
)
_TEXT_EXTENSIONS = frozenset(
    {
        "",
        ".bash",
        ".cfg",
        ".conf",
        ".cjs",
        ".ini",
        ".js",
        ".json",
        ".md",
        ".mjs",
        ".pl",
        ".ps1",
        ".py",
        ".rb",
        ".sh",
        ".toml",
        ".ts",
        ".txt",
        ".yaml",
        ".yml",
        ".zsh",
    }
)
_SUSPICIOUS_BINARY_EXTENSIONS = frozenset(
    {
        ".bin",
        ".class",
        ".dll",
        ".dylib",
        ".exe",
        ".jar",
        ".o",
        ".so",
        ".wasm",
    }
)
_SECRET_FILE_NAMES = frozenset(
    {
        ".env",
        ".netrc",
        ".npmrc",
        ".pypirc",
        "id_ed25519",
        "id_rsa",
    }
)
_ALLOWED_HIDDEN_FILE_PATHS = frozenset(
    {
        # OpenClaw ClawHub installs record per-skill origin metadata here.
        (".clawhub", "origin.json"),
    }
)
_NETWORK_HINT_RE = re.compile(
    r"\b(curl|wget)\b|\brequests\.(get|post|put|delete)\s*\(|\burllib\.request\b|"
    r"\bfetch\s*\(|https?://",
    re.IGNORECASE,
)
_NETWORK_DECLARATION_RE = re.compile(
    r"\b(network|http|https|url|download|fetch|remote|联网|网络|下载|远程)\b",
    re.IGNORECASE,
)


@dataclass(frozen=True)
class StaticRule:
    """A single static regex rule loaded from package YAML."""

    id: str
    target: str
    severity: str
    category: str
    title: str
    message: str
    remediation: str
    pattern: str
    compiled: re.Pattern[str]


@dataclass
class _ScanBudget:
    """Mutable accounting for one bounded scan (not a retained buffer)."""

    file_count: int = 0
    total_bytes: int = 0
    text_chars: int = 0


@dataclass(frozen=True)
class _StreamedFile:
    """One decoded file, dropped as soon as its rules have run."""

    rel_path: str
    text: str
    is_code: bool


def scan_skill(
    skill_dir: str | Path,
    *,
    options: dict[str, Any] | None = None,
) -> list[dict[str, Any]]:
    """Scan a Skill directory and return ``NormalizedFinding`` dictionaries.

    The scan is streaming and bounded: at most one decoded file is held at a
    time, and file-count / depth / aggregate-byte / cumulative-text /
    findings caps stop the scan with a structured diagnostic instead of
    scaling memory with the Skill's aggregate text size.
    """
    root = Path(skill_dir).resolve()
    opts = options or {}
    max_file_bytes = int(opts.get("maxFileBytes", _DEFAULT_MAX_FILE_BYTES))
    rules = _load_rules()
    findings: list[dict[str, Any]] = []

    skill_path = root / _SKILL_MANIFEST
    skill_text = _read_required_text(
        skill_path, _SKILL_MANIFEST, findings, max_file_bytes
    )
    front_matter: dict[str, Any] = {}
    network_declared = False
    if skill_text is not None:
        front_matter, body_text = _scan_skill_manifest(skill_text, findings)
        network_declared = _network_declared(front_matter)
        for rule in rules:
            if rule.target != "skill":
                continue
            try:
                _apply_rule(rule, _SKILL_MANIFEST, body_text, findings)
            except Exception as exc:
                findings.append(_rule_error_finding(rule, exc))

    budget = _ScanBudget()
    network_reported = False
    for streamed in _iter_streamed_files(
        root, findings, budget, max_file_bytes, skill_text
    ):
        for rule in rules:
            try:
                if rule.target == "all_text":
                    _apply_rule(rule, streamed.rel_path, streamed.text, findings)
                elif rule.target == "code" and streamed.is_code:
                    _apply_rule(rule, streamed.rel_path, streamed.text, findings)
            except Exception as exc:
                findings.append(_rule_error_finding(rule, exc))

        if not network_declared and not network_reported and streamed.is_code:
            network_hint = _find_network_hint(streamed.text)
            if network_hint is not None:
                line_number, matched_text = network_hint
                findings.append(
                    _finding(
                        rule="undeclared-network-access",
                        severity="medium",
                        message=(
                            "Skill helper content appears to use network access "
                            "not declared in metadata."
                        ),
                        file=streamed.rel_path,
                        line=line_number,
                        metadata={
                            "category": "network",
                            "title": "Undeclared network behavior",
                            "remediation": (
                                "Declare network behavior in SKILL.md metadata "
                                "or remove the network call."
                            ),
                            "matchedText": _safe_excerpt(matched_text),
                        },
                    )
                )
                network_reported = True

        if len(findings) >= _MAX_FINDINGS:
            findings.append(
                _limit_finding(
                    "findings-limit",
                    "Static scan stopped after reaching the findings budget.",
                    metadata={
                        "max_findings": _MAX_FINDINGS,
                    },
                )
            )
            break

    return findings


def _iter_streamed_files(
    root: Path,
    findings: list[dict[str, Any]],
    budget: _ScanBudget,
    max_file_bytes: int,
    skill_text: str | None,
) -> Iterator[_StreamedFile]:
    """Stream decoded text files one at a time under the scan budgets.

    Walking uses ``os.walk`` directly instead of materialising
    ``sorted(root.rglob("*"))``, so the path list itself never scales with
    tree size.  Each yielded file is fully processed and dropped by the
    caller before the next one is read.  The manifest is not re-read: when
    the required read succeeded its text is reused, and when it failed the
    required read already emitted the manifest's diagnostic.
    """
    for path in _iter_skill_files(root, findings, budget):
        rel_path = path.relative_to(root).as_posix()
        _scan_path_metadata(path, rel_path, findings)
        if rel_path == _SKILL_MANIFEST and path.parent == root:
            if skill_text is None:
                continue
            text = skill_text
        else:
            text = _read_optional_text(path, rel_path, max_file_bytes, findings)
            if text is None:
                continue
        budget.text_chars += len(text)
        if budget.text_chars > _MAX_TOTAL_TEXT_CHARS:
            findings.append(
                _limit_finding(
                    "cumulative-text-limit",
                    "Skill text content exceeds the cumulative static-scan budget.",
                    metadata={
                        "max_total_text_chars": _MAX_TOTAL_TEXT_CHARS,
                        "total_text_chars": budget.text_chars,
                    },
                )
            )
            return
        yield _StreamedFile(
            rel_path=rel_path,
            text=text,
            is_code=_is_code_file(path, text),
        )


def _iter_skill_files(
    root: Path,
    findings: list[dict[str, Any]],
    budget: _ScanBudget,
) -> Iterator[Path]:
    """Yield regular files under *root*, enforcing the shared tree budgets.

    Symlinked directories are pruned with a warning finding (matching the
    previous ``rglob`` behavior); deeper-than-budget directories are pruned
    with a ``directory-depth-limit`` diagnostic; exceeding the file-count or
    aggregate-byte budget stops the walk with one diagnostic each.
    """
    for current_root, dirnames, filenames in os.walk(root, followlinks=False):
        current = Path(current_root)
        relative_dir = current.relative_to(root)
        if len(relative_dir.parts) > MAX_DIRECTORY_DEPTH:
            findings.append(
                _limit_finding(
                    "directory-depth-limit",
                    f"Skill directory depth exceeds {MAX_DIRECTORY_DEPTH}; "
                    "deeper entries are not scanned.",
                    file=relative_dir.as_posix(),
                    metadata={"max_directory_depth": MAX_DIRECTORY_DEPTH},
                )
            )
            dirnames[:] = []
            continue

        kept: list[str] = []
        for dirname in sorted(dirnames):
            if dirname in _SKIP_DIRS:
                continue
            entry = current / dirname
            if entry.is_symlink():
                _scan_symlink(root, entry, entry.relative_to(root).as_posix(), findings)
                continue
            kept.append(dirname)
        dirnames[:] = kept

        for filename in sorted(filenames):
            entry = current / filename
            rel_path = entry.relative_to(root).as_posix()
            if entry.is_symlink():
                _scan_symlink(root, entry, rel_path, findings)
                continue
            if not entry.is_file():
                continue
            try:
                size = entry.stat().st_size
            except OSError:
                continue

            budget.file_count += 1
            if budget.file_count > MAX_FILES:
                findings.append(
                    _limit_finding(
                        "file-count-limit",
                        f"Skill contains more than {MAX_FILES} scannable files.",
                        metadata={
                            "max_files": MAX_FILES,
                            "file_count": budget.file_count,
                        },
                    )
                )
                return

            budget.total_bytes += size
            if budget.total_bytes > MAX_TOTAL_BYTES:
                findings.append(
                    _limit_finding(
                        "total-size-limit",
                        f"Skill content exceeds {MAX_TOTAL_BYTES} bytes.",
                        metadata={
                            "max_total_bytes": MAX_TOTAL_BYTES,
                            "total_bytes": budget.total_bytes,
                        },
                    )
                )
                return

            yield entry


@lru_cache(maxsize=1)
def _load_rules() -> tuple[StaticRule, ...]:
    """Load bundled static rules from YAML."""
    path = Path(__file__).with_name("rules") / "static_rules.yaml"
    with path.open(encoding="utf-8") as fh:
        data = yaml.safe_load(fh)
    if not isinstance(data, dict) or not isinstance(data.get("rules"), list):
        raise ValueError(f"Invalid Cisco static scanner rules file: {path}")

    rules: list[StaticRule] = []
    for idx, item in enumerate(data["rules"]):
        if not isinstance(item, dict):
            raise ValueError(f"Invalid rule at index {idx}: expected object")
        pattern = str(item["pattern"])
        rules.append(
            StaticRule(
                id=str(item["id"]),
                target=str(item["target"]),
                severity=str(item["severity"]),
                category=str(item["category"]),
                title=str(item["title"]),
                message=str(item["message"]),
                remediation=str(item["remediation"]),
                pattern=pattern,
                compiled=re.compile(pattern, re.IGNORECASE | re.MULTILINE),
            )
        )
    return tuple(rules)


def _scan_skill_manifest(
    text: str,
    findings: list[dict[str, Any]],
) -> tuple[dict[str, Any], str]:
    """Validate SKILL.md front matter and return ``(metadata, body)``."""
    metadata: dict[str, Any] = {}
    body = text
    front_matter_present = False

    lines = text.splitlines()
    if lines and lines[0].strip() == "---":
        front_matter_present = True
        closing_idx = next(
            (
                idx
                for idx, line in enumerate(lines[1:], start=1)
                if line.strip() == "---"
            ),
            None,
        )
        if closing_idx is None:
            findings.append(
                _finding(
                    rule="skill-frontmatter-unclosed",
                    severity="medium",
                    message="SKILL.md front matter starts with '---' but has no closing delimiter.",
                    file=_SKILL_MANIFEST,
                    line=1,
                    metadata={
                        "category": "manifest",
                        "title": "Unclosed Skill metadata",
                        "remediation": "Close YAML front matter with a second '---' line.",
                    },
                )
            )
        else:
            raw_yaml = "\n".join(lines[1:closing_idx])
            body = "\n".join(lines[closing_idx + 1 :])
            try:
                parsed = yaml.safe_load(raw_yaml) or {}
                if isinstance(parsed, dict):
                    metadata = parsed
                else:
                    findings.append(
                        _finding(
                            rule="skill-frontmatter-invalid",
                            severity="medium",
                            message="SKILL.md front matter must be a YAML object.",
                            file=_SKILL_MANIFEST,
                            line=1,
                            metadata={
                                "category": "manifest",
                                "title": "Invalid Skill metadata",
                                "remediation": "Use key-value YAML front matter.",
                            },
                        )
                    )
            except yaml.YAMLError as exc:
                findings.append(
                    _finding(
                        rule="skill-frontmatter-invalid",
                        severity="medium",
                        message=f"SKILL.md front matter is invalid YAML: {exc}",
                        file=_SKILL_MANIFEST,
                        line=1,
                        metadata={
                            "category": "manifest",
                            "title": "Invalid Skill metadata",
                            "remediation": "Fix YAML syntax in SKILL.md front matter.",
                        },
                    )
                )

    if not front_matter_present:
        findings.append(
            _finding(
                rule="skill-frontmatter-missing",
                severity="medium",
                message="SKILL.md is missing YAML front matter.",
                file=_SKILL_MANIFEST,
                line=1,
                metadata={
                    "category": "manifest",
                    "title": "Missing Skill metadata",
                    "remediation": "Add YAML front matter with name and description fields.",
                },
            )
        )

    for key in ("name", "description"):
        if not metadata.get(key):
            findings.append(
                _finding(
                    rule=f"skill-metadata-missing-{key}",
                    severity="medium",
                    message=f"SKILL.md front matter is missing required field: {key}.",
                    file=_SKILL_MANIFEST,
                    line=1,
                    metadata={
                        "category": "manifest",
                        "title": "Missing Skill metadata field",
                        "remediation": f"Add a non-empty {key!r} field to SKILL.md front matter.",
                    },
                )
            )

    return metadata, body


def _scan_symlink(
    root: Path,
    path: Path,
    rel_path: str,
    findings: list[dict[str, Any]],
) -> None:
    """Warn when a Skill contains symlinks, especially ones escaping the root."""
    try:
        target = path.resolve(strict=True)
        escapes_root = not target.is_relative_to(root)
    except OSError:
        target = None
        escapes_root = True

    findings.append(
        _finding(
            rule="path-escape-symlink" if escapes_root else "symlink-file",
            severity="high" if escapes_root else "medium",
            message=(
                "Skill contains a symlink that resolves outside the Skill directory."
                if escapes_root
                else "Skill contains a symlink; symlink targets are not scanned."
            ),
            file=rel_path,
            metadata={
                "category": "path_escape" if escapes_root else "filesystem",
                "title": (
                    "Symlink target escapes Skill directory"
                    if escapes_root
                    else "Symlink skipped"
                ),
                "remediation": "Replace symlinks with regular files inside the Skill directory.",
                "target": (
                    "unresolved"
                    if target is None
                    else "outside-root" if escapes_root else "inside-root"
                ),
            },
        )
    )


def _scan_path_metadata(
    path: Path,
    rel_path: str,
    findings: list[dict[str, Any]],
) -> None:
    """Scan file names and extensions for static risk signals."""
    parts = Path(rel_path).parts
    if any(part.startswith(".") for part in parts):
        if path.name in _SECRET_FILE_NAMES:
            findings.append(
                _finding(
                    rule="secret-material-file",
                    severity="high",
                    message="Skill contains a file name commonly used for secrets or credentials.",
                    file=rel_path,
                    metadata={
                        "category": "credential_access",
                        "title": "Credential-like file included",
                        "remediation": "Remove secrets and credential files from the Skill package.",
                    },
                )
            )
        elif not _is_allowed_hidden_file_path(parts):
            findings.append(
                _finding(
                    rule="hidden-file",
                    severity="medium",
                    message="Skill contains a hidden file or directory.",
                    file=rel_path,
                    metadata={
                        "category": "filesystem",
                        "title": "Hidden file included",
                        "remediation": "Keep hidden files out of Skill packages unless they are documented and required.",
                    },
                )
            )

    if path.suffix.lower() in _SUSPICIOUS_BINARY_EXTENSIONS:
        findings.append(
            _finding(
                rule="suspicious-binary-asset",
                severity="medium",
                message="Skill contains a binary executable or bytecode-like asset.",
                file=rel_path,
                metadata={
                    "category": "binary_asset",
                    "title": "Suspicious binary asset",
                    "remediation": "Remove binary executables or document and verify their provenance.",
                },
            )
        )


def _read_required_text(
    path: Path,
    rel_path: str,
    findings: list[dict[str, Any]],
    max_file_bytes: int,
) -> str | None:
    """Read a required text file and create a warning finding on failure.

    The size is checked before any read, and the read itself is capped at
    ``max_file_bytes + 1`` bytes so an oversized required file is never
    fully buffered.
    """
    try:
        size = path.stat().st_size
    except OSError as exc:
        findings.append(
            _finding(
                rule="file-read-error",
                severity="medium",
                message=f"Required file could not be read: {exc}",
                file=rel_path,
                metadata={
                    "category": "scanner_error",
                    "title": "File read error",
                    "remediation": "Ensure the Skill file is readable.",
                },
            )
        )
        return None
    if size > max_file_bytes:
        findings.append(_large_file_finding(rel_path, max_file_bytes))
        return None
    try:
        with path.open("rb") as fh:
            raw = fh.read(max_file_bytes + 1)
    except OSError as exc:
        findings.append(
            _finding(
                rule="file-read-error",
                severity="medium",
                message=f"Required file could not be read: {exc}",
                file=rel_path,
                metadata={
                    "category": "scanner_error",
                    "title": "File read error",
                    "remediation": "Ensure the Skill file is readable.",
                },
            )
        )
        return None
    if len(raw) > max_file_bytes:
        findings.append(_large_file_finding(rel_path, max_file_bytes))
        return None
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        findings.append(
            _finding(
                rule="file-decode-error",
                severity="medium",
                message=f"Required file is not valid UTF-8 text: {exc}",
                file=rel_path,
                metadata={
                    "category": "scanner_error",
                    "title": "File decode error",
                    "remediation": "Store SKILL.md as UTF-8 text.",
                },
            )
        )
        return None


def _read_optional_text(
    path: Path,
    rel_path: str,
    max_file_bytes: int,
    findings: list[dict[str, Any]],
) -> str | None:
    """Read a text-like file.  Binary or oversized files are skipped.

    The size is checked via ``stat`` before any read, and the read itself is
    capped at ``max_file_bytes + 1`` bytes, so a file that is oversized (or
    grows between ``stat`` and read) is never fully buffered.
    """
    if path.suffix.lower() not in _TEXT_EXTENSIONS:
        return None
    try:
        size = path.stat().st_size
    except OSError as exc:
        findings.append(
            _finding(
                rule="file-read-error",
                severity="medium",
                message=f"File could not be read during static scan: {exc}",
                file=rel_path,
                metadata={
                    "category": "scanner_error",
                    "title": "File read error",
                    "remediation": "Ensure the Skill file is readable.",
                },
            )
        )
        return None
    if size > max_file_bytes:
        findings.append(_large_file_finding(rel_path, max_file_bytes))
        return None
    try:
        with path.open("rb") as fh:
            raw = fh.read(max_file_bytes + 1)
    except OSError as exc:
        findings.append(
            _finding(
                rule="file-read-error",
                severity="medium",
                message=f"File could not be read during static scan: {exc}",
                file=rel_path,
                metadata={
                    "category": "scanner_error",
                    "title": "File read error",
                    "remediation": "Ensure the Skill file is readable.",
                },
            )
        )
        return None
    if len(raw) > max_file_bytes:
        findings.append(_large_file_finding(rel_path, max_file_bytes))
        return None
    if b"\0" in raw:
        return None
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        findings.append(
            _finding(
                rule="file-decode-error",
                severity="medium",
                message="File is not valid UTF-8 text and could not be scanned.",
                file=rel_path,
                metadata={
                    "category": "scanner_error",
                    "title": "File decode error",
                    "remediation": "Store text-like Skill files as UTF-8 text.",
                },
            )
        )
        return None


def _large_file_finding(rel_path: str, max_file_bytes: int) -> dict[str, Any]:
    """Build the shared oversized-file diagnostic."""
    return _finding(
        rule="large-file-skipped",
        severity="medium",
        message="File exceeded static scanner size limit and was skipped.",
        file=rel_path,
        metadata={
            "category": "scanner_limit",
            "title": "Large file skipped",
            "remediation": (
                "Keep Skill files small enough for static review or raise the "
                "scanner limit."
            ),
            "maxFileBytes": max_file_bytes,
        },
    )


def _rule_error_finding(rule: StaticRule, exc: Exception) -> dict[str, Any]:
    """Build the per-rule failure diagnostic."""
    return _finding(
        rule="scanner-rule-error",
        severity="medium",
        message=f"Static rule {rule.id!r} failed during scan: {exc}",
        metadata={
            "category": "scanner_error",
            "title": "Static rule error",
            "remediation": "Fix or disable the failing static rule.",
        },
    )


def _limit_finding(
    rule: str,
    message: str,
    *,
    file: str | None = None,
    metadata: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Build a structured scan-budget diagnostic."""
    return _finding(
        rule=rule,
        severity="medium",
        message=message,
        file=file,
        metadata={
            "category": "scanner_limit",
            "title": "Scan budget limit reached",
            "remediation": (
                "Reduce the Skill size or split it so it can be scanned "
                "within the documented memory budget."
            ),
            **(metadata or {}),
        },
    )


def _apply_rule(
    rule: StaticRule,
    rel_path: str,
    text: str,
    findings: list[dict[str, Any]],
) -> None:
    """Apply one regex rule to one text buffer."""
    match = rule.compiled.search(text)
    if match is None:
        return
    findings.append(
        _finding(
            rule=rule.id,
            severity=rule.severity,
            message=rule.message,
            file=rel_path,
            line=_line_for_offset(text, match.start()),
            metadata={
                "category": rule.category,
                "title": rule.title,
                "remediation": rule.remediation,
                "matchedText": _safe_excerpt(match.group(0)),
            },
        )
    )


def _network_declared(front_matter: dict[str, Any]) -> bool:
    """Return whether SKILL.md metadata declares network behavior."""
    declaration_text = " ".join(
        str(front_matter.get(key, ""))
        for key in ("description", "allowedTools", "allowed_tools", "capabilities")
    )
    return _NETWORK_DECLARATION_RE.search(declaration_text) is not None


def _find_network_hint(text: str) -> tuple[int, str] | None:
    """Find network behavior in executable text, ignoring code comments."""
    in_block_comment = False
    for line_number, line in enumerate(text.splitlines(), start=1):
        code_line, in_block_comment = _strip_network_comment_text(
            line, in_block_comment
        )
        match = _NETWORK_HINT_RE.search(code_line)
        if match is not None:
            return line_number, match.group(0)
    return None


def _strip_network_comment_text(
    line: str,
    in_block_comment: bool,
) -> tuple[str, bool]:
    """Remove comment text before applying the undeclared-network heuristic."""
    if in_block_comment:
        end = line.find("*/")
        if end == -1:
            return "", True
        line = line[end + 2 :]
        in_block_comment = False

    while True:
        start = line.find("/*")
        if start == -1:
            break
        end = line.find("*/", start + 2)
        if end == -1:
            return line[:start], True
        line = f"{line[:start]} {line[end + 2 :]}"

    comment_start = _line_comment_start(line)
    if comment_start is not None:
        line = line[:comment_start]
    return line, in_block_comment


def _line_comment_start(line: str) -> int | None:
    """Return the first Python/shell/JS-style comment marker in a code line."""
    markers = [idx for idx in (line.find("#"), _slash_comment_start(line)) if idx >= 0]
    if not markers:
        return None
    return min(markers)


def _slash_comment_start(line: str) -> int:
    """Find a ``//`` comment marker without treating URL schemes as comments."""
    start = 0
    while True:
        idx = line.find("//", start)
        if idx == -1:
            return -1
        if idx > 0 and line[idx - 1] == ":":
            start = idx + 2
            continue
        return idx


def _is_allowed_hidden_file_path(parts: tuple[str, ...]) -> bool:
    return parts in _ALLOWED_HIDDEN_FILE_PATHS


def _is_code_file(path: Path, text: str) -> bool:
    """Return whether a text file should be treated as executable/helper code."""
    suffix = path.suffix.lower()
    if suffix in _CODE_EXTENSIONS:
        return True
    lines = text.splitlines()
    first_line = lines[0] if lines else ""
    return first_line.startswith("#!") and any(
        marker in first_line.lower()
        for marker in ("bash", "sh", "zsh", "python", "node", "ruby", "perl")
    )


def _finding(
    *,
    rule: str,
    severity: str,
    message: str,
    file: str | None = None,
    line: int | None = None,
    metadata: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Build a ``NormalizedFinding`` dict with Cisco-style metadata preserved."""
    item: dict[str, Any] = {
        "rule": rule,
        "level": _level_from_severity(severity),
        "message": message,
        "metadata": {
            "source": SCANNER_SOURCE,
            "analyzer": "StaticAnalyzer",
            "severity": severity,
            **(metadata or {}),
        },
    }
    if file is not None:
        item["file"] = file
    if line is not None:
        item["line"] = line
    return item


def _level_from_severity(severity: str) -> str:
    """Map Cisco-style severity into skill-ledger levels."""
    sev = severity.lower()
    if sev in {"critical", "high"}:
        return "deny"
    if sev in {"medium", "low"}:
        return "warn"
    return "pass"


def _line_for_offset(text: str, offset: int) -> int:
    """Return a 1-based line number for an offset in *text*."""
    return text.count("\n", 0, offset) + 1


def _safe_excerpt(value: str, *, limit: int = 160) -> str:
    """Return a compact, single-line match excerpt."""
    excerpt = " ".join(value.split())
    if len(excerpt) <= limit:
        return excerpt
    return excerpt[: limit - 3] + "..."
