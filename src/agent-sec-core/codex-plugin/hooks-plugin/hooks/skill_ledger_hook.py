#!/usr/bin/env python3
"""Codex UserPromptSubmit hook for skill-ledger.

Reads a Codex UserPromptSubmit JSON from stdin, parses ``$skill-name``
mentions from the user prompt, resolves each to an installed skill directory,
invokes ``agent-sec-cli skill-ledger check`` for each, and writes a Codex
HookOutput JSON to stdout.

Hook point: **UserPromptSubmit** (no matcher — fires on every prompt)

Policy (controlled by SKILL_LEDGER_MODE, default: ask):
  - observe: silent pass-through, only audit trail via agent-sec-cli events.
            Even if integrity check fails, it will NOT be blocked.
  - warn: show a warning and continue.
  - ask: fall back to warn because this Codex hook cannot request confirmation.
  - block: block the entire turn when any skill fails integrity check.

The compatibility values debug and deny map to observe and block, respectively.

Input schema::

    {
        "session_id": "thread_xxx",
        "turn_id": "turn_xxx",
        "cwd": "/current/working/directory",
        "hook_event_name": "UserPromptSubmit",
        "model": "...",
        "permission_mode": "default",
        "prompt": "$my-skill 帮我重构代码"
    }

Output mapping (policy=block):

    all skills pass          → (empty stdout — allow)
    any skill status in BLOCK_STATUSES → {"decision": "block", "reason": "..."}

Output mapping (policy=observe):

    always                   → (empty stdout — allow, audit only)

Usage::

    python3 skill_ledger_hook.py          # reads stdin, writes stdout

This script is intentionally self-contained — it does NOT import any
``agent_sec_cli`` package.  All it needs is the standard library and the
``agent-sec-cli`` binary on $PATH.
"""

import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

from hook_config import env_flag_enabled, env_hook_policy, normalize_hook_policy
from trace_context import with_trace_context

# -- config ----------------------------------------------------------------

_HOOK_ENABLED = env_flag_enabled("SKILL_LEDGER_HOOK_ENABLED", True)
MODE = os.environ.get("SKILL_LEDGER_MODE")


def _read_policy() -> str:
    """Read the configured Skill Ledger mode."""
    raw = os.environ.get("SKILL_LEDGER_MODE")
    policy = env_hook_policy("SKILL_LEDGER_MODE", "ask")
    if "SKILL_LEDGER_MODE" in os.environ and normalize_hook_policy(raw, "") == "":
        print("[skill-ledger] invalid SKILL_LEDGER_MODE; using ask", file=sys.stderr)
    return policy


_POLICY = _read_policy()


def _effective_policy() -> str:
    """Return policy while preserving module-level test overrides."""
    return normalize_hook_policy(MODE, _POLICY)


try:
    TIMEOUT = int(os.environ.get("SKILL_LEDGER_TIMEOUT", "5"))
except (ValueError, TypeError):
    TIMEOUT = 5
_INIT_TIMEOUT = 3  # seconds for key initialization

# -- constants -------------------------------------------------------------

# Statuses that should trigger block in deny mode.
# Mirrors agent-sec-cli skill-ledger check output status values.
_BLOCK_STATUSES = frozenset({"none", "drifted", "warn", "deny", "tampered"})

# Regex to extract $skill-name mentions from prompt.
# Matches: $ followed by letter, then [a-zA-Z0-9_:-]*
# Consistent with Codex's `extract_tool_mentions_with_sigil` in injection.rs.
_MENTION_RE = re.compile(r"\$([a-zA-Z][a-zA-Z0-9_:\-]*)")

# Common environment variable names to exclude from skill matching.
# Prevents $PATH, $HOME etc. from being treated as skill mentions.
_COMMON_ENV_VARS = frozenset(
    {
        "PATH",
        "HOME",
        "USER",
        "SHELL",
        "PWD",
        "TMPDIR",
        "TEMP",
        "TMP",
        "LANG",
        "TERM",
        "EDITOR",
        "VISUAL",
        "PAGER",
        "DISPLAY",
        "HOSTNAME",
        "LOGNAME",
        "MAIL",
        "OLDPWD",
        "SHLVL",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "CODEX_HOME",
    }
)

_STATUS_LABELS = {
    "none": "从未扫描",
    "drifted": "文件内容已变更",
    "warn": "扫描有低风险发现",
    "deny": "扫描有高风险发现",
    "tampered": "签名验证失败",
}


# -- skill directory resolution --------------------------------------------

# Markers used to detect project root (mirrors Codex's default_project_root_markers).
_PROJECT_ROOT_MARKERS = frozenset(
    {
        ".git",
        ".hg",
        ".svn",
        "package.json",
        "Cargo.toml",
        "pyproject.toml",
        "go.mod",
        "Makefile",
        "CMakeLists.txt",
    }
)


