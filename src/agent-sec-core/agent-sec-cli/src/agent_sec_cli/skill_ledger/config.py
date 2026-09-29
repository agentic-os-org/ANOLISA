"""Configuration loading for skill-ledger (``~/.config/agent-sec/skill-ledger/config.json``)."""

import json
import logging
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_ledger.activation_policy import (
    ACTIVATION_POLICIES as ACTIVATION_POLICIES,
)
from agent_sec_cli.skill_ledger.activation_policy import (
    ACTIVATION_POLICY_PASS_WARN_ONLY as ACTIVATION_POLICY_PASS_WARN_ONLY,
)
from agent_sec_cli.skill_ledger.activation_policy import (
    DEFAULT_ACTIVATION_POLICY as DEFAULT_ACTIVATION_POLICY,
)
from agent_sec_cli.skill_ledger.activation_policy import (
    validate_activation_policy,
)
from agent_sec_cli.skill_ledger.errors import ConfigError
from agent_sec_cli.skill_ledger.path_identity import (
    normalize_canonical_skill_dir,
    validate_path_root_syntax,
)
from agent_sec_cli.skill_ledger.paths import (
    get_anolisa_skill_dir,
    get_config_dir,
)
from agent_sec_cli.skill_ledger.scanner.names import (
    CODE_SCANNER_NAME,
    STATIC_SCANNER_NAME,
    validate_scanner_name,
)

logger = logging.getLogger(__name__)

_SKILL_MANIFEST = "SKILL.md"
_DEPRECATED_SKILL_DIRS_KEY = "skillDirs"
DEFAULT_SYSTEM_SKILL_ROOTS = (
    Path("/usr/share/anolisa/skills"),
    Path("/usr/local/share/anolisa/skills"),
)
DEFAULT_SKILL_DIRS = [
    "~/.openclaw/skills/*",
    "~/.copilot-shell/skills/*",
    "~/.hermes/skills/**",
    "~/.qoder/skills/*",
    *(f"{root}/*" for root in DEFAULT_SYSTEM_SKILL_ROOTS),
]
_IGNORED_RECURSIVE_DIRS = frozenset(
    {".git", ".github", ".hub", ".archive", ".skill-meta"}
)

_DEFAULT_CONFIG: dict[str, Any] = {
    "signingBackend": "ed25519",
    "activationPolicy": DEFAULT_ACTIVATION_POLICY,
    "enableDefaultSkillDirs": True,
    "managedSkillDirs": [],
    # ── Scanner / parser registry (see design doc §2) ──
    "scanners": [
        {
            "name": "skill-vetter",
            "type": "skill",
            "parser": "findings-array",
            "description": "LLM-driven 4-phase skill audit",
        },
        {
            "name": CODE_SCANNER_NAME,
            "type": "builtin",
            "parser": "findings-array",
            "enabled": True,
            "description": "Scan Skill code files via code-scanner",
        },
        {
            "name": STATIC_SCANNER_NAME,
            "type": "builtin",
            "parser": "findings-array",
            "enabled": True,
            "description": "Static Skill security scanner based on Cisco skill-scanner rules",
        },
    ],
    "parsers": {
        "findings-array": {
            "type": "findings-array",
        },
    },
}


def config_path() -> Path:
    """Return the path to ``config.json``."""
    return get_config_dir() / "config.json"


