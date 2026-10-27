"""Install V2 products from a standalone component source tree without compiling."""

import os
import shlex
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


class DeliveryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.source = self.base / "source"
        for name in (
            "Makefile",
            "LICENSE",
            "packaging/build.mk",
            "packaging/install.mk",
            "packaging/raw.mk",
            "packaging/delivery.mk",
            "packaging/validate-stage.py",
            "packaging/systemd/agent-sec-core-v2.service.in",
            "packaging/systemd/skillsec.json",
        ):
            destination = self.source / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, destination)
        self.build = self.source / "target"
        for name in (
            "v2/bin/agent-sec-cli",
            "v2/bin/agent-sec-daemon",
            "v2/bin/agent-sec-daemon-health",
            "linux-sandbox",
        ):
            self.artifact(name, "#!/bin/sh\nexit 0\n", executable=True)
        for name in (
            "cosh-extension/cosh-extension.json",
            "codex-plugin/install.sh",
            "qoder-plugin/install.sh",
            "qoder-plugin/hooks/fixture.py",
            "openclaw-plugin/openclaw.plugin.json",
            "openclaw-plugin/package.json",
            "openclaw-plugin/dist/index.js",
            "openclaw-plugin/scripts/deploy.sh",
            "hermes-plugin/src/plugin.yaml",
            "hermes-plugin/scripts/deploy.sh",
            "qwen-code-extension/scripts/deploy.sh",
            "qwen-code-extension/hooks/observability_hook.py",
            "qwen-code-extension/hooks/skill_ledger_hook.py",
            "skills/demo/SKILL.md",
        ):
            self.artifact(name, "fixture\n")

    def artifact(self, name, contents, executable=False):
        destination = self.build / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(contents)
        if executable:
            destination.chmod(0o755)

    def make(self, *arguments, env_overrides=None):
        environment = os.environ.copy()
        for name in (
            "MAKEFLAGS",
            "MFLAGS",
            "MAKEOVERRIDES",
            "AWPRJ",
            "PREFIX",
            "DESTDIR",
            "BINDIR",
            "LIBDIR",
            "LIBEXECDIR",
            "DATADIR",
            "ADAPTERDIR",
            "SKILLDIR",
            "UNITDIR",
            "SYSCONFDIR",
            "CONFIGDIR",
            "BUILD_DIR",
            "INSTALL_PROFILE",
            "SYSTEMD_SKILLSEC_CONFIG",
            "SYSTEMD_SKILLSEC_CONFIG_ARGS",
        ):
            environment.pop(name, None)
        if env_overrides:
            environment.update(env_overrides)
        # Installation fixtures explicitly exercise system-package /usr paths.
        if (
            any(
                arg in {"install", "install-core", "install-plugins", "install-skills"}
                for arg in arguments
            )
            and not any(arg.startswith("PREFIX=") for arg in arguments)
            and not (env_overrides and "PREFIX" in env_overrides)
        ):
            arguments = (*arguments, "PREFIX=/usr")
        return subprocess.run(
            ["make", "--no-print-directory", "-C", str(self.source), *arguments],
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )

    def test_root_layout_ignores_legacy_file_defaults_but_honors_explicit_inputs(self):
        result = self.make("layout")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("PREFIX=/usr/local\n", result.stdout)
        self.assertIn("LIBDIR=/usr/local/lib/agent-sec-core\n", result.stdout)
        self.assertIn(
            "SKILLDIR=/usr/local/share/agent-sec-core/skills\n", result.stdout
        )
        self.assertNotIn("overriding recipe", result.stderr)
        result = self.make(
            "layout",
            env_overrides={
                "PREFIX": "/opt/env",
                "AWPRJ": "suite",
                "ADAPTERDIR": "/env/hooks",
            },
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DATADIR=/opt/env/share/suite/agent-sec-core\n", result.stdout)
        self.assertIn("ADAPTERDIR=/env/hooks\n", result.stdout)
        self.assertIn("SKILLDIR=/opt/env/share/suite/skills\n", result.stdout)
        result = self.make("layout", "PREFIX=/usr", "AWPRJ=anolisa")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("SKILLDIR=/usr/share/anolisa/skills\n", result.stdout)
        result = self.make("layout", "AWPRJ=anolisa", "DATADIR=/srv/data")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("SKILLDIR=/srv/data/skills\n", result.stdout)
        result = self.make("layout", "AWPRJ=anolisa", "SKILLDIR=/srv/skills")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("SKILLDIR=/srv/skills\n", result.stdout)
        for name in (
            "PREFIX",
            "BINDIR",
            "LIBDIR",
            "LIBEXECDIR",
            "DATADIR",
            "ADAPTERDIR",
            "SKILLDIR",
            "UNITDIR",
            "SYSCONFDIR",
            "CONFIGDIR",
        ):
            self.assertNotEqual(self.make("layout", f"{name}=").returncode, 0)
            self.assertNotEqual(
                self.make("layout", env_overrides={name: ""}).returncode, 0
            )
        self.assertEqual(self.make("layout", "AWPRJ=").returncode, 0)
        for profile in ("", "user"):
            self.assertNotEqual(
                self.make("layout", f"INSTALL_PROFILE={profile}").returncode, 0
            )
            self.assertNotEqual(
                self.make(
                    "layout", env_overrides={"INSTALL_PROFILE": profile}
                ).returncode,
                0,
            )

    def test_legacy_stage_skills_keeps_its_build_directory_contract(self):
        source = self.source / "skills/legacy/SKILL.md"
        source.parent.mkdir(parents=True)
        source.write_text("legacy fixture\n")
        result = self.make("stage-skills", "BUILD_DIR=legacy-build")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            (self.source / "legacy-build/skills/legacy/SKILL.md").read_text(),
            "legacy fixture\n",
        )
        default = self.make()
        self.assertEqual(default.returncode, 0, default.stderr)
        self.assertIn("Show this help message", default.stdout)
        self.assertNotIn("cargo build", default.stdout)

    def test_stage_products_without_distribution_or_component_manifest(self):
        stage = self.base / "stage"
        result = self.make("install", f"DESTDIR={stage}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for name in (
            "agent-sec-cli",
            "agent-sec-daemon",
            "agent-sec-daemon-health",
            "linux-sandbox",
        ):
            self.assertTrue((stage / "usr/bin" / name).stat().st_mode & 0o111)
        unit = (stage / "usr/lib/systemd/system/agent-sec-core.service").read_text()
        self.assertIn(
            'ExecStart="/usr/bin/agent-sec-daemon" serve --skillsec-config "/etc/agent-sec/skillsec.json"',
            unit,
        )
        self.assertNotIn(str(stage), unit)
        self.assertTrue((stage / "etc/agent-sec/skillsec.json").is_file())
        self.assertTrue(
            (stage / "usr/share/agent-sec-core/adapters/codex/install.sh").is_file()
        )
        self.assertTrue(
            (stage / "usr/share/agent-sec-core/skills/demo/SKILL.md").is_file()
        )
        self.assertFalse(
            (
                stage
                / "etc/systemd/system/multi-user.target.wants/agent-sec-core.service"
            ).exists()
        )
        self.assertFalse(any(path.name == "anolisa" for path in stage.rglob("*")))
        self.assertFalse((self.source / ".anolisa").exists())
        self.assertFalse((self.source / "packaging/anolisa").exists())

    def test_namespace_and_independent_binary_resource_roots(self):
        stage = self.base / "custom"
        result = self.make(
            "install",
            f"DESTDIR={stage}",
            "AWPRJ=work",
            "PREFIX=/opt/vendor",
            "BINDIR=/srv/bin",
            "ADAPTERDIR=/srv/adapters",
            "SKILLDIR=/srv/skills",
            "UNITDIR=/srv/units",
            "CONFIGDIR=/srv/config",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(
            (stage / "opt/vendor/share/work/agent-sec-core/LICENSE").is_file()
        )
        self.assertTrue((stage / "srv/adapters/openclaw/dist/index.js").is_file())
        self.assertTrue((stage / "srv/skills/demo/SKILL.md").is_file())
        self.assertIn(
            "/srv/bin/agent-sec-daemon",
            (stage / "srv/units/agent-sec-core.service").read_text(),
        )
        self.assertTrue((stage / "srv/config/skillsec.json").is_file())
        self.assertIn(
            '--skillsec-config "/srv/config/skillsec.json"',
            (stage / "srv/units/agent-sec-core.service").read_text(),
        )
        self.assertNotIn(
            str(stage), (stage / "srv/units/agent-sec-core.service").read_text()
        )

    def test_native_system_unit_preserves_default_configuration_resolution(self):
        stage = self.base / "native-system"
        result = self.make("install-systemd-system", f"DESTDIR={stage}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        template = (
            self.source / "packaging/systemd/agent-sec-core-v2.service.in"
        ).read_text()
        self.assertEqual(
            (stage / "usr/lib/systemd/system/agent-sec-core.service").read_text(),
            template.replace("{bindir}", "/usr/bin"),
        )

    def test_v2_copy_targets_keep_rpm_and_source_destinations_separate(self):
        stage = self.base / "binary-copy"
        result = self.make(
            "install-cli-v2",
            "install-cli-v2-source",
            f"DESTDIR={stage}",
            "PREFIX=/opt/ignored",
            "BINDIR=/srv/source-bin",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for directory in (stage / "usr/bin", stage / "srv/source-bin"):
            self.assertEqual(
                {path.name for path in directory.iterdir()},
                {"agent-sec-cli", "agent-sec-daemon", "agent-sec-daemon-health"},
            )
            self.assertTrue(
                all(path.stat().st_mode & 0o111 for path in directory.iterdir())
            )
        self.assertFalse((stage / "opt").exists())

    def test_stage_requires_safe_destdir_before_writing(self):
        for arguments in (
            ("install", "DESTDIR=/"),
            ("install", "DESTDIR=//"),
            ("install", "DESTDIR=///"),
            ("install", f"DESTDIR={self.base}/bad", "AWPRJ=../escape"),
        ):
            result = self.make(*arguments)
            self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.base / "bad").exists())

    def test_stage_does_not_follow_destination_or_resource_links(self):
        outside = self.base / "outside"
        outside.mkdir()
        linked = self.base / "linked"
        linked.symlink_to(outside, target_is_directory=True)
        for destination in (linked, linked / "nested"):
            result = self.make("install", f"DESTDIR={destination}")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("symlink", result.stderr)
        stage = self.base / "stage-links"
        stage.mkdir()
        (stage / "usr").symlink_to(outside, target_is_directory=True)
        result = self.make("install", f"DESTDIR={stage}")
        self.assertNotEqual(result.returncode, 0)
        (self.build / "codex-plugin/escape").symlink_to(
            outside, target_is_directory=True
        )
        result = self.make("install", f"DESTDIR={self.base}/plugins")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(outside.iterdir()))

    def test_legacy_raw_rejects_a_namespace_it_cannot_describe(self):
        for namespace in ("", "work"):
            result = subprocess.run(
                ["bash", str(ROOT / "packaging/raw/package.sh"), "stage"],
                env={**os.environ, "AWPRJ": namespace},
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("raw packages require AWPRJ=anolisa", result.stderr)

    def test_direct_prefix_install_never_builds_registers_or_activates(self):
        prefix = self.base / "direct"
        tools = self.base / "tools"
        tools.mkdir()
        for name in ("cargo", "npm", "systemctl", "setcap", "uv"):
            command = tools / name
            command.write_text("#!/bin/sh\nexit 91\n")
            command.chmod(0o755)
        result = self.make(
            "install",
            f"PREFIX={prefix}",
            "AWPRJ=suite",
            env_overrides={"PATH": str(tools) + os.pathsep + os.environ["PATH"]},
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((prefix / "bin/agent-sec-cli").is_file())
        self.assertTrue((prefix / "etc/agent-sec/skillsec.json").is_file())
        self.assertTrue(
            (prefix / "lib/systemd/system/agent-sec-core.service").is_file()
        )
        unit = (prefix / "lib/systemd/system/agent-sec-core.service").read_text()
        command = next(
            line.removeprefix("ExecStart=")
            for line in unit.splitlines()
            if line.startswith("ExecStart=")
        )
        self.assertEqual(
            shlex.split(command),
            [
                str(prefix / "bin/agent-sec-daemon"),
                "serve",
                "--skillsec-config",
                str(prefix / "etc/agent-sec/skillsec.json"),
            ],
        )
        self.assertTrue((prefix / "share/suite/agent-sec-core/LICENSE").is_file())
        self.assertNotIn("overriding recipe", result.stderr)
        self.assertNotIn("cargo build", result.stdout)

    def test_install_preflights_missing_inputs_before_any_file_write(self):
        missing = self.build / "openclaw-plugin/dist/index.js"
        missing.unlink()
        stage = self.base / "missing"
        result = self.make("install", f"DESTDIR={stage}")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing prebuilt input", result.stderr)
        self.assertFalse(stage.exists())

    def test_shared_destdir_ignores_other_components_symlinks(self):
        stage = self.base / "shared"
        stage.mkdir()
        (stage / "another-component").symlink_to(self.base / "unrelated")
        result = self.make("install", f"DESTDIR={stage}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((stage / "another-component").is_symlink())

    def test_core_install_does_not_require_or_copy_plugin_and_skill_inputs(self):
        for name in (
            "cosh-extension",
            "codex-plugin",
            "qoder-plugin",
            "openclaw-plugin",
            "hermes-plugin",
            "qwen-code-extension",
            "skills",
        ):
            shutil.rmtree(self.build / name)
        stage = self.base / "core-only"
        result = self.make("install-core", f"DESTDIR={stage}", "AWPRJ=suite")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((stage / "usr/bin/agent-sec-cli").is_file())
        self.assertTrue((stage / "usr/bin/linux-sandbox").is_file())
        self.assertTrue(
            (stage / "usr/lib/systemd/system/agent-sec-core.service").is_file()
        )
        self.assertTrue((stage / "etc/agent-sec/skillsec.json").is_file())
        self.assertTrue((stage / "usr/share/suite/agent-sec-core/LICENSE").is_file())
        self.assertFalse((stage / "usr/share/suite/agent-sec-core/adapters").exists())
        self.assertFalse((stage / "usr/share/suite/agent-sec-core/skills").exists())
        self.assertNotIn("overriding recipe", result.stderr)

    def test_plugins_install_requires_no_core_or_skill_inputs(self):
        shutil.rmtree(self.build / "v2")
        (self.build / "linux-sandbox").unlink()
        shutil.rmtree(self.build / "skills")
        (self.source / "LICENSE").unlink()
        shutil.rmtree(self.source / "packaging/systemd")
        stage = self.base / "plugins-only"
        result = self.make("install-plugins", f"DESTDIR={stage}", "AWPRJ=suite")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        adapters = stage / "usr/share/suite/agent-sec-core/adapters"
        self.assertEqual(
            {path.name for path in adapters.iterdir()},
            {"cosh", "codex", "qoder", "openclaw", "hermes", "qwencode"},
        )
        self.assertTrue((adapters / "openclaw/dist/index.js").is_file())
        self.assertFalse((stage / "usr/bin").exists())
        self.assertFalse((stage / "etc").exists())
        self.assertFalse((adapters.parent / "LICENSE").exists())
        self.assertFalse((adapters.parent / "skills").exists())

    def test_skills_install_requires_no_core_or_plugin_inputs(self):
        for path in self.build.iterdir():
            if path.name != "skills":
                if path.is_dir():
                    shutil.rmtree(path)
                else:
                    path.unlink()
        (self.source / "LICENSE").unlink()
        shutil.rmtree(self.source / "packaging/systemd")
        self.artifact("skills/demo/run.sh", "#!/bin/sh\nexit 0\n")
        prefix = self.base / "skills-prefix"
        result = self.make("install-skills", f"PREFIX={prefix}", "AWPRJ=suite")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        skills = prefix / "share/suite/skills"
        self.assertTrue((skills / "demo/SKILL.md").is_file())
        self.assertEqual((skills / "demo/run.sh").stat().st_mode & 0o777, 0o755)
        self.assertFalse((prefix / "bin").exists())
        self.assertFalse((prefix / "etc").exists())
        self.assertEqual({path.name for path in skills.parent.iterdir()}, {"skills"})
        self.assertNotIn("overriding recipe", result.stderr)

    def test_namespaced_skills_install_uses_shared_root(self):
        stage = self.base / "anolisa-skills"
        result = self.make("install-skills", f"DESTDIR={stage}", "AWPRJ=anolisa")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((stage / "usr/share/anolisa/skills/demo/SKILL.md").is_file())
        self.assertFalse((stage / "usr/share/anolisa/agent-sec-core/skills").exists())

    def test_uninstall_skills_uses_matching_layout(self):
        source_skill = self.source / "skills/demo/SKILL.md"
        source_skill.parent.mkdir(parents=True)
        source_skill.write_text("fixture\n")
        for namespace, relative in (
            ("", "usr/local/share/agent-sec-core/skills"),
            ("anolisa", "usr/local/share/anolisa/skills"),
        ):
            stage = self.base / (namespace or "standalone")
            args = (f"DESTDIR={stage}", "PREFIX=/usr/local", f"AWPRJ={namespace}")
            installed = self.make("install-skills", *args)
            self.assertEqual(installed.returncode, 0, installed.stderr)
            skill = stage / relative / "demo/SKILL.md"
            self.assertTrue(skill.is_file())
            untouched = stage / "usr/share/anolisa/skills/demo/SKILL.md"
            untouched.parent.mkdir(parents=True, exist_ok=True)
            untouched.write_text("unrelated\n")
            removed = self.make("uninstall", *args)
            self.assertEqual(removed.returncode, 0, removed.stderr)
            self.assertFalse(skill.exists())
            self.assertEqual(untouched.read_text(), "unrelated\n")

    def test_each_group_preflights_its_inputs_before_any_write(self):
        for group, name in (
            ("core", "v2/bin/agent-sec-daemon"),
            ("plugins", "openclaw-plugin/dist/index.js"),
            ("skills", "skills/demo/SKILL.md"),
        ):
            missing = self.build / name
            saved = self.base / "saved-input"
            missing.replace(saved)
            stage = self.base / (group + "-missing")
            try:
                result = self.make("install-" + group, f"DESTDIR={stage}")
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("missing", result.stderr)
                self.assertFalse(stage.exists())
            finally:
                saved.replace(missing)

    def test_skills_without_any_skill_definition_fail_before_writing(self):
        (self.build / "skills/demo/SKILL.md").unlink()
        self.artifact("skills/demo/README.md", "This is not a skill definition.\n")
        stage = self.base / "no-skill-definition"
        result = self.make("install-skills", f"DESTDIR={stage}")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("skills/*/SKILL.md", result.stderr)
        self.assertFalse(stage.exists())

    def test_build_and_grouped_installs_exclude_retired_stage_entrypoints(self):
        result = self.make("-n", "build")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cd v2 &&", result.stdout)
        self.assertIn("cargo build --release --locked", result.stdout)
        self.assertNotIn("maturin", result.stdout)
        for target in (
            "stage-package",
            "stage-package-core",
            "stage-package-plugin",
            "stage-package-skills",
        ):
            result = self.make(target)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("No rule to make target", result.stderr)

    def test_direct_install_checks_written_paths_without_scanning_shared_parents(self):
        prefix = self.base / "direct-links"
        bindir = prefix / "bin"
        bindir.mkdir(parents=True)
        outside = self.base / "outside-file"
        outside.write_text("leave unchanged\n")
        (bindir / "unrelated-link").symlink_to(outside)
        result = self.make("install", f"PREFIX={prefix}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        (bindir / "agent-sec-cli").unlink()
        (bindir / "agent-sec-cli").symlink_to(outside)
        result = self.make("install", f"PREFIX={prefix}")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symlink", result.stderr)
        self.assertEqual(outside.read_text(), "leave unchanged\n")

    def test_shared_skill_root_only_checks_and_chmods_this_components_files(self):
        prefix = self.base / "shared-prefix"
        skills = prefix / "shared-skills"
        other = skills / "other"
        other.mkdir(parents=True)
        untouched = other / "private.sh"
        untouched.write_text("leave unchanged\n")
        untouched.chmod(0o600)
        (other / "legal-link").symlink_to(untouched)
        self.artifact("skills/demo/run.sh", "#!/bin/sh\nexit 0\n")
        result = self.make("install", f"PREFIX={prefix}", f"SKILLDIR={skills}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(untouched.stat().st_mode & 0o777, 0o600)
        self.assertEqual((skills / "demo/run.sh").stat().st_mode & 0o777, 0o755)


if __name__ == "__main__":
    unittest.main()