def _find_project_root(cwd: Path) -> Path:
    """Walk up from cwd to find project root via marker files."""
    current = cwd.resolve()
    for ancestor in [current, *current.parents]:
        if any((ancestor / marker).exists() for marker in _PROJECT_ROOT_MARKERS):
            return ancestor
    return current  # fallback to cwd if no marker found


def _skill_roots(cwd: str) -> list[Path]:
    """Return known Codex skill root directories.

    Mirrors Codex's skill_roots logic (loader.rs):
    - Repo level: every .agents/skills/ from project root down to cwd
    - User level: $CODEX_HOME/skills/ and $HOME/.agents/skills/
    - System level: /etc/codex/skills/
    """
    cwd_path = Path(cwd).resolve()
    project_root = _find_project_root(cwd_path)
    codex_home = Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex")))

    roots: list[Path] = []

    # Repo-level: project_root -> ... -> cwd, each .agents/skills/
    if cwd_path.is_relative_to(project_root):
        rel = cwd_path.relative_to(project_root)
        dirs = [project_root] + [
            project_root / Path(*rel.parts[: i + 1]) for i in range(len(rel.parts))
        ]
        for d in dirs:
            candidate = d / ".agents" / "skills"
            if candidate.is_dir():
                roots.append(candidate)
    else:
        candidate = cwd_path / ".agents" / "skills"
        if candidate.is_dir():
            roots.append(candidate)

    # User-level
    roots.append(codex_home / "skills")
    roots.append(Path.home() / ".agents" / "skills")

    # System-level
    roots.append(Path("/etc/codex/skills"))

    return roots


def _parse_skill_name(skill_md_path: Path) -> str | None:
    """Extract 'name' from SKILL.md YAML frontmatter. Returns None on failure."""
    try:
        text = skill_md_path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        return None
    frontmatter_lines: list[str] = []
    for line in lines[1:]:
        if line.strip() == "---":
            break
        frontmatter_lines.append(line)
    else:
        return None  # no closing ---
    # Simple YAML parse for 'name: xxx' (avoid importing yaml)
    for line in frontmatter_lines:
        if line.startswith("name:"):
            value = line[5:].strip().strip('"').strip("'")
            return value if value else None
    return None


def _build_skill_catalog(cwd: str) -> dict[str, list[str]]:
    """Build {canonical_name: [dir_path, ...]} catalog from all skill roots.

    Scans each root's immediate subdirectories for SKILL.md, reads the
    frontmatter name field. Falls back to directory name if no name field.
    A name may map to multiple directories (e.g. same skill installed in
    both repo-level and user-level roots).

    Security: after resolving symlinks, verifies the resolved path is still
    within the skill root boundary (prevents path traversal via symlinks).
    """
    catalog: dict[str, list[str]] = {}
    for root in _skill_roots(cwd):
        if not root.is_dir():
            continue
        try:
            resolved_root = root.resolve()
        except (OSError, ValueError):
            continue
        try:
            entries = sorted(root.iterdir())
        except OSError:
            continue
        for entry in entries:
            if not entry.is_dir():
                continue
            try:
                resolved_entry = entry.resolve()
            except (OSError, ValueError):
                continue
            # Path traversal check: resolved entry must stay within root
            if not resolved_entry.is_relative_to(resolved_root):
                continue
            skill_md = resolved_entry / "SKILL.md"
            if not skill_md.is_file():
                continue
            name = _parse_skill_name(skill_md) or entry.name
            catalog.setdefault(name, []).append(str(resolved_entry))
    return catalog


def _resolve_skill_dir(skill_name: str, catalog: dict[str, list[str]]) -> str | None:
    """Resolve a skill name using the pre-built catalog.

    Returns the directory path only when the name uniquely maps to a single
    directory.  Codex treats a plain $skill-name as ambiguous when the same
    name exists in multiple roots, so we fail-open in that case to avoid
    checking a directory that Codex might not actually inject.
    """
    dirs = catalog.get(skill_name)
    if not dirs:
        return None
    if len(dirs) == 1:
        return dirs[0]
    # Ambiguous: same name in multiple roots – fail-open with warning
    print(
        f"[skill-ledger] ⚠ skill '{skill_name}' found in {len(dirs)} "
        f"roots, skipping check (ambiguous)",
        file=sys.stderr,
    )
    return None


# -- prompt parsing --------------------------------------------------------


def _extract_skill_mentions(prompt: str) -> list[str]:
    """Extract $skill-name mentions from prompt, excluding env vars.

    Returns deduplicated list of potential skill names (without $ prefix).
    """
    seen: set[str] = set()
    result: list[str] = []
    for match in _MENTION_RE.findall(prompt):
        if match.upper() in _COMMON_ENV_VARS:
            continue
        if match not in seen:
            seen.add(match)
            result.append(match)
    return result