def _deep_merge_config(
    defaults: dict[str, Any], user: dict[str, Any]
) -> dict[str, Any]:
    """Merge *user* config onto *defaults* with list-of-dict awareness.

    Rules:
    - ``managedSkillDirs`` (list[str]): user-managed discovery entries are
      stored separately from built-in defaults and are replaced by user config.
    - ``enableDefaultSkillDirs`` (bool): controls whether built-in default
      discovery entries participate in runtime resolution.
    - ``scanners`` (list[dict]): merge by ``name`` — user entries override
      defaults with the same ``name``; defaults not in user are preserved.
    - ``parsers`` (dict[str, dict]): shallow dict merge per parser name.
    - Other scalar / list top-level keys: user value wins outright.
    """
    merged = dict(defaults)
    for key, user_val in user.items():
        if key == "managedSkillDirs" and isinstance(user_val, list):
            entries = [str(v) for v in user_val]
            _validate_managed_skill_dir_entries(entries)
            merged["managedSkillDirs"] = _compact_skill_dirs(entries)
        elif key == "scanners" and isinstance(user_val, list):
            # Index defaults by name for O(1) lookup
            by_name: dict[str, dict[str, Any]] = {}
            for s in defaults.get("scanners", []):
                if isinstance(s, dict) and "name" in s:
                    canonical = validate_scanner_name(str(s["name"]))
                    by_name[canonical] = {**s, "name": canonical}
            # User entries override by name
            for s in user_val:
                if isinstance(s, dict) and "name" in s:
                    canonical = validate_scanner_name(str(s["name"]))
                    by_name[canonical] = {**s, "name": canonical}
            merged["scanners"] = list(by_name.values())
        elif key == "parsers" and isinstance(user_val, dict):
            merged_parsers = dict(defaults.get("parsers", {}))
            merged_parsers.update(user_val)
            merged["parsers"] = merged_parsers
        else:
            merged[key] = user_val
    return merged


def _validate_managed_skill_dir_entries(entries: list[str]) -> None:
    """Reject entries that could create an ambiguous canonical identity."""
    for entry in entries:
        base = entry
        if entry.endswith("/**"):
            base = entry[:-3]
        elif entry.endswith("/*"):
            base = entry[:-2]
        expanded = str(Path(base).expanduser())
        try:
            validate_path_root_syntax(
                expanded,
                subject=f"managedSkillDirs entry {entry!r}",
            )
        except ValueError as exc:
            raise ConfigError(str(exc)) from exc


def default_skill_dir_entries() -> list[str]:
    """Resolve built-in discovery entries in the current installation environment."""
    return [*DEFAULT_SKILL_DIRS, f"{get_anolisa_skill_dir()}/*"]


def effective_skill_dir_entries(config: dict[str, Any]) -> list[str]:
    """Return built-in plus managed skill directory entries for discovery."""
    entries: list[str] = []
    if config.get("enableDefaultSkillDirs", True):
        entries.extend(default_skill_dir_entries())
    entries.extend(str(v) for v in config.get("managedSkillDirs", []))
    return _compact_skill_dirs(entries)


def managed_skill_dir_entries(config: dict[str, Any]) -> list[str]:
    """Return only explicitly managed skill directory entries."""
    return _compact_skill_dirs([str(v) for v in config.get("managedSkillDirs", [])])


def deprecated_skill_dir_entries(config: dict[str, Any]) -> list[str]:
    """Return deprecated skillDirs entries retained only for diagnostics."""
    entries = config.get(_DEPRECATED_SKILL_DIRS_KEY)
    if isinstance(entries, list):
        return [str(v) for v in entries]
    return []


def load_config() -> dict[str, Any]:
    """Load and return the config file.  Returns defaults if the file does not exist."""
    path = config_path()
    if not path.is_file():
        return dict(_DEFAULT_CONFIG)
    try:
        raw = path.read_text(encoding="utf-8")
        cfg = json.loads(raw)
        if not isinstance(cfg, dict):
            raise ConfigError(
                f"config.json must be a JSON object, got {type(cfg).__name__}"
            )
        try:
            resolve_activation_policy(cfg)
        except ConfigError as exc:
            raise ConfigError(f"{path}: {exc.reason}") from exc
        scanners = cfg.get("scanners", [])
        for index, scanner in enumerate(scanners if isinstance(scanners, list) else []):
            if isinstance(scanner, dict) and "name" in scanner:
                try:
                    validate_scanner_name(str(scanner["name"]))
                except ValueError as exc:
                    raise ConfigError(f"{path}: scanners[{index}].name: {exc}") from exc
        if _DEPRECATED_SKILL_DIRS_KEY in cfg:
            logger.warning(
                "Ignoring deprecated skill-ledger config key %r in %s; use "
                "managedSkillDirs instead. Set enableDefaultSkillDirs=false "
                "for isolated discovery.",
                _DEPRECATED_SKILL_DIRS_KEY,
                path,
            )
        return _deep_merge_config(_DEFAULT_CONFIG, cfg)
    except json.JSONDecodeError as exc:
        raise ConfigError(f"Invalid JSON in {path}: {exc}") from exc


