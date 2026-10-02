"""Unit tests for skill_ledger config — merge, resolve, remember, compact.

These tests protect the configuration-layer invariants:
1. Defaults stay enabled unless explicitly disabled.
2. Dynamic discovery entries are stored in managedSkillDirs.
3. SKILL.md gate — glob resolution only includes dirs with SKILL.md.
4. Auto-remember — scan/certify auto-append uncovered skill dirs.
5. Compact — specific paths subsumed by a glob are pruned.
"""

import json
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from agent_sec_cli.skill_ledger import config as config_module
from agent_sec_cli.skill_ledger.config import (
    _DEFAULT_CONFIG,
    DEFAULT_SKILL_DIRS,
    _compact_skill_dirs,
    _deep_merge_config,
    default_skill_dir_entries,
    deprecated_skill_dir_entries,
    effective_skill_dir_entries,
    is_covered,
    is_default_system_skill_dir,
    is_default_user_skill_dir,
    is_managed_covered,
    load_config,
    remember_skill_dir,
    resolve_activation_policy,
    resolve_managed_skill_dirs,
    resolve_skill_dirs,
)
from agent_sec_cli.skill_ledger.errors import ConfigError

ACTIVATION_POLICY_PASS_WARN_ONLY = "pass_warn_only"


class TestDefaultConfig(unittest.TestCase):
    """Default config must include the well-known skill directories."""

    def test_default_skill_dirs_present(self):
        dirs = DEFAULT_SKILL_DIRS
        self.assertIn("~/.openclaw/skills/*", dirs)
        self.assertIn("~/.copilot-shell/skills/*", dirs)
        self.assertIn("~/.hermes/skills/**", dirs)
        self.assertIn("~/.qoder/skills/*", dirs)
        self.assertIn("/usr/share/anolisa/skills/*", dirs)
        self.assertIn("/usr/local/share/anolisa/skills/*", dirs)
        self.assertNotIn(".qoder/skills/*", dirs)
        self.assertTrue(_DEFAULT_CONFIG["enableDefaultSkillDirs"])
        self.assertEqual(_DEFAULT_CONFIG["managedSkillDirs"], [])

    def test_default_signing_backend(self):
        self.assertEqual(_DEFAULT_CONFIG["signingBackend"], "ed25519")

    def test_system_skill_matching_uses_exact_parent(self):
        self.assertTrue(
            is_default_system_skill_dir("/usr/share/anolisa/skills/weather")
        )
        self.assertTrue(
            is_default_system_skill_dir("/usr/local/share/anolisa/skills/weather")
        )
        self.assertTrue(
            is_default_system_skill_dir("/usr/share/anolisa/skills/../skills/weather")
        )
        self.assertFalse(
            is_default_system_skill_dir("/usr/share/anolisa/skills-evil/weather")
        )
        self.assertFalse(is_default_system_skill_dir("/usr/share/anolisa/skills"))
        self.assertFalse(
            is_default_system_skill_dir("/usr/share/anolisa/skills/weather/nested")
        )

    def test_default_activation_policy(self):
        self.assertEqual(
            _DEFAULT_CONFIG["activationPolicy"], ACTIVATION_POLICY_PASS_WARN_ONLY
        )
        self.assertEqual(
            resolve_activation_policy(_DEFAULT_CONFIG), ACTIVATION_POLICY_PASS_WARN_ONLY
        )

    def test_pass_warn_only_constant_is_exported(self):
        self.assertEqual(
            getattr(config_module, "ACTIVATION_POLICY_PASS_WARN_ONLY", None),
            ACTIVATION_POLICY_PASS_WARN_ONLY,
        )

    def test_default_scanners_present(self):
        scanners = {entry["name"]: entry for entry in _DEFAULT_CONFIG["scanners"]}
        self.assertIn("skill-vetter", scanners)
        self.assertIn("code-scanner", scanners)
        self.assertIn("static-scanner", scanners)
        self.assertEqual(scanners["code-scanner"]["type"], "builtin")
        self.assertEqual(scanners["static-scanner"]["type"], "builtin")
        self.assertTrue(scanners["code-scanner"]["enabled"])
        self.assertTrue(scanners["static-scanner"]["enabled"])