# -- key management --------------------------------------------------------


def _keys_exist() -> bool:
    """Return True if both key.pub and key.enc exist."""
    xdg_data = os.environ.get("XDG_DATA_HOME", "")
    if not xdg_data:
        xdg_data = str(Path.home() / ".local" / "share")
    data_dir = Path(xdg_data) / "agent-sec" / "skill-ledger"
    return (data_dir / "key.pub").is_file() and (data_dir / "key.enc").is_file()


def _ensure_keys(input_data: dict[str, Any]) -> None:
    """Auto-initialize signing keys if missing (fire-and-forget)."""
    if _keys_exist():
        return
    try:
        cmd = with_trace_context(
            ["agent-sec-cli", "skill-ledger", "init", "--no-baseline"],
            input_data,
        )
        subprocess.run(
            cmd,
            capture_output=True,
            check=False,
            text=True,
            timeout=_INIT_TIMEOUT,
        )
    except Exception:
        pass


# -- output helpers --------------------------------------------------------


def _block(failed_skills: list[tuple[str, str]]) -> None:
    """Output block decision to reject the turn (mode=deny).

    Args:
        failed_skills: list of (skill_name, status) tuples that failed check.
    """
    lines = ["[skill-ledger] ⛔ 技能完整性校验失败："]
    for name, status in failed_skills:
        label = _STATUS_LABELS.get(status, f"状态异常({status})")
        lines.append(f"  - {name}: {label}")
    lines.append("")
    lines.append("请运行以下命令重新认证后重试：")
    for name, _ in failed_skills:
        lines.append(f"  agent-sec-cli skill-ledger scan <{name}_dir>")
    msg = "\n".join(lines)
    print(json.dumps({"decision": "block", "reason": msg}, ensure_ascii=False))


def _warn(failed_skills: list[tuple[str, str]]) -> None:
    """Warn without claiming Codex can request confirmation at this hook."""
    details = ", ".join(
        f"{name}: {_STATUS_LABELS.get(status, status)}"
        for name, status in failed_skills
    )
    message = f"[skill-ledger] Skill review required; execution continues: {details}"
    print(json.dumps({"systemMessage": message}, ensure_ascii=False))


# -- main ------------------------------------------------------------------


def main() -> None:
    if not _HOOK_ENABLED:
        return

    # 1. Read stdin JSON (fail-open: empty stdout = allow in Codex)
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError, ValueError):
        return

    # 2. Extract user prompt text
    prompt = input_data.get("prompt", "")
    if not prompt or not isinstance(prompt, str) or not prompt.strip():
        return  # nothing to check, allow

    # 3. Parse $skill-name mentions from prompt
    mentions = _extract_skill_mentions(prompt)
    if not mentions:
        return  # no skill mentions, allow

    # 4. Resolve mentions to installed skill directories via catalog
    cwd = input_data.get("cwd", ".")
    catalog = _build_skill_catalog(cwd)
    skills_to_check: list[tuple[str, str]] = []  # (name, dir_path)
    for skill_name in mentions:
        skill_dir = _resolve_skill_dir(skill_name, catalog)
        if skill_dir is not None:
            skills_to_check.append((skill_name, skill_dir))

    if not skills_to_check:
        return  # no matching installed skills, allow

    # 5. Ensure signing keys exist (auto-init if missing)
    _ensure_keys(input_data)

    # 6. Check each skill via agent-sec-cli
    failed: list[tuple[str, str]] = []  # (name, status)
    for skill_name, skill_dir in skills_to_check:
        try:
            cmd = with_trace_context(
                ["agent-sec-cli", "skill-ledger", "check", skill_dir],
                input_data,
            )
            proc = subprocess.run(
                cmd,
                capture_output=True,
                check=False,
                text=True,
                timeout=TIMEOUT,
            )
        except Exception:
            continue  # fail-open on subprocess error

        try:
            check_result = json.loads(proc.stdout)
        except (json.JSONDecodeError, ValueError):
            continue  # fail-open on parse error

        status = check_result.get("status", "unknown")
        if status in _BLOCK_STATUSES:
            failed.append((skill_name, status))

    if not failed:
        return  # all passed or no checks needed, allow

    # 7. Mode-based output
    # - observe: 不拦截，仅通过 agent-sec-cli events 审计
    # - deny: status 异常时拦截整个 turn（skill 内容不会注入模型上下文）
    policy = _effective_policy()
    if policy == "observe":
        return  # observe 模式：不拦截
    if policy == "block":
        _block(failed)
        return
    _warn(failed)  # Codex cannot request approval here; ask falls back to warn.


if __name__ == "__main__":
    main()
