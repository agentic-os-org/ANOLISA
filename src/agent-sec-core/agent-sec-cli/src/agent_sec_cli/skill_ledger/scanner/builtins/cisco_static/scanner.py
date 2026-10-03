"""Static-only Skill scanner inspired by Cisco AI Defense skill-scanner.

This module intentionally implements only local static checks.  It does not
import cisco-ai-skill-scanner, YARA, LLM analyzers, remote services, or UI
dependencies.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path
from typing import Any

import yaml
from agent_sec_cli.skill_ledger.scanner.names import STATIC_SCANNER_NAME

SCANNER_NAME = STATIC_SCANNER_NAME
SCANNER_VERSION = "cisco-static-only-0.1.1"
SCANNER_SOURCE = "cisco-skill-scanner-static-only"

_SKILL_MANIFEST = "SKILL.md"
_DEFAULT_MAX_FILE_BYTES = 1_000_000
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


@dataclass(frozen=True)
class _TextFile:
    rel_path: str
    path: Path
    text: str
    is_code: bool


def scan_skill(
    skill_dir: str | Path,
    *,
    options: dict[str, Any] | None = None,
) -> list[dict[str, Any]]:
    """Scan a Skill directory and return ``NormalizedFinding`` dictionaries."""
    root = Path(skill_dir).resolve()
    opts = options or {}
    max_file_bytes = int(opts.get("maxFileBytes", _DEFAULT_MAX_FILE_BYTES))
    rules = _load_rules()
    findings: list[dict[str, Any]] = []

    skill_path = root / _SKILL_MANIFEST
    skill_text = _read_required_text(skill_path, _SKILL_MANIFEST, findings)
    front_matter: dict[str, Any] = {}
    body_text = skill_text
    if skill_text is not None:
        front_matter, body_text = _scan_skill_manifest(skill_text, findings)

    text_files: list[_TextFile] = []
    for path in _walk_skill_files(root, findings):
        rel_path = str(path.relative_to(root))
        _scan_path_metadata(path, rel_path, findings)
        text = _read_optional_text(path, rel_path, max_file_bytes, findings)
        if text is not None:
            text_files.append(
                _TextFile(
                    rel_path=rel_path,
                    path=path,
                    text=text,
                    is_code=_is_code_file(path, text),
                )
            )

    for rule in rules:
        try:
            if rule.target == "skill" and skill_text is not None:
                _apply_rule(rule, _SKILL_MANIFEST, body_text, findings)
            elif rule.target == "all_text":
                for text_file in text_files:
                    _apply_rule(rule, text_file.rel_path, text_file.text, findings)
            elif rule.target == "code":
                for text_file in text_files:
                    if text_file.is_code:
                        _apply_rule(rule, text_file.rel_path, text_file.text, findings)
        except Exception as exc:
            findings.append(
                _finding(
                    rule="scanner-rule-error",
                    severity="medium",
                    message=f"Static rule {rule.id!r} failed during scan: {exc}",
                    metadata={
                        "category": "scanner_error",
                        "title": "Static rule error",
                        "remediation": "Fix or disable the failing static rule.",
                    },
                )
            )

    _scan_undeclared_network(front_matter, text_files, findings)
    return findings


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


def _walk_skill_files(root: Path, findings: list[dict[str, Any]]) -> list[Path]:
    """Return sorted files under *root*, warning on symlink escapes."""
    files: list[Path] = []
    for entry in sorted(root.rglob("*")):
        rel = entry.relative_to(root)
        if _is_skipped(rel):
            continue
        if entry.is_symlink():
            _scan_symlink(root, entry, str(rel), findings)
            continue
        if entry.is_file():
            files.append(entry)
    return files


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
) -> str | None:
    """Read a required text file and create a warning finding on failure."""
    try:
        return path.read_text(encoding="utf-8")
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
    """Read a text-like file.  Binary or oversized files are skipped."""
    if path.suffix.lower() not in _TEXT_EXTENSIONS:
        return None
    try:
        raw = path.read_bytes()
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
        findings.append(
            _finding(
                rule="large-file-skipped",
                severity="medium",
                message="File exceeded static scanner size limit and was skipped.",
                file=rel_path,
                metadata={
                    "category": "scanner_limit",
                    "title": "Large file skipped",
                    "remediation": "Keep Skill files small enough for static review or raise the scanner limit.",
                    "maxFileBytes": max_file_bytes,
                },
            )
        )
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


def _scan_undeclared_network(
    front_matter: dict[str, Any],
    text_files: list[_TextFile],
    findings: list[dict[str, Any]],
) -> None:
    """Warn when network behavior appears without a metadata declaration."""
    declaration_text = " ".join(
        str(front_matter.get(key, ""))
        for key in ("description", "allowedTools", "allowed_tools", "capabilities")
    )
    if _NETWORK_DECLARATION_RE.search(declaration_text):
        return

    for text_file in text_files:
        if not text_file.is_code:
            continue
        network_hint = _find_network_hint(text_file.text)
        if network_hint is None:
            continue
        line_number, matched_text = network_hint
        findings.append(
            _finding(
                rule="undeclared-network-access",
                severity="medium",
                message="Skill helper content appears to use network access not declared in metadata.",
                file=text_file.rel_path,
                line=line_number,
                metadata={
                    "category": "network",
                    "title": "Undeclared network behavior",
                    "remediation": "Declare network behavior in SKILL.md metadata or remove the network call.",
                    "matchedText": _safe_excerpt(matched_text),
                },
            )
        )
        return


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


def _is_skipped(rel_path: Path) -> bool:
    """Return whether a relative path is under a skipped directory."""
    return any(part in _SKIP_DIRS for part in rel_path.parts)


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