class TestConfigMerge(unittest.TestCase):
    """Managed dirs are distinct from default discovery dirs."""

    def test_managed_dirs_replaced_from_user_config(self):
        defaults = {"managedSkillDirs": ["/default/managed/*"]}
        user = {"managedSkillDirs": ["/opt/custom/*"]}
        merged = _deep_merge_config(defaults, user)
        self.assertEqual(merged["managedSkillDirs"], ["/opt/custom/*"])

    def test_managed_duplicate_entries_deduped(self):
        defaults = {"managedSkillDirs": []}
        user = {"managedSkillDirs": ["/opt/new/*", "/opt/new/*"]}
        merged = _deep_merge_config(defaults, user)
        self.assertEqual(merged["managedSkillDirs"], ["/opt/new/*"])

    def test_managed_dirs_reject_multiple_leading_separators(self):
        for entry in ("//skills/weather", "//skills/*", "//skills/**"):
            with self.subTest(entry=entry):
                with self.assertRaisesRegex(ConfigError, "single leading"):
                    _deep_merge_config(
                        {"managedSkillDirs": []},
                        {"managedSkillDirs": [entry]},
                    )

    def test_empty_managed_dirs_are_preserved(self):
        defaults = {"managedSkillDirs": ["a/*"]}
        user = {"managedSkillDirs": []}
        merged = _deep_merge_config(defaults, user)
        self.assertEqual(merged["managedSkillDirs"], [])

    def test_effective_entries_include_defaults_by_default(self):
        config = {"enableDefaultSkillDirs": True, "managedSkillDirs": ["/opt/custom/*"]}
        entries = effective_skill_dir_entries(config)
        self.assertEqual(entries, [*default_skill_dir_entries(), "/opt/custom/*"])

    def test_managed_resolver_excludes_default_skill_dirs(self):
        tmpdir = Path(tempfile.mkdtemp())
        try:
            default_parent = tmpdir / "default-skills"
            managed_parent = tmpdir / "managed-skills"
            default_parent.mkdir()
            managed_parent.mkdir()
            default_skill = default_parent / "default-only"
            managed_skill = managed_parent / "managed-only"
            for skill in (default_skill, managed_skill):
                skill.mkdir()
                (skill / "SKILL.md").write_text("---\nname: test\n---\n")
            config = {
                "enableDefaultSkillDirs": True,
                "managedSkillDirs": [str(managed_parent / "*")],
            }

            with patch.object(
                config_module,
                "DEFAULT_SKILL_DIRS",
                [str(default_parent / "*")],
            ):
                all_dirs = resolve_skill_dirs(config)
                managed_dirs = resolve_managed_skill_dirs(config)

            self.assertEqual(
                {path.resolve() for path in all_dirs},
                {default_skill.resolve(), managed_skill.resolve()},
            )
            self.assertEqual(
                [path.resolve() for path in managed_dirs],
                [managed_skill.resolve()],
            )
        finally:
            shutil.rmtree(tmpdir)

    def test_effective_entries_can_disable_defaults(self):
        config = {
            "enableDefaultSkillDirs": False,
            "managedSkillDirs": ["/opt/custom/*"],
        }
        entries = effective_skill_dir_entries(config)
        self.assertEqual(entries, ["/opt/custom/*"])

    def test_deprecated_skilldirs_are_diagnostic_only(self):
        config = {
            "enableDefaultSkillDirs": False,
            "skillDirs": ["/legacy/*"],
            "managedSkillDirs": ["/managed/*"],
        }
        self.assertEqual(deprecated_skill_dir_entries(config), ["/legacy/*"])
        self.assertEqual(effective_skill_dir_entries(config), ["/managed/*"])

    def test_load_config_warns_when_deprecated_skilldirs_present(self):
        cfg_dir = Path(tempfile.mkdtemp())
        try:
            cfg_path = cfg_dir / "config.json"
            cfg_path.write_text(
                '{"skillDirs": ["/legacy/*"], "managedSkillDirs": ["/managed/*"]}',
                encoding="utf-8",
            )
            with patch(
                "agent_sec_cli.skill_ledger.config.get_config_dir",
                return_value=cfg_dir,
            ):
                with self.assertLogs(
                    "agent_sec_cli.skill_ledger.config", level="WARNING"
                ) as logs:
                    cfg = load_config()

            self.assertIn("skillDirs", cfg)
            self.assertIn("/legacy/*", cfg["skillDirs"])
            self.assertEqual(cfg["managedSkillDirs"], ["/managed/*"])
            self.assertTrue(any("Ignoring deprecated" in msg for msg in logs.output))
        finally:
            shutil.rmtree(cfg_dir)

    def test_non_managed_keys_still_replaced(self):
        """Other list keys use standard replacement, not additive."""
        defaults = {"otherList": [1, 2]}
        user = {"otherList": [3]}
        merged = _deep_merge_config(defaults, user)
        self.assertEqual(merged["otherList"], [3])

    def test_resolve_activation_policy_accepts_pass_warn_only(self):
        self.assertEqual(
            resolve_activation_policy(
                {"activationPolicy": ACTIVATION_POLICY_PASS_WARN_ONLY}
            ),
            ACTIVATION_POLICY_PASS_WARN_ONLY,
        )

    def test_resolve_activation_policy_rejects_unknown_policy(self):
        with self.assertRaisesRegex(ConfigError, "activationPolicy"):
            resolve_activation_policy({"activationPolicy": "unknown"})

    def test_resolve_activation_policy_rejects_non_string_policy(self):
        with self.assertRaisesRegex(ConfigError, "activationPolicy"):
            resolve_activation_policy({"activationPolicy": ["pass_warn_only"]})

    def test_load_config_rejects_invalid_policy_before_merging(self):
        with tempfile.TemporaryDirectory() as directory:
            cfg_dir = Path(directory)
            cfg_path = cfg_dir / "config.json"
            cfg_path.write_text(
                json.dumps({"activationPolicy": "invalid"}), encoding="utf-8"
            )
            before = cfg_path.read_bytes()
            with patch.object(config_module, "get_config_dir", return_value=cfg_dir):
                with patch.object(config_module, "_deep_merge_config") as merge:
                    with self.assertRaises(ConfigError) as raised:
                        load_config()
                    merge.assert_not_called()
            for expected in (
                str(cfg_path),
                "activationPolicy",
                "invalid",
                "pass_warn_only",
            ):
                self.assertIn(expected, str(raised.exception))
            self.assertEqual(cfg_path.read_bytes(), before)

    def test_load_config_preserves_pass_warn_only_activation_policy(self):
        cfg_dir = Path(tempfile.mkdtemp())
        try:
            cfg_path = cfg_dir / "config.json"
            cfg_path.write_text(
                json.dumps({"activationPolicy": ACTIVATION_POLICY_PASS_WARN_ONLY}),
                encoding="utf-8",
            )
            with patch(
                "agent_sec_cli.skill_ledger.config.get_config_dir",
                return_value=cfg_dir,
            ):
                cfg = load_config()
            self.assertEqual(
                resolve_activation_policy(cfg),
                ACTIVATION_POLICY_PASS_WARN_ONLY,
            )
        finally:
            shutil.rmtree(cfg_dir)