def resolve_activation_policy(config: dict[str, Any] | None = None) -> str:
    """Return the configured activation policy."""
    if config is None:
        config = load_config()
    policy = config.get("activationPolicy", DEFAULT_ACTIVATION_POLICY)
    try:
        return validate_activation_policy(policy)
    except ValueError as exc:
        raise ConfigError(f"Invalid activationPolicy: {exc}") from exc


def resolve_skill_dirs(config: dict[str, Any] | None = None) -> list[Path]:
    """Expand effective skill dir entries into concrete directories.

    Supports three formats per entry:
    - ``"path/*"`` — glob pattern: each matching subdirectory **that contains
      SKILL.md** is included.
    - ``"path/**"`` — recursive pattern: every descendant directory containing
      SKILL.md is included, with hidden/internal metadata dirs skipped.
    - ``"path/to/skill"`` — single skill directory; must also contain
      ``SKILL.md`` to be included.

    Non-existent directories are silently skipped.  Duplicates (by resolved
    path) are removed while preserving discovery order.
    """
    if config is None:
        config = load_config()
    return _resolve_skill_dir_entries(effective_skill_dir_entries(config))


def resolve_managed_skill_dirs(config: dict[str, Any] | None = None) -> list[Path]:
    """Expand only explicitly managed canonical skill directories."""
    if config is None:
        config = load_config()
    return _resolve_skill_dir_entries(managed_skill_dir_entries(config))


def _resolve_skill_dir_entries(entries: list[str]) -> list[Path]:
    """Expand skill directory entries into concrete directories."""
    skill_dirs: list[Path] = []
    seen: set[Path] = set()

    for entry in entries:
        entry = str(entry)
        expanded = Path(entry).expanduser()

        if entry.endswith("/**"):
            parent = expanded.parent
            if parent.is_dir():
                for skill_file in sorted(parent.rglob(_SKILL_MANIFEST)):
                    skill_dir = skill_file.parent
                    if _is_ignored_recursive_skill_dir(skill_dir, parent):
                        continue
                    resolved = _lexical_path(skill_dir)
                    if resolved not in seen:
                        seen.add(resolved)
                        skill_dirs.append(skill_dir)
        elif entry.endswith("/*"):
            # Glob mode: parent directory, each child with SKILL.md is a skill
            parent = expanded.parent
            if parent.is_dir():
                for child in sorted(parent.iterdir()):
                    if (
                        child.is_dir()
                        and not child.name.startswith(".")
                        and (child / _SKILL_MANIFEST).is_file()
                    ):
                        resolved = _lexical_path(child)
                        if resolved not in seen:
                            seen.add(resolved)
                            skill_dirs.append(child)
        else:
            # Single directory — still requires SKILL.md
            if expanded.is_dir() and (expanded / _SKILL_MANIFEST).is_file():
                resolved = _lexical_path(expanded)
                if resolved not in seen:
                    seen.add(resolved)
                    skill_dirs.append(expanded)

    return skill_dirs


# ---------------------------------------------------------------------------
# Auto-remember: append unknown skill dirs on check/certify
# ---------------------------------------------------------------------------


def _compact_skill_dirs(entries: list[str]) -> list[str]:
    """Remove entries that are subsumed by a glob in the same list.

    A specific path ``parent/X`` is redundant when ``parent/*`` also appears.
    Preserves order; keeps the glob, drops the specifics.
    """
    glob_parents: set[str] = set()
    recursive_parents: set[Path] = set()
    for entry in entries:
        if entry.endswith("/**"):
            recursive_parents.add(_lexical_path(Path(entry[:-3]).expanduser()))
        elif entry.endswith("/*"):
            # Normalise: resolve ~ so "/home/user/.copilot-shell/skills/*"
            # and "~/.copilot-shell/skills/*" are treated as the same parent.
            glob_parents.add(str(_lexical_path(Path(entry[:-2]).expanduser())))

    compacted: list[str] = []
    seen: set[str] = set()
    for entry in entries:
        if entry in seen:
            continue
        seen.add(entry)

        # Skip specific paths whose parent is covered by a glob
        if not entry.endswith(("/*", "/**")):
            expanded = _lexical_path(Path(entry).expanduser())
            parent_str = str(expanded.parent)
            if parent_str in glob_parents:
                continue
            if any(expanded.is_relative_to(parent) for parent in recursive_parents):
                continue

        compacted.append(entry)
    return compacted


