"""Unit tests for codex-plugin/hooks/skill_ledger_hook.py.

Coverage targets:
  - Fail-open paths (invalid JSON, empty prompt, no skill mentions)
  - Skill mention parsing ($name extraction, env var exclusion)
  - Skill directory resolution (multi-root lookup)
  - Key auto-initialization
  - Mode-based decisions (observe vs deny)
  - Block output formatting
  - Status → block mapping
  - Trace context injection
"""

import io
import json
import os
import stat
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest
from standalone_hook_test_loader import load_standalone_hook

# ---------------------------------------------------------------------------
# Hook path & module import
# ---------------------------------------------------------------------------

_HOOKS_DIR = str(
    Path(__file__).resolve().parents[2]
    / ".."
    / "codex-plugin"
    / "hooks-plugin"
    / "hooks"
)
skill_ledger_hook = load_standalone_hook(
    "codex_skill_ledger_hook",
    Path(_HOOKS_DIR) / "skill_ledger_hook.py",
)

_HOOK_SCRIPT = os.path.join(_HOOKS_DIR, "skill_ledger_hook.py")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run_hook(input_data, *, env_override=None):
    """Run skill_ledger_hook.py as subprocess and return parsed JSON output."""
    env = os.environ.copy()
    if env_override:
        env.update(env_override)
    stdin_text = json.dumps(input_data) if isinstance(input_data, dict) else input_data
    proc = subprocess.run(
        [sys.executable, _HOOK_SCRIPT],
        input=stdin_text,
        capture_output=True,
        check=False,
        text=True,
        timeout=15,
        env=env,
    )
    assert proc.returncode == 0, f"Hook crashed: stderr={proc.stderr}"
    if not proc.stdout.strip():
        return {}
    return json.loads(proc.stdout)


_MOCK_CLI_SCRIPT = f"#!{sys.executable}\n" + textwrap.dedent("""\
    import os, sys
    output = os.environ.get("_MOCK_CLI_OUTPUT", "")
    rc = int(os.environ.get("_MOCK_CLI_RC", "0"))
    if output:
        print(output)
    sys.exit(rc)
""")