class TestResolveSkillDirs(unittest.TestCase):
    """Glob resolution must filter by SKILL.md presence and dedup."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.parent = Path(self.tmpdir) / "skills"
        self.parent.mkdir()

    def tearDown(self):
        shutil.rmtree(self.tmpdir)

    def _make_skill(self, name: str, has_manifest: bool = True) -> Path:
        d = self.parent / name
        d.mkdir(exist_ok=True)
        if has_manifest:
            (d / "SKILL.md").write_text("---\nname: test\n---\n")
        return d

    def test_glob_includes_dirs_with_skill_md(self):
        self._make_skill("alpha", has_manifest=True)
        self._make_skill("beta", has_manifest=True)
        config = {
            "enableDefaultSkillDirs": False,
            "managedSkillDirs": [str(self.parent) + "/*"],
        }
        result = resolve_skill_dirs(config)
        names = [p.name for p in result]
        self.assertIn("alpha", names)
        self.assertIn("beta", names)

    def test_glob_excludes_dirs_without_skill_md(self):
        self._make_skill("real-skill", has_manifest=True)
        self._make_skill("not-a-skill", has_manifest=False)
        config = {
            "enableDefaultSkillDirs": False,
            "managedSkillDirs": [str(self.parent) + "/*"],
        }
        result = resolve_skill_dirs(config)
        names = [p.name for p in result]
        self.assertIn("real-skill", names)
        self.assertNotIn("not-a-skill", names)

    def test_glob_excludes_hidden_dirs(self):
        self._make_skill(".hidden", has_manifest=True)
        config = {
            "enableDefaultSkillDirs": False,
            "managedSkillDirs": [str(self.parent) + "/*"],
        }
        result = resolve_skill_dirs(config)
        names = [p.name for p in result]
        self.assertNotIn(".hidden", names)

    def test_specific_path_requires_skill_md(self):
        """Explicit paths are also filtered by SKILL.md presence."""
        d = self._make_skill("explicit", has_manifest=False)
        config = {"enableDefaultSkillDirs": False, "managedSkillDirs": [str(d)]}
        result = resolve_skill_dirs(config)
        self.assertEqual(result, [])

    def test_specific_path_with_skill_md_included(self):
        d = self._make_skill("explicit", has_manifest=True)
        config = {"enableDefaultSkillDirs": False, "managedSkillDirs": [str(d)]}
        result = resolve_skill_dirs(config)
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0].name, "explicit")

    def test_nonexistent_dir_silently_skipped(self):
        config = {
            "enableDefaultSkillDirs": False,
            "managedSkillDirs": ["/no/such/path/*", "/no/such/single"],
        }
        result = resolve_skill_dirs(config)
        self.assertEqual(result, [])

    def test_dedup_by_resolved_path(self):
        self._make_skill("dup", has_manifest=True)
        d = self.parent / "dup"
        config = {
            "enableDefaultSkillDirs": False,
            "managedSkillDirs": [str(self.parent) + "/*", str(d)],
        }
        result = resolve_skill_dirs(config)
        resolved = [p.resolve() for p in result]
        self.assertEqual(len(resolved), len(set(resolved)))

    def test_recursive_glob_includes_nested_hermes_skills(self):
        skill_dir = self.parent / "mlops" / "axolotl"
        skill_dir.mkdir(parents=True)
        (skill_dir / "SKILL.md").write_text("---\nname: axolotl\n---\n")
        config = {
            "enableDefaultSkillDirs": False,
            "managedSkillDirs": [str(self.parent) + "/**"],
        }
        result = resolve_skill_dirs(config)
        self.assertEqual([p.resolve() for p in result], [skill_dir.resolve()])

    def test_recursive_glob_skips_internal_and_hidden_dirs(self):
        visible = self.parent / "ai" / "visible"
        hidden = self.parent / ".archive" / "hidden"
        meta = self.parent / "real" / ".skill-meta" / "snapshot"
        for skill_dir in (visible, hidden, meta):
            skill_dir.mkdir(parents=True)
            (skill_dir / "SKILL.md").write_text("---\nname: test\n---\n")
        config = {
            "enableDefaultSkillDirs": False,
            "managedSkillDirs": [str(self.parent) + "/**"],
        }
        result = resolve_skill_dirs(config)
        self.assertEqual([p.resolve() for p in result], [visible.resolve()])


class TestCompactSkillDirs(unittest.TestCase):
    """Specific paths subsumed by a glob must be pruned."""

    def test_specific_removed_when_glob_exists(self):
        entries = ["/opt/skills/*", "/opt/skills/foo"]
        result = _compact_skill_dirs(entries)
        self.assertEqual(result, ["/opt/skills/*"])

    def test_glob_kept_when_no_overlap(self):
        entries = ["/a/*", "/b/specific"]
        result = _compact_skill_dirs(entries)
        self.assertEqual(entries, result)

    def test_duplicate_entries_deduped(self):
        entries = ["/a/*", "/a/*", "/b"]
        result = _compact_skill_dirs(entries)
        self.assertEqual(result, ["/a/*", "/b"])

    def test_tilde_normalised_for_comparison(self):
        home = str(Path.home())
        entries = [
            "~/.copilot-shell/skills/*",
            f"{home}/.copilot-shell/skills/my-tool",
        ]
        result = _compact_skill_dirs(entries)
        self.assertEqual(result, ["~/.copilot-shell/skills/*"])

    def test_specific_removed_when_recursive_glob_exists(self):
        entries = ["/opt/hermes/skills/**", "/opt/hermes/skills/mlops/axolotl"]
        result = _compact_skill_dirs(entries)
        self.assertEqual(result, ["/opt/hermes/skills/**"])


class TestRememberSkillDir(unittest.TestCase):
    """Auto-remember must add correct entry and compact afterward."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.config_dir = Path(self.tmpdir) / "config" / "agent-sec" / "skill-ledger"
        self.config_dir.mkdir(parents=True)
        self.config_file = self.config_dir / "config.json"

        self.skills_root = Path(self.tmpdir) / "skills"
        self.skills_root.mkdir()

    def tearDown(self):
        shutil.rmtree(self.tmpdir)

    def _make_skill(self, name: str) -> Path:
        d = self.skills_root / name
        d.mkdir(exist_ok=True)
        (d / "SKILL.md").write_text("---\nname: test\n---\n")
        return d

    def _patched_remember(self, skill_dir: Path, config: dict) -> str | None:
        """Call remember_skill_dir with config_path and save_config patched."""
        with (
            patch(
                "agent_sec_cli.skill_ledger.config.config_path",
                return_value=self.config_file,
            ),
            patch(
                "agent_sec_cli.skill_ledger.config.load_config",
                return_value=config,
            ),
        ):
            return remember_skill_dir(skill_dir, config)

    def test_single_skill_adds_specific_path(self):
        s = self._make_skill("only-one")
        config = {"enableDefaultSkillDirs": False, "managedSkillDirs": []}
        entry = self._patched_remember(s, config)
        self.assertEqual(entry, str(s))

    def test_two_siblings_adds_parent_glob(self):
        self._make_skill("alpha")
        s = self._make_skill("beta")
        config = {"enableDefaultSkillDirs": False, "managedSkillDirs": []}
        entry = self._patched_remember(s, config)
        self.assertEqual(entry, str(self.skills_root) + "/*")

    def test_already_covered_returns_none(self):
        s = self._make_skill("covered")
        config = {
            "enableDefaultSkillDirs": False,
            "managedSkillDirs": [str(self.skills_root) + "/*"],
        }
        entry = self._patched_remember(s, config)
        self.assertIsNone(entry)

    def test_default_dir_coverage_still_remembers_as_managed(self):
        s = self._make_skill("default-covered")
        config = {"enableDefaultSkillDirs": True, "managedSkillDirs": []}

        with patch.object(
            config_module,
            "DEFAULT_SKILL_DIRS",
            [str(self.skills_root) + "/*"],
        ):
            entry = self._patched_remember(s, config)

        self.assertEqual(entry, str(s))
        self.assertEqual(config["managedSkillDirs"], [str(s)])

    def test_compact_prunes_after_glob_promotion(self):
        s1 = self._make_skill("first")
        config = {"enableDefaultSkillDirs": False, "managedSkillDirs": [str(s1)]}
        # Add second sibling → should promote to parent/* and remove specific
        s2 = self._make_skill("second")
        self._patched_remember(s2, config)
        self.assertIn(str(self.skills_root) + "/*", config["managedSkillDirs"])
        self.assertNotIn(str(s1), config["managedSkillDirs"])