def _is_ignored_recursive_skill_dir(skill_dir: Path, root: Path) -> bool:
    """Return True when *skill_dir* is under a hidden/internal subtree."""
    try:
        parts = skill_dir.relative_to(root).parts
    except ValueError:
        return True
    return any(
        part.startswith(".") or part in _IGNORED_RECURSIVE_DIRS for part in parts
    )


def is_covered(skill_dir: Path, config: dict[str, Any] | None = None) -> bool:
    """Return whether current config lexically covers the canonical path."""
    if config is None:
        config = load_config()
    return _is_path_covered_by_entries(
        skill_dir,
        effective_skill_dir_entries(config),
    )


def is_managed_covered(skill_dir: Path, config: dict[str, Any] | None = None) -> bool:
    """Return ``True`` if *skill_dir* is explicitly covered by managedSkillDirs."""
    if config is None:
        config = load_config()
    return _is_path_covered_by_entries(
        skill_dir,
        managed_skill_dir_entries(config),
    )


def is_default_system_skill_dir(skill_dir: str | Path) -> bool:
    """Return whether *skill_dir* is an immediate child of a packaged system root."""
    canonical_dir = _lexical_path(Path(skill_dir).expanduser())
    return canonical_dir.parent in DEFAULT_SYSTEM_SKILL_ROOTS


def is_default_user_skill_dir(skill_dir: str | Path) -> bool:
    """Return whether *skill_dir* is an immediate child of the raw user root."""
    canonical_dir = _lexical_path(Path(skill_dir).expanduser())
    return canonical_dir.parent == get_anolisa_skill_dir()


def _is_path_covered_by_entries(skill_dir: Path, entries: list[str]) -> bool:
    """Match a canonical path against config entries without filesystem access."""
    target = _lexical_path(skill_dir)
    for entry in entries:
        if entry.endswith("/**"):
            root = _lexical_path(Path(entry[:-3]).expanduser())
            if target.is_relative_to(root):
                return True
            continue
        if entry.endswith("/*"):
            parent = _lexical_path(Path(entry[:-2]).expanduser())
            if target.parent == parent:
                return True
            continue
        if target == _lexical_path(Path(entry).expanduser()):
            return True
    return False


def _lexical_path(path: Path) -> Path:
    """Normalize a path without following symlinks or requiring it to exist."""
    return normalize_canonical_skill_dir(path)


def remember_skill_dir(
    skill_dir: Path, config: dict[str, Any] | None = None
) -> str | None:
    """Append *skill_dir* (or its parent glob) to ``managedSkillDirs`` if not covered.

    Heuristic for entry format:
    - Raw user default Skills keep individual paths so auto-remember does not
      turn read-only siblings into explicitly managed Skills.
    - If the parent directory contains **at least two** sibling sub-directories
      that each contain ``SKILL.md``, other roots use ``"parent/*"``.
    - Otherwise, add the specific directory path.

    After appending, runs :func:`_compact_skill_dirs` to prune entries that
    are now subsumed by the new (or existing) glob.

    Returns the entry string that was added, or ``None`` if already covered.
    """
    if config is None:
        config = load_config()

    skill_dir = _lexical_path(skill_dir.expanduser())

    if is_managed_covered(skill_dir, config):
        return None

    parent = skill_dir.parent
    try:
        sibling_skills = [
            d
            for d in parent.iterdir()
            if d.is_dir()
            and not d.name.startswith(".")
            and (d / _SKILL_MANIFEST).is_file()
        ]
    except OSError:
        sibling_skills = []

    if not is_default_user_skill_dir(skill_dir) and len(sibling_skills) >= 2:
        entry = str(parent) + "/*"
    else:
        entry = str(skill_dir)

    existing = list(config.get("managedSkillDirs", []))
    if entry not in existing:
        existing.append(entry)
    config["managedSkillDirs"] = _compact_skill_dirs(existing)
    save_config(config)
    logger.info("Added %r to managedSkillDirs in %s", entry, config_path())

    return entry


def save_config(config: dict[str, Any]) -> Path:
    """Write *config* to ``config.json``.  Creates parent dirs if needed."""
    path = config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(config, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    return path