@pytest.fixture()
def mock_cli(tmp_path):
    """Create a mock agent-sec-cli that returns canned responses via env vars."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    cli_script = bin_dir / "agent-sec-cli"
    cli_script.write_text(_MOCK_CLI_SCRIPT)
    cli_script.chmod(cli_script.stat().st_mode | stat.S_IEXEC)

    def _make_env(output: str = "", *, rc: int = 0, extra: dict | None = None):
        env = {
            "PATH": str(bin_dir) + os.pathsep + os.environ.get("PATH", ""),
            "_MOCK_CLI_OUTPUT": output,
            "_MOCK_CLI_RC": str(rc),
        }
        if extra:
            env.update(extra)
        return env

    return _make_env


# ---------------------------------------------------------------------------
# Helper data
# ---------------------------------------------------------------------------

_PASS_CHECK = json.dumps({"status": "pass"})
_DRIFTED_CHECK = json.dumps({"status": "drifted", "modified": ["SKILL.md"]})
_NONE_CHECK = json.dumps({"status": "none"})
_TAMPERED_CHECK = json.dumps({"status": "tampered"})


def _make_skill_dir(parent, name="test-skill"):
    """Create a minimal skill dir with SKILL.md."""
    skill_dir = Path(parent) / name
    skill_dir.mkdir(parents=True, exist_ok=True)
    (skill_dir / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: Test\n---\n# {name}\n"
    )
    return str(skill_dir)


# ---------------------------------------------------------------------------
# Subprocess-based (black-box) tests
# ---------------------------------------------------------------------------


class TestFailOpen:
    """Every error must produce empty stdout (= allow)."""

    def test_hook_disabled_short_circuits_before_work(self, monkeypatch, capsys):
        monkeypatch.setattr(skill_ledger_hook, "_HOOK_ENABLED", False)
        monkeypatch.setattr(
            skill_ledger_hook.json,
            "load",
            lambda _stream: pytest.fail("input should not be read"),
        )
        monkeypatch.setattr(
            skill_ledger_hook,
            "_extract_skill_mentions",
            lambda *_args: pytest.fail("skills should not be parsed"),
        )
        monkeypatch.setattr(
            skill_ledger_hook,
            "_ensure_keys",
            lambda *_args: pytest.fail("keys should not be initialized"),
        )
        monkeypatch.setattr(
            skill_ledger_hook.subprocess,
            "run",
            lambda *_args, **_kwargs: pytest.fail("CLI should not be called"),
        )

        skill_ledger_hook.main()

        assert capsys.readouterr().out == ""

    def test_invalid_json_allows(self):
        output = _run_hook("not-json")
        assert output == {}

    def test_empty_stdin_allows(self):
        output = _run_hook("")
        assert output == {}

    def test_empty_prompt_allows(self, mock_cli):
        env = mock_cli(output=_DRIFTED_CHECK, extra={"SKILL_LEDGER_MODE": "deny"})
        output = _run_hook(
            {"hook_event_name": "UserPromptSubmit", "prompt": "", "cwd": "/tmp"},
            env_override=env,
        )
        assert output == {}

    def test_whitespace_prompt_allows(self, mock_cli):
        env = mock_cli(output=_DRIFTED_CHECK, extra={"SKILL_LEDGER_MODE": "deny"})
        output = _run_hook(
            {"hook_event_name": "UserPromptSubmit", "prompt": "   ", "cwd": "/tmp"},
            env_override=env,
        )
        assert output == {}

    def test_no_skill_mentions_allows(self, mock_cli):
        env = mock_cli(output=_DRIFTED_CHECK, extra={"SKILL_LEDGER_MODE": "deny"})
        output = _run_hook(
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "hello world no skills here",
                "cwd": "/tmp",
            },
            env_override=env,
        )
        assert output == {}

    def test_only_env_var_mentions_allows(self, mock_cli):
        env = mock_cli(output=_DRIFTED_CHECK, extra={"SKILL_LEDGER_MODE": "deny"})
        output = _run_hook(
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "echo $HOME $PATH",
                "cwd": "/tmp",
            },
            env_override=env,
        )
        assert output == {}

    def test_unresolvable_skill_allows(self, mock_cli):
        env = mock_cli(output=_DRIFTED_CHECK, extra={"SKILL_LEDGER_MODE": "deny"})
        output = _run_hook(
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "$nonexistent-skill-xyz hello",
                "cwd": "/tmp",
            },
            env_override=env,
        )
        assert output == {}


# ---------------------------------------------------------------------------
# Skill mention parsing tests
# ---------------------------------------------------------------------------


class TestSkillMentionParsing:
    """Test _extract_skill_mentions."""

    def test_single_mention(self):
        result = skill_ledger_hook._extract_skill_mentions("$my-skill hello")
        assert result == ["my-skill"]

    def test_multiple_mentions(self):
        result = skill_ledger_hook._extract_skill_mentions(
            "$skill-a and $skill-b please"
        )
        assert result == ["skill-a", "skill-b"]

    def test_deduplication(self):
        result = skill_ledger_hook._extract_skill_mentions(
            "$my-skill $my-skill $my-skill"
        )
        assert result == ["my-skill"]

    def test_env_vars_excluded(self):
        result = skill_ledger_hook._extract_skill_mentions("$PATH $HOME $my-skill")
        assert result == ["my-skill"]

    def test_codex_home_excluded(self):
        result = skill_ledger_hook._extract_skill_mentions("$CODEX_HOME/skills")
        assert result == []

    def test_case_insensitive_env_exclusion(self):
        """Env var exclusion uses .upper() so $path should also be excluded."""
        result = skill_ledger_hook._extract_skill_mentions("$path")
        # "path" -> "PATH" which is in _COMMON_ENV_VARS
        assert result == []

    def test_must_start_with_letter(self):
        result = skill_ledger_hook._extract_skill_mentions("$123 $-bad")
        assert result == []

    def test_colon_in_name(self):
        result = skill_ledger_hook._extract_skill_mentions("$org:my-skill")
        assert result == ["org:my-skill"]

    def test_no_mentions(self):
        result = skill_ledger_hook._extract_skill_mentions("no skills here at all")
        assert result == []


# ---------------------------------------------------------------------------
# Skill directory resolution tests
# ---------------------------------------------------------------------------


class TestSkillDirResolution:
    """Test _build_skill_catalog + _resolve_skill_dir."""

    def test_finds_skill_in_codex_home(self, tmp_path, monkeypatch):
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "test-skill")
        monkeypatch.setenv("CODEX_HOME", str(codex_home))

        catalog = skill_ledger_hook._build_skill_catalog(str(tmp_path))
        result = skill_ledger_hook._resolve_skill_dir("test-skill", catalog)
        assert result is not None
        assert "test-skill" in result

    def test_finds_skill_in_project_agents(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        # Place a marker so project is recognized as project root
        (project / ".git").mkdir()
        skills_dir = project / ".agents" / "skills"
        _make_skill_dir(skills_dir, "proj-skill")

        catalog = skill_ledger_hook._build_skill_catalog(str(project))
        result = skill_ledger_hook._resolve_skill_dir("proj-skill", catalog)
        assert result is not None
        assert "proj-skill" in result

    def test_returns_none_for_missing_skill(self, tmp_path, monkeypatch):
        monkeypatch.setenv("CODEX_HOME", str(tmp_path / ".codex"))
        catalog = skill_ledger_hook._build_skill_catalog(str(tmp_path))
        result = skill_ledger_hook._resolve_skill_dir("nonexistent", catalog)
        assert result is None

    def test_requires_skill_md(self, tmp_path, monkeypatch):
        """Directory exists but no SKILL.md → not resolved."""
        codex_home = tmp_path / ".codex"
        skill_dir = codex_home / "skills" / "no-skill-md"
        skill_dir.mkdir(parents=True)
        monkeypatch.setenv("CODEX_HOME", str(codex_home))

        catalog = skill_ledger_hook._build_skill_catalog(str(tmp_path))
        result = skill_ledger_hook._resolve_skill_dir("no-skill-md", catalog)
        assert result is None


# ---------------------------------------------------------------------------
# Project root detection tests
# ---------------------------------------------------------------------------


class TestProjectRootDetection:
    """Test _find_project_root."""

    def test_finds_git_marker(self, tmp_path):
        project = tmp_path / "repo"
        project.mkdir()
        (project / ".git").mkdir()
        sub = project / "a" / "b"
        sub.mkdir(parents=True)

        result = skill_ledger_hook._find_project_root(sub)
        assert result == project

    def test_finds_pyproject_marker(self, tmp_path):
        project = tmp_path / "pyproj"
        project.mkdir()
        (project / "pyproject.toml").write_text("[project]")
        sub = project / "src" / "pkg"
        sub.mkdir(parents=True)

        result = skill_ledger_hook._find_project_root(sub)
        assert result == project

    def test_fallback_to_cwd_if_no_marker(self, tmp_path):
        bare = tmp_path / "bare"
        bare.mkdir()
        result = skill_ledger_hook._find_project_root(bare)
        assert result == bare.resolve()

    def test_nearest_marker_wins(self, tmp_path):
        """When multiple markers exist, the nearest ancestor wins."""
        outer = tmp_path / "outer"
        outer.mkdir()
        (outer / ".git").mkdir()
        inner = outer / "inner"
        inner.mkdir()
        (inner / "Cargo.toml").write_text("[package]")
        deep = inner / "src"
        deep.mkdir()

        # Should find inner (Cargo.toml) not outer (.git) when starting from deep
        # Actually _find_project_root walks up from cwd, first match wins
        # deep -> inner (has Cargo.toml) -> found!
        result = skill_ledger_hook._find_project_root(deep)
        assert result == inner


# ---------------------------------------------------------------------------
# Skill roots walk-up tests
# ---------------------------------------------------------------------------


class TestSkillRootsWalkUp:
    """Test _skill_roots walks up from cwd to project root."""

    def test_finds_parent_agents_skills(self, tmp_path, monkeypatch):
        """Skills in project root .agents/skills/ are found from subdir."""
        project = tmp_path / "proj"
        project.mkdir()
        (project / ".git").mkdir()
        skills_root = project / ".agents" / "skills"
        skills_root.mkdir(parents=True)

        sub = project / "src" / "app"
        sub.mkdir(parents=True)
        monkeypatch.setenv("CODEX_HOME", str(tmp_path / "no-codex-home"))

        roots = skill_ledger_hook._skill_roots(str(sub))
        assert skills_root in roots

    def test_finds_skills_at_multiple_levels(self, tmp_path, monkeypatch):
        """Skills dirs at both project root and intermediate dir are found."""
        project = tmp_path / "proj"
        project.mkdir()
        (project / ".git").mkdir()
        root_skills = project / ".agents" / "skills"
        root_skills.mkdir(parents=True)
        mid_skills = project / "sub" / ".agents" / "skills"
        mid_skills.mkdir(parents=True)

        cwd = project / "sub" / "deep"
        cwd.mkdir(parents=True)
        monkeypatch.setenv("CODEX_HOME", str(tmp_path / "no-codex-home"))

        roots = skill_ledger_hook._skill_roots(str(cwd))
        assert root_skills in roots
        assert mid_skills in roots

    def test_codex_home_included(self, tmp_path, monkeypatch):
        codex_home = tmp_path / "my-codex"
        monkeypatch.setenv("CODEX_HOME", str(codex_home))
        roots = skill_ledger_hook._skill_roots(str(tmp_path))
        assert (codex_home / "skills") in roots


# ---------------------------------------------------------------------------
# Frontmatter name parsing tests
# ---------------------------------------------------------------------------


class TestFrontmatterNameParsing:
    """Test _parse_skill_name."""

    def test_extracts_name(self, tmp_path):
        md = tmp_path / "SKILL.md"
        md.write_text("---\nname: my-helper\ndescription: A tool\n---\n# Content")
        assert skill_ledger_hook._parse_skill_name(md) == "my-helper"

    def test_quoted_name(self, tmp_path):
        md = tmp_path / "SKILL.md"
        md.write_text('---\nname: "quoted-name"\n---\n# Content')
        assert skill_ledger_hook._parse_skill_name(md) == "quoted-name"

    def test_single_quoted_name(self, tmp_path):
        md = tmp_path / "SKILL.md"
        md.write_text("---\nname: 'single-quoted'\n---\n# Content")
        assert skill_ledger_hook._parse_skill_name(md) == "single-quoted"

    def test_no_frontmatter(self, tmp_path):
        md = tmp_path / "SKILL.md"
        md.write_text("# Just content\nno frontmatter here")
        assert skill_ledger_hook._parse_skill_name(md) is None

    def test_no_closing_dashes(self, tmp_path):
        md = tmp_path / "SKILL.md"
        md.write_text("---\nname: broken\nno closing")
        assert skill_ledger_hook._parse_skill_name(md) is None

    def test_no_name_field(self, tmp_path):
        md = tmp_path / "SKILL.md"
        md.write_text("---\ndescription: no name\n---\n# Content")
        assert skill_ledger_hook._parse_skill_name(md) is None

    def test_empty_name_returns_none(self, tmp_path):
        md = tmp_path / "SKILL.md"
        md.write_text("---\nname: \n---\n# Content")
        assert skill_ledger_hook._parse_skill_name(md) is None

    def test_nonexistent_file(self, tmp_path):
        md = tmp_path / "nope.md"
        assert skill_ledger_hook._parse_skill_name(md) is None


# ---------------------------------------------------------------------------
# Skill catalog tests (frontmatter name vs directory name)
# ---------------------------------------------------------------------------


class TestSkillCatalog:
    """Test _build_skill_catalog with frontmatter name resolution."""

    def test_frontmatter_name_used_over_dir_name(self, tmp_path, monkeypatch):
        """When SKILL.md has name: X, catalog uses X not directory name."""
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        # Directory named 'dir-name' but frontmatter says 'fancy-name'
        skill_dir = skills_dir / "dir-name"
        skill_dir.mkdir(parents=True)
        (skill_dir / "SKILL.md").write_text(
            "---\nname: fancy-name\ndescription: Test\n---\n# Skill"
        )
        monkeypatch.setenv("CODEX_HOME", str(codex_home))

        catalog = skill_ledger_hook._build_skill_catalog(str(tmp_path))
        # Should be found by frontmatter name
        assert "fancy-name" in catalog
        # Should NOT be found by directory name
        assert "dir-name" not in catalog

    def test_fallback_to_dir_name_when_no_frontmatter_name(self, tmp_path, monkeypatch):
        """When SKILL.md has no name field, directory name is used."""
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        skill_dir = skills_dir / "my-tool"
        skill_dir.mkdir(parents=True)
        (skill_dir / "SKILL.md").write_text(
            "---\ndescription: No name field\n---\n# Content"
        )
        monkeypatch.setenv("CODEX_HOME", str(codex_home))

        catalog = skill_ledger_hook._build_skill_catalog(str(tmp_path))
        assert "my-tool" in catalog

    def test_first_match_wins_priority(self, tmp_path, monkeypatch):
        """Same name in multiple roots → catalog collects all of them.

        _resolve_skill_dir should return None (ambiguous) for duplicates,
        matching Codex semantics where plain $skill-name is only used when
        the name is unique across all roots.
        """
        # Create project with .git marker and .agents/skills/
        project = tmp_path / "proj"
        project.mkdir()
        (project / ".git").mkdir()
        repo_skill = project / ".agents" / "skills" / "dup-skill"
        repo_skill.mkdir(parents=True)
        (repo_skill / "SKILL.md").write_text(
            "---\nname: dup-skill\ndescription: Repo version\n---\n"
        )

        # Also in CODEX_HOME
        codex_home = tmp_path / ".codex"
        user_skill = codex_home / "skills" / "dup-skill"
        user_skill.mkdir(parents=True)
        (user_skill / "SKILL.md").write_text(
            "---\nname: dup-skill\ndescription: User version\n---\n"
        )
        monkeypatch.setenv("CODEX_HOME", str(codex_home))

        catalog = skill_ledger_hook._build_skill_catalog(str(project))
        # Both directories should be collected
        assert len(catalog["dup-skill"]) == 2
        # Resolve should return None (ambiguous)
        result = skill_ledger_hook._resolve_skill_dir("dup-skill", catalog)
        assert result is None

    def test_catalog_from_parent_walk_up(self, tmp_path, monkeypatch):
        """Skills in project root .agents/skills/ found from subdir."""
        project = tmp_path / "proj"
        project.mkdir()
        (project / ".git").mkdir()
        skill_dir = project / ".agents" / "skills" / "parent-skill"
        skill_dir.mkdir(parents=True)
        (skill_dir / "SKILL.md").write_text("---\nname: parent-skill\n---\n# Parent")

        cwd = project / "src" / "deep"
        cwd.mkdir(parents=True)
        monkeypatch.setenv("CODEX_HOME", str(tmp_path / "no-codex"))

        catalog = skill_ledger_hook._build_skill_catalog(str(cwd))
        assert "parent-skill" in catalog
        assert str(skill_dir.resolve()) in catalog["parent-skill"]

    def test_symlink_traversal_excluded(self, tmp_path, monkeypatch):
        """Symlink pointing outside skill root is excluded from catalog."""
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        skills_dir.mkdir(parents=True)

        # Create a real dir outside the skill root
        outside = tmp_path / "outside-secret"
        outside.mkdir()
        (outside / "SKILL.md").write_text("---\nname: evil-skill\n---\n# Evil")

        # Create a symlink inside skills_dir pointing to outside
        symlink = skills_dir / "evil-link"
        symlink.symlink_to(outside)

        monkeypatch.setenv("CODEX_HOME", str(codex_home))
        catalog = skill_ledger_hook._build_skill_catalog(str(tmp_path))
        # Should NOT be in catalog because resolved path escapes skill root
        assert "evil-skill" not in catalog
        assert "evil-link" not in catalog

    def test_valid_symlink_within_root_included(self, tmp_path, monkeypatch):
        """Symlink pointing within skill root is allowed."""
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        skills_dir.mkdir(parents=True)

        # Create a real skill dir inside the root
        real_skill = skills_dir / "real-skill"
        real_skill.mkdir()
        (real_skill / "SKILL.md").write_text("---\nname: good-skill\n---\n# Good")

        # Symlink inside root pointing to another dir inside root
        alias = skills_dir / "alias-skill"
        alias.symlink_to(real_skill)

        monkeypatch.setenv("CODEX_HOME", str(codex_home))
        catalog = skill_ledger_hook._build_skill_catalog(str(tmp_path))
        # real-skill is in catalog (frontmatter name "good-skill")
        assert "good-skill" in catalog


# ---------------------------------------------------------------------------
# Key management tests
# ---------------------------------------------------------------------------


class TestKeyManagement:
    """Test _keys_exist and _ensure_keys."""

    def test_keys_exist_true(self, tmp_path, monkeypatch):
        data_dir = tmp_path / "agent-sec" / "skill-ledger"
        data_dir.mkdir(parents=True)
        (data_dir / "key.pub").write_text("pub")
        (data_dir / "key.enc").write_text("enc")
        monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path))

        assert skill_ledger_hook._keys_exist() is True

    def test_keys_exist_false(self, tmp_path, monkeypatch):
        monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path))
        assert skill_ledger_hook._keys_exist() is False

    def test_ensure_keys_skips_when_exist(self, tmp_path, monkeypatch):
        data_dir = tmp_path / "agent-sec" / "skill-ledger"
        data_dir.mkdir(parents=True)
        (data_dir / "key.pub").write_text("pub")
        (data_dir / "key.enc").write_text("enc")
        monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path))

        captured = {}

        def fake_run(args, **kwargs):
            captured["called"] = True
            return subprocess.CompletedProcess(args, 0, "", "")

        monkeypatch.setattr(skill_ledger_hook.subprocess, "run", fake_run)
        skill_ledger_hook._ensure_keys({})
        assert "called" not in captured  # should not have called subprocess


# ---------------------------------------------------------------------------
# Mode-based tests with real skill dir
# ---------------------------------------------------------------------------


class TestDenyMode:
    """In deny mode, failed integrity check triggers block."""

    def test_drifted_blocks(self, tmp_path, mock_cli, monkeypatch):
        # Setup skill dir
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "my-skill")
        # Setup keys
        data_dir = tmp_path / "xdg" / "agent-sec" / "skill-ledger"
        data_dir.mkdir(parents=True)
        (data_dir / "key.pub").write_text("pub")
        (data_dir / "key.enc").write_text("enc")

        env = mock_cli(
            output=_DRIFTED_CHECK,
            extra={
                "SKILL_LEDGER_MODE": "deny",
                "CODEX_HOME": str(codex_home),
                "XDG_DATA_HOME": str(tmp_path / "xdg"),
            },
        )
        output = _run_hook(
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "$my-skill 帮我重构",
                "cwd": str(tmp_path),
            },
            env_override=env,
        )
        assert output["decision"] == "block"
        assert "my-skill" in output["reason"]
        assert "文件内容已变更" in output["reason"]

    def test_pass_allows(self, tmp_path, mock_cli, monkeypatch):
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "good-skill")
        data_dir = tmp_path / "xdg" / "agent-sec" / "skill-ledger"
        data_dir.mkdir(parents=True)
        (data_dir / "key.pub").write_text("pub")
        (data_dir / "key.enc").write_text("enc")

        env = mock_cli(
            output=_PASS_CHECK,
            extra={
                "SKILL_LEDGER_MODE": "deny",
                "CODEX_HOME": str(codex_home),
                "XDG_DATA_HOME": str(tmp_path / "xdg"),
            },
        )
        output = _run_hook(
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "$good-skill hello",
                "cwd": str(tmp_path),
            },
            env_override=env,
        )
        assert output == {}

    def test_tampered_blocks(self, tmp_path, mock_cli):
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "bad-skill")
        data_dir = tmp_path / "xdg" / "agent-sec" / "skill-ledger"
        data_dir.mkdir(parents=True)
        (data_dir / "key.pub").write_text("pub")
        (data_dir / "key.enc").write_text("enc")

        env = mock_cli(
            output=_TAMPERED_CHECK,
            extra={
                "SKILL_LEDGER_MODE": "deny",
                "CODEX_HOME": str(codex_home),
                "XDG_DATA_HOME": str(tmp_path / "xdg"),
            },
        )
        output = _run_hook(
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "$bad-skill do something",
                "cwd": str(tmp_path),
            },
            env_override=env,
        )
        assert output["decision"] == "block"
        assert "签名验证失败" in output["reason"]


class TestObserveMode:
    """In observe mode, failed checks don't block."""

    def test_drifted_not_blocked(self, tmp_path, mock_cli):
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "my-skill")
        data_dir = tmp_path / "xdg" / "agent-sec" / "skill-ledger"
        data_dir.mkdir(parents=True)
        (data_dir / "key.pub").write_text("pub")
        (data_dir / "key.enc").write_text("enc")

        env = mock_cli(
            output=_DRIFTED_CHECK,
            extra={
                "SKILL_LEDGER_MODE": "observe",
                "CODEX_HOME": str(codex_home),
                "XDG_DATA_HOME": str(tmp_path / "xdg"),
            },
        )
        output = _run_hook(
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "$my-skill hello",
                "cwd": str(tmp_path),
            },
            env_override=env,
        )
        assert output == {}