class TestIsCovered(unittest.TestCase):
    """Coverage detection must match resolve_skill_dirs output."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.parent = Path(self.tmpdir) / "skills"
        self.parent.mkdir()

    def tearDown(self):
        shutil.rmtree(self.tmpdir)

    def test_covered_by_glob(self):
        d = self.parent / "my-skill"
        d.mkdir()
        (d / "SKILL.md").write_text("---\nname: test\n---\n")
        config = {
            "enableDefaultSkillDirs": False,
            "managedSkillDirs": [str(self.parent) + "/*"],
        }
        self.assertTrue(is_covered(d, config))

    def test_not_covered(self):
        d = self.parent / "orphan"
        d.mkdir()
        (d / "SKILL.md").write_text("---\nname: test\n---\n")
        config = {"enableDefaultSkillDirs": False, "managedSkillDirs": []}
        self.assertFalse(is_covered(d, config))

    def test_hidden_nested_path_is_covered_by_recursive_managed_entry(self):
        hidden = self.parent / "apple" / "apple-notes"
        config = {
            "enableDefaultSkillDirs": False,
            "managedSkillDirs": [str(self.parent) + "/**"],
        }

        self.assertFalse(hidden.exists())
        self.assertTrue(is_managed_covered(hidden, config))


def test_raw_user_remember_keeps_individual_paths(tmp_path, monkeypatch):
    home = tmp_path / "home"
    monkeypatch.setenv("HOME", str(home))
    for custom_xdg in (False, True):
        data_root = tmp_path / "data" if custom_xdg else home / ".local/share"
        if custom_xdg:
            monkeypatch.setenv("XDG_DATA_HOME", str(data_root))
        else:
            monkeypatch.delenv("XDG_DATA_HOME", raising=False)
        monkeypatch.setenv("XDG_CONFIG_HOME", str(data_root / "config"))
        root = data_root / "anolisa/skills"
        first, sibling, later = [root / name for name in ("first", "sibling", "later")]
        for skill in (first, sibling):
            skill.mkdir(parents=True)
            (skill / "SKILL.md").write_text("---\nname: test\n---\n")
        config_module.save_config({"managedSkillDirs": []})

        assert remember_skill_dir(first) == str(first)
        assert not is_managed_covered(sibling)
        later.mkdir()
        (later / "SKILL.md").write_text("---\nname: test\n---\n")
        assert remember_skill_dir(later) == str(later)
        assert load_config()["managedSkillDirs"] == [str(first), str(later)]
        assert not is_managed_covered(sibling)
        before = config_module.config_path().read_bytes()
        assert remember_skill_dir(first) is None
        assert config_module.config_path().read_bytes() == before

        for entry in (str(first), str(root) + "/*", str(root) + "/**"):
            config_module.save_config({"managedSkillDirs": [entry]})
            before = config_module.config_path().read_bytes()
            assert remember_skill_dir(first) is None
            assert config_module.config_path().read_bytes() == before


def test_raw_user_defaults_follow_xdg_and_default_opt_out(tmp_path, monkeypatch):
    home = tmp_path / "home"
    monkeypatch.setenv("HOME", str(home))
    for data_home in [
        None,
        str(tmp_path / "XDG Data"),
        "",
        "relative",
        "/x/./y",
        "/x/../y",
    ]:
        if data_home is None:
            monkeypatch.delenv("XDG_DATA_HOME", raising=False)
        else:
            monkeypatch.setenv("XDG_DATA_HOME", data_home)
        root = (
            Path(data_home)
            if data_home == str(tmp_path / "XDG Data")
            else home / ".local/share"
        )
        skill = root / "anolisa/skills/raw-probe"
        skill.mkdir(parents=True, exist_ok=True)
        (skill / "SKILL.md").write_text("---\nname: raw-probe\n---\n")
        assert skill in resolve_skill_dirs({"enableDefaultSkillDirs": True})
        assert not resolve_skill_dirs({"enableDefaultSkillDirs": False})
        assert is_default_user_skill_dir(skill)
        assert not is_default_user_skill_dir(skill.parent)
        assert not is_default_user_skill_dir(skill / "nested")
        assert not is_default_user_skill_dir(root / "anolisa/skills-evil/raw-probe")
        assert not is_default_system_skill_dir(skill)


def test_raw_user_defaults_normalize_leading_slashes(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    for count in (2, 3):
        root = tmp_path / f"data-{count}"
        monkeypatch.setenv("XDG_DATA_HOME", "/" * count + str(root).lstrip("/"))
        config = {"enableDefaultSkillDirs": True}
        assert not root.exists()
        assert f"{root}/anolisa/skills/*" in effective_skill_dir_entries(config)
        skill = root / "anolisa/skills/raw-probe"
        assert skill not in resolve_skill_dirs(config)
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text("---\nname: raw-probe\n---\n")
        assert skill in resolve_skill_dirs(config)
        assert is_covered(skill, config)
        assert is_default_user_skill_dir(skill)


if __name__ == "__main__":
    unittest.main()