class TestUnknownMode:
    """Unknown mode acts as fail-open."""

    def test_unknown_mode_allows(self, tmp_path, mock_cli):
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "my-skill")
        data_dir = tmp_path / "xdg" / "agent-sec" / "skill-ledger"
        data_dir.mkdir(parents=True)
        (data_dir / "key.pub").write_text("pub")
        (data_dir / "key.enc").write_text("enc")

        env = mock_cli(
            output=_DRIFTED_CHECK,
            extra={
                "SKILL_LEDGER_MODE": "banana",
                "CODEX_HOME": str(codex_home),
                "XDG_DATA_HOME": str(tmp_path / "xdg"),
            },
        )
        output = _run_hook(
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "$my-skill hello",
                "cwd": str(tmp_path),
            },
            env_override=env,
        )
        assert "execution continues" in output["systemMessage"]


def test_invalid_mode_reports_ask_fallback(monkeypatch, capsys):
    monkeypatch.setenv("SKILL_LEDGER_MODE", "banana")

    assert skill_ledger_hook._read_policy() == "ask"
    assert "invalid SKILL_LEDGER_MODE; using ask" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# Monkeypatch-based (white-box) tests
# ---------------------------------------------------------------------------


class TestMainMonkeypatch:
    """Direct main() testing with mocked internals."""

    def _run_main(self, monkeypatch, capsys, input_data, *, mode="deny"):
        monkeypatch.setattr(skill_ledger_hook, "MODE", mode)
        monkeypatch.setattr(
            skill_ledger_hook.sys,
            "stdin",
            io.StringIO(json.dumps(input_data)),
        )
        skill_ledger_hook.main()
        out = capsys.readouterr().out
        return json.loads(out) if out.strip() else {}

    def test_subprocess_exception_allows(self, monkeypatch, capsys, tmp_path):
        """CLI failure during check → fail-open."""
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "test-skill")
        monkeypatch.setenv("CODEX_HOME", str(codex_home))

        def fail_run(*args, **kwargs):
            raise OSError("not found")

        monkeypatch.setattr(skill_ledger_hook.subprocess, "run", fail_run)
        monkeypatch.setattr(skill_ledger_hook, "_keys_exist", lambda: True)

        output = self._run_main(
            monkeypatch,
            capsys,
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "$test-skill hello",
                "cwd": str(tmp_path),
            },
        )
        assert output == {}

    def test_trace_context_injected(self, monkeypatch, capsys, tmp_path):
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "test-skill")
        monkeypatch.setenv("CODEX_HOME", str(codex_home))

        captured = {}

        def fake_run(args, **kwargs):
            captured["args"] = args
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps({"status": "pass"}),
                stderr="",
            )

        monkeypatch.setattr(skill_ledger_hook.subprocess, "run", fake_run)
        monkeypatch.setattr(skill_ledger_hook, "_keys_exist", lambda: True)

        self._run_main(
            monkeypatch,
            capsys,
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "$test-skill do it",
                "cwd": str(tmp_path),
                "trace_id": "t1",
                "session_id": "s1",
            },
        )
        assert "--trace-context" in captured["args"]

    def test_multiple_skills_checked(self, monkeypatch, capsys, tmp_path):
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "skill-a")
        _make_skill_dir(skills_dir, "skill-b")
        monkeypatch.setenv("CODEX_HOME", str(codex_home))

        call_count = {"n": 0}

        def fake_run(args, **kwargs):
            call_count["n"] += 1
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps({"status": "pass"}),
                stderr="",
            )

        monkeypatch.setattr(skill_ledger_hook.subprocess, "run", fake_run)
        monkeypatch.setattr(skill_ledger_hook, "_keys_exist", lambda: True)

        self._run_main(
            monkeypatch,
            capsys,
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "$skill-a and $skill-b",
                "cwd": str(tmp_path),
            },
        )
        assert call_count["n"] == 2

    def test_one_fail_one_pass_blocks(self, monkeypatch, capsys, tmp_path):
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "good-skill")
        _make_skill_dir(skills_dir, "bad-skill")
        monkeypatch.setenv("CODEX_HOME", str(codex_home))

        def fake_run(args, **kwargs):
            # Return drifted for bad-skill, pass for good-skill
            skill_dir = args[-1]
            if "bad-skill" in skill_dir:
                return subprocess.CompletedProcess(
                    args, 0, json.dumps({"status": "drifted"}), ""
                )
            return subprocess.CompletedProcess(
                args, 0, json.dumps({"status": "pass"}), ""
            )

        monkeypatch.setattr(skill_ledger_hook.subprocess, "run", fake_run)
        monkeypatch.setattr(skill_ledger_hook, "_keys_exist", lambda: True)

        output = self._run_main(
            monkeypatch,
            capsys,
            {
                "hook_event_name": "UserPromptSubmit",
                "prompt": "$good-skill $bad-skill",
                "cwd": str(tmp_path),
            },
        )
        assert output["decision"] == "block"
        assert "bad-skill" in output["reason"]
        assert "good-skill" not in output["reason"]


# ---------------------------------------------------------------------------
# Block status coverage
# ---------------------------------------------------------------------------


class TestBlockStatuses:
    """All statuses in _BLOCK_STATUSES should trigger block in deny mode."""

    @pytest.mark.parametrize(
        "status,label",
        [
            ("none", "从未扫描"),
            ("drifted", "文件内容已变更"),
            ("warn", "扫描有低风险发现"),
            ("deny", "扫描有高风险发现"),
            ("tampered", "签名验证失败"),
        ],
    )
    def test_block_status(self, status, label, monkeypatch, capsys, tmp_path):
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "s")
        monkeypatch.setenv("CODEX_HOME", str(codex_home))

        def fake_run(args, **kwargs):
            return subprocess.CompletedProcess(
                args, 0, json.dumps({"status": status}), ""
            )

        monkeypatch.setattr(skill_ledger_hook.subprocess, "run", fake_run)
        monkeypatch.setattr(skill_ledger_hook, "_keys_exist", lambda: True)
        monkeypatch.setattr(skill_ledger_hook, "MODE", "deny")
        monkeypatch.setattr(
            skill_ledger_hook.sys,
            "stdin",
            io.StringIO(
                json.dumps(
                    {
                        "hook_event_name": "UserPromptSubmit",
                        "prompt": "$s hello",
                        "cwd": str(tmp_path),
                    }
                )
            ),
        )
        skill_ledger_hook.main()
        out = capsys.readouterr().out
        output = json.loads(out)
        assert output["decision"] == "block"
        assert label in output["reason"]

    def test_pass_status_allows(self, monkeypatch, capsys, tmp_path):
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "s")
        monkeypatch.setenv("CODEX_HOME", str(codex_home))

        def fake_run(args, **kwargs):
            return subprocess.CompletedProcess(
                args, 0, json.dumps({"status": "pass"}), ""
            )

        monkeypatch.setattr(skill_ledger_hook.subprocess, "run", fake_run)
        monkeypatch.setattr(skill_ledger_hook, "_keys_exist", lambda: True)
        monkeypatch.setattr(skill_ledger_hook, "MODE", "deny")
        monkeypatch.setattr(
            skill_ledger_hook.sys,
            "stdin",
            io.StringIO(
                json.dumps(
                    {
                        "hook_event_name": "UserPromptSubmit",
                        "prompt": "$s hello",
                        "cwd": str(tmp_path),
                    }
                )
            ),
        )
        skill_ledger_hook.main()
        out = capsys.readouterr().out
        assert out.strip() == ""

    def test_unknown_status_allows(self, monkeypatch, capsys, tmp_path):
        codex_home = tmp_path / ".codex"
        skills_dir = codex_home / "skills"
        _make_skill_dir(skills_dir, "s")
        monkeypatch.setenv("CODEX_HOME", str(codex_home))

        def fake_run(args, **kwargs):
            return subprocess.CompletedProcess(
                args, 0, json.dumps({"status": "unknown"}), ""
            )

        monkeypatch.setattr(skill_ledger_hook.subprocess, "run", fake_run)
        monkeypatch.setattr(skill_ledger_hook, "_keys_exist", lambda: True)
        monkeypatch.setattr(skill_ledger_hook, "MODE", "deny")
        monkeypatch.setattr(
            skill_ledger_hook.sys,
            "stdin",
            io.StringIO(
                json.dumps(
                    {
                        "hook_event_name": "UserPromptSubmit",
                        "prompt": "$s hello",
                        "cwd": str(tmp_path),
                    }
                )
            ),
        )
        skill_ledger_hook.main()
        out = capsys.readouterr().out
        assert out.strip() == ""
