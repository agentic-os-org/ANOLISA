"""Exercise file-only staging without a Linux daemon or framework installation."""

import os
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

COMPONENT = Path(__file__).resolve().parents[1]


class PackagingLayout(unittest.TestCase):
    def assert_installed_cron_uses_path_binary(self, cron: Path, binary: Path) -> None:
        namespace = {"__name__": "installed_cron_fixture"}
        exec(compile(cron.read_text(), str(cron), "exec"), namespace)
        written = []
        namespace["_LOCK_PATH"] = str(self.work / "cron.lock")
        namespace["_read_crontab"] = lambda: ["# keep"]
        namespace["_write_crontab"] = lambda lines: written.extend(lines) or True
        with patch.dict(os.environ, {"PATH": str(binary.parent)}):
            self.assertTrue(namespace["CrontabManager"].sync("/workspace", ["0 * * * *"]))
        self.assertEqual(written[0], "# keep")
        self.assertIn(f" {binary} checkpoint ", written[1])

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="ws-ckpt-layout-")
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name).resolve()
        self.source = self.work / "source"
        self.source.mkdir()
        shutil.copyfile(COMPONENT / "LICENSE", self.source / "LICENSE")
        for name in ("Makefile",):
            shutil.copyfile(COMPONENT / name, self.source / name)
        for relative in ("scripts", "src/skills", "src/plugins/hermes"):
            shutil.copytree(
                COMPONENT / relative,
                self.source / relative,
                ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
            )
        shutil.copyfile(
            COMPONENT / "src/config.toml.sample", self.source / "src/config.toml.sample"
        )
        plugin = self.source / "src/plugins/openclaw"
        (plugin / "dist/src").mkdir(parents=True)
        for name in ("package.json", "package-lock.json", "openclaw.plugin.json"):
            shutil.copyfile(COMPONENT / "src/plugins/openclaw" / name, plugin / name)
        (plugin / "dist/src/index.js").write_text("export default {};\n")
        shutil.copytree(
            COMPONENT / "packaging",
            self.source / "packaging",
            ignore=shutil.ignore_patterns("__pycache__"),
        )
        self.binary = self.work / "renamed-input"
        self.binary.write_text("fixture; never execute\n")
        self.binary.chmod(0o755)
        self.destination = self.work / "stage"
        self.traps = self.work / "traps"
        self.traps.mkdir()
        for command in ("cargo", "npm", "systemctl"):
            trap = self.traps / command
            trap.write_text("#!/bin/sh\necho unexpected-activation-or-build >&2\nexit 91\n")
            trap.chmod(0o755)

    def make(self, target: str, **values: str) -> subprocess.CompletedProcess[str]:
        # Stage these ownership fixtures in the explicit RPM-style /usr layout.
        if any(
            goal in {"install", "install-core", "install-plugins", "install-skills"}
            for goal in target.split()
        ):
            values = {"PREFIX": "/usr", **values}
        settings = {
            "DESTDIR": str(self.destination),
            "BINARY": str(self.binary),
            **values,
        }
        return subprocess.run(
            [
                "make",
                "-s",
                "-C",
                str(self.source),
                *target.split(),
                *(f"{key}={value}" for key, value in settings.items()),
            ],
            env={**os.environ, "PATH": f"{self.traps}:{os.environ['PATH']}"},
            text=True,
            capture_output=True,
        )

    def test_core_install_requires_no_plugin_or_skill_bundle(self) -> None:
        shutil.rmtree(self.source / "src/plugins")
        shutil.rmtree(self.source / "src/skills")
        shutil.rmtree(self.source / "scripts")
        result = self.make("install-core", AWPRJ="owner")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (self.destination / "usr/bin/ws-ckpt").read_bytes(), self.binary.read_bytes()
        )
        self.assertTrue((self.destination / "usr/share/doc/ws-ckpt/LICENSE").is_file())
        self.assertTrue((self.destination / "etc/ws-ckpt/config.toml").is_file())
        unit = (self.destination / "usr/lib/systemd/system/ws-ckpt.service").read_text()
        self.assertIn("ExecStart=/usr/bin/ws-ckpt daemon", unit)
        self.assertFalse((self.destination / "usr/libexec/owner").exists())
        self.assertFalse((self.destination / "usr/share/owner").exists())

    def test_plugins_install_requires_no_core_or_skills(self) -> None:
        self.binary.unlink()
        (self.source / "LICENSE").unlink()
        (self.source / "src/config.toml.sample").unlink()
        (self.source / "packaging/systemd/ws-ckpt.service.in").unlink()
        shutil.rmtree(self.source / "src/skills")
        result = self.make("install-plugins", AWPRJ="owner")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(
            (
                self.destination / "usr/libexec/owner/ws-ckpt/plugins/openclaw/dist/src/index.js"
            ).is_file()
        )
        self.assertTrue(
            (
                self.destination / "usr/share/owner/ws-ckpt/adapters/hermes/install-hermes.sh"
            ).is_file()
        )
        self.assertFalse((self.destination / "usr/bin").exists())
        self.assertFalse((self.destination / "usr/lib/systemd").exists())
        self.assertFalse((self.destination / "usr/share/owner/ws-ckpt/skills").exists())

    def test_skills_install_requires_no_core_or_plugin_files(self) -> None:
        self.binary.unlink()
        (self.source / "LICENSE").unlink()
        (self.source / "src/config.toml.sample").unlink()
        shutil.rmtree(self.source / "packaging/systemd")
        shutil.rmtree(self.source / "src/plugins")
        shutil.rmtree(self.source / "scripts")
        result = self.make("install-skills", AWPRJ="owner")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.destination / "usr/share/owner/ws-ckpt/skills/SKILL.md").is_file())
        self.assertFalse((self.destination / "usr/bin").exists())
        self.assertFalse((self.destination / "usr/libexec").exists())
        self.assertFalse((self.destination / "usr/share/owner/ws-ckpt/adapters").exists())

    def test_complete_install_checks_all_plugin_scripts_before_core_writes(self) -> None:
        (self.source / "scripts/hermes/install-hermes.sh").unlink()
        result = self.make("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("scripts/hermes/install-hermes.sh", result.stderr)
        self.assertNotIn("run make build first", result.stderr)
        self.assertFalse(self.destination.exists())

    def test_complete_install_checks_skills_before_core_writes(self) -> None:
        (self.source / "src/skills/ws-ckpt/SKILL.md").unlink()
        result = self.make("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())

    def test_core_ignores_unselected_plugin_destination_links(self) -> None:
        plugin = self.destination / "usr/libexec/ws-ckpt/plugins"
        plugin.parent.mkdir(parents=True)
        outside = self.work / "outside-plugin"
        outside.mkdir()
        plugin.symlink_to(outside, target_is_directory=True)
        result = self.make("install-core")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.destination / "usr/bin/ws-ckpt").is_file())
        self.assertTrue(plugin.is_symlink())
        self.assertEqual(list(outside.iterdir()), [])

    def test_plugin_and_skill_groups_ignore_unselected_core_destination_links(self) -> None:
        binary = self.destination / "usr/bin/ws-ckpt"
        binary.parent.mkdir(parents=True)
        outside = self.work / "outside-binary"
        outside.write_text("preserve")
        binary.symlink_to(outside)
        for target in ("install-plugins", "install-skills"):
            result = self.make(target)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(binary.is_symlink())
        self.assertEqual(outside.read_text(), "preserve")

    def test_selected_groups_preserve_destination_and_missing_input_checks(self) -> None:
        for target in ("install-core", "install-plugins", "install-skills"):
            for destination in ("/", "relative", f"{self.work}/../escape"):
                with self.subTest(target=target, destination=destination):
                    self.assertNotEqual(self.make(target, DESTDIR=destination).returncode, 0)
                    self.assertFalse(self.destination.exists())
        self.binary.unlink()
        (self.source / "src/plugins/openclaw/dist/src/index.js").unlink()
        (self.source / "src/skills/ws-ckpt/SKILL.md").unlink()
        for target in ("install-core", "install-plugins", "install-skills"):
            with self.subTest(target=target):
                self.assertNotEqual(self.make(target).returncode, 0)
                self.assertFalse(self.destination.exists())

    def test_install_copies_complete_prebuilt_payload_and_overrides(self) -> None:
        result = self.make(
            "install", AWPRJ="custom", BINDIR="/opt/ws/bin", SKILLDIR="/opt/skills/ws-ckpt"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (self.destination / "opt/ws/bin/ws-ckpt").read_bytes(), self.binary.read_bytes()
        )
        unit = (self.destination / "usr/lib/systemd/system/ws-ckpt.service").read_text()
        self.assertIn("ExecStart=/opt/ws/bin/ws-ckpt daemon", unit)
        self.assertIn("ExecReload=/opt/ws/bin/ws-ckpt reload", unit)
        self.assertNotIn("{bindir}", unit)
        self.assertNotIn(str(self.destination), unit)
        self.assertTrue((self.destination / "etc/ws-ckpt/config.toml").is_file())
        self.assertTrue((self.destination / "usr/share/doc/ws-ckpt/LICENSE").is_file())
        self.assertTrue(
            (
                self.destination / "usr/share/custom/ws-ckpt/adapters/openclaw/install-openclaw.sh"
            ).is_file()
        )
        self.assertTrue(
            (
                self.destination / "usr/libexec/custom/ws-ckpt/plugins/openclaw/dist/src/index.js"
            ).is_file()
        )
        self.assertTrue((self.destination / "opt/skills/ws-ckpt/SKILL.md").is_file())
        self.assert_installed_cron_uses_path_binary(
            self.destination / "usr/libexec/custom/ws-ckpt/plugins/hermes/cron.py",
            self.destination / "opt/ws/bin/ws-ckpt",
        )
        self.assertIn("Runtime discovery", result.stdout)

    def test_missing_plugin_bundle_fails_before_copying_core(self) -> None:
        (self.source / "src/plugins/openclaw/dist/src/index.js").unlink()
        result = self.make("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("run make build first", result.stderr)
        self.assertFalse(self.destination.exists())

    def test_explicit_legacy_layout_retains_resource_paths(self) -> None:
        result = self.make("install", PROFILE="legacy-anolisa")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.destination / "usr/local/bin/ws-ckpt").is_file())
        self.assertTrue(
            (
                self.destination
                / "usr/share/anolisa/runtime/ws-ckpt/plugins/openclaw/dist/src/index.js"
            ).is_file()
        )
        self.assertTrue(
            (self.destination / "usr/share/anolisa/components/ws-ckpt/component.toml").is_file()
        )

    def test_explicit_legacy_install_retains_build_profile_behavior(self) -> None:
        result = self.make("-n install-legacy", INSTALL_PROFILE="system")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cargo build", result.stdout)
        self.assertIn("npm install", result.stdout)
        self.assertIn("/usr/local/bin/ws-ckpt", result.stdout)

    def test_direct_install_uses_prefix_and_namespace_without_scanning_root(self) -> None:
        prefix = self.work / "direct"
        result = self.make(
            "install", DESTDIR="", PREFIX=str(prefix), SYSCONFDIR=str(prefix / "etc"), AWPRJ="owner"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((prefix / "bin" / COMPONENT.name).read_bytes(), self.binary.read_bytes())
        self.assertTrue((prefix / "share/owner" / COMPONENT.name).is_dir())
        self.assertTrue((prefix / "etc/ws-ckpt/config.toml").is_file())
        self.assertTrue((prefix / "lib/systemd/system/ws-ckpt.service").is_file())
        self.assertIn("no build, framework registration, or service activation", result.stdout)
        self.assertFalse(self.destination.exists())

    def test_missing_prebuilt_inputs_fail_before_any_installation(self) -> None:
        self.binary.unlink()
        result = self.make("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("run make build first", result.stderr)
        self.assertNotIn("unexpected-activation-or-build", result.stderr)
        self.assertFalse(self.destination.exists())

    def test_removed_stage_targets_are_not_public_entrypoints(self) -> None:
        for target in (
            "stage-package",
            "stage-package-core",
            "stage-package-plugin",
            "stage-package-skills",
        ):
            with self.subTest(target=target):
                result = self.make(target)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("No rule to make target", result.stderr)
        help_result = self.make("help")
        self.assertEqual(help_result.returncode, 0, help_result.stderr)
        self.assertNotIn("stage-package", help_result.stdout)
        self.assertIn("install", help_result.stdout)

    def test_rejects_unsafe_destinations_and_layouts_before_writing(self) -> None:
        for value in ("/", "//", "///", "relative", f"{self.work}/../escape"):
            with self.subTest(destination=value):
                self.assertNotEqual(self.make("install", DESTDIR=value).returncode, 0)
        self.assertNotEqual(self.make("install", BINDIR="/usr/../escape").returncode, 0)
        self.assertNotEqual(self.make("layout", AWPRJ="../../escape").returncode, 0)
        self.assertFalse(self.destination.exists())

    def test_rejects_symlinked_destination_ancestor(self) -> None:
        outside = self.work / "outside"
        outside.mkdir()
        self.destination.mkdir()
        (self.destination / "usr").symlink_to(outside, target_is_directory=True)
        result = self.make("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(list(outside.iterdir()), [])
        (self.destination / "usr").unlink()
        linked = self.work / "linked-parent"
        linked.symlink_to(outside, target_is_directory=True)
        result = self.make("install", DESTDIR=str(linked / "stage"))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(list(outside.iterdir()), [])

    def test_shared_destination_ignores_other_components_links(self) -> None:
        binary_directory = self.destination / "usr/bin"
        binary_directory.mkdir(parents=True)
        outside = self.work / "outside-file"
        outside.write_text("unrelated component data")
        link = binary_directory / "another-component"
        link.symlink_to(outside)
        (self.destination / "another-component-tree").symlink_to(
            self.work, target_is_directory=True
        )
        result = self.make("install")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(link.is_symlink())
        self.assertEqual(outside.read_text(), "unrelated component data")

    def test_direct_install_rejects_linked_own_file_without_overwriting_it(self) -> None:
        prefix = self.work / "direct"
        (prefix / "bin").mkdir(parents=True)
        outside = self.work / "outside-file"
        outside.write_text("preserve")
        (prefix / "bin" / COMPONENT.name).symlink_to(outside)
        result = self.make("install", DESTDIR="", PREFIX=str(prefix))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(outside.read_text(), "preserve")
        self.assertFalse((prefix / "share").exists())

    def test_rejects_paths_that_cannot_form_a_systemd_executable(self) -> None:
        for value in (
            "/opt/owner package/bin",
            "/opt/unit%name/bin",
            "/opt/x\nExecStart=/bin/x",
            "/opt/owner'package/bin",
        ):
            with self.subTest(path=value):
                result = self.make("install", BINDIR=value)
                self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())

    def test_user_install_profile_requires_explicit_legacy_entrypoint(self) -> None:
        result = self.make("install", INSTALL_PROFILE="user")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("INSTALL_PROFILE=system only", result.stderr)
        self.assertFalse(self.destination.exists())

    def test_explicit_environment_overrides_and_empty_namespace(self) -> None:
        environment = {**os.environ, "AWPRJ": "owner", "PREFIX": "/opt/owner"}
        result = subprocess.run(
            ["make", "-s", "-C", str(self.source), "layout"],
            env=environment,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DATADIR=/opt/owner/share/owner/" + COMPONENT.name, result.stdout)
        result = subprocess.run(
            ["make", "-s", "-C", str(self.source), "layout", "AWPRJ=", "PREFIX=/usr"],
            env=environment,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DATADIR=/usr/share/" + COMPONENT.name, result.stdout)
        environment["BINDIR"] = ""
        result = subprocess.run(
            ["make", "-s", "-C", str(self.source), "layout"],
            env=environment,
            text=True,
            capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertNotEqual(self.make("layout", PREFIX="").returncode, 0)

    def test_mixed_goals_keep_legacy_file_helpers_separate(self) -> None:
        environment = {
            **os.environ,
            "HOME": str(self.work / "home"),
            "PATH": f"{self.traps}:{os.environ['PATH']}",
        }
        result = subprocess.run(
            [
                "make",
                "-s",
                "-C",
                str(self.source),
                "install",
                "install-core-files",
                "layout",
                f"DESTDIR={self.destination}",
                f"BINARY={self.binary}",
            ],
            env=environment,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.destination / "usr/local/bin" / COMPONENT.name).is_file())
        legacy_bin = (
            self.work / "home/.local/bin"
            if COMPONENT.name == "agent-memory"
            else Path("/usr/local/bin")
        )
        self.assertTrue((self.destination / str(legacy_bin).lstrip("/") / COMPONENT.name).is_file())
        self.assertIn("DATADIR=/usr/local/share/" + COMPONENT.name, result.stdout)

    def test_root_entry_works_from_archived_source_without_monorepo(self) -> None:
        archive = self.work / "source.tar"
        with tarfile.open(archive, "w") as output:
            output.add(self.source, arcname="component")
        unpacked = self.work / "unpacked"
        with tarfile.open(archive) as source:
            source.extractall(unpacked, filter="data")
        staged = self.work / "unpacked-stage"
        result = subprocess.run(
            [
                "make",
                "-s",
                "-C",
                str(unpacked / "component"),
                "install",
                f"DESTDIR={staged}",
                f"BINARY={self.binary}",
            ],
            env={**os.environ, "PATH": f"{self.traps}:{os.environ['PATH']}"},
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((staged / "usr/local/bin" / COMPONENT.name).is_file())
        self.assertTrue((unpacked / "component/packaging/check-install.py").is_file())
        self.assertFalse((unpacked / "component/packaging/Makefile").exists())

    def test_srpm_source_archive_keeps_included_modules_and_unit(self) -> None:
        for name in ("Cargo.toml", "Cargo.lock"):
            shutil.copyfile(COMPONENT / "src" / name, self.source / "src" / name)
        for name in ("LICENSE", "README.md"):
            shutil.copyfile(COMPONENT / name, self.source / name)
        shutil.copytree(COMPONENT / "src/crates", self.source / "src/crates")
        shutil.copytree(
            COMPONENT / "tests", self.source / "tests", ignore=shutil.ignore_patterns("__pycache__")
        )
        cargo = self.traps / "cargo"
        cargo.write_text(
            '#!/bin/sh\n[ "$1" = vendor ] || exit 91\nmkdir -p vendor\nprintf fixture > vendor/fixture\n'
        )
        rpm = self.traps / "rpmbuild"
        rpm.write_text(
            "#!/usr/bin/env python3\n"
            "from pathlib import Path\nimport re, sys\n"
            "spec = Path(sys.argv[-1])\ntext = spec.read_text()\n"
            "name = re.search(r'^Name:\\s+(\\S+)', text, re.M).group(1)\n"
            "version = re.search(r'^Version:\\s+(\\S+)', text, re.M).group(1)\n"
            "(spec.parents[1] / 'SRPMS' / f'{name}-{version}-1.src.rpm').write_text('fixture')\n"
        )
        rpm.chmod(0o755)
        result = self.make("srpm")
        self.assertEqual(result.returncode, 0, result.stderr)
        archives = list((self.source / "rpmbuild/SOURCES").glob("ws-ckpt-*.tar.gz"))
        source_archive = next(path for path in archives if "vendor" not in path.name)
        unpacked = self.work / "srpm-source"
        with tarfile.open(source_archive) as archive:
            archive.extractall(unpacked, filter="data")
        source = next(path for path in unpacked.iterdir() if path.is_dir())
        self.assertTrue((source / "packaging/rpm/ws-ckpt.spec.in").is_file())
        self.assertTrue((source / "packaging/systemd/ws-ckpt.service.in").is_file())
        self.assertTrue((source / "packaging/install.mk").is_file())
        self.assertTrue((source / "packaging/anolisa/raw/component.toml").is_file())
        self.assertTrue((source / "packaging/anolisa/rpm/component.toml").is_file())
        self.assertFalse((source / ".anolisa").exists())
        self.assertFalse((source / "component.toml").exists())
        result = subprocess.run(
            ["make", "-s", "-C", str(source), "layout"], text=True, capture_output=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DATADIR=/usr/local/share/ws-ckpt", result.stdout)

    def test_raw_archive_rejects_independent_namespace(self) -> None:
        result = subprocess.run(
            ["bash", str(COMPONENT / "packaging/raw/package.sh"), "package"],
            env={**os.environ, "AWPRJ": ""},
            text=True,
            capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("raw payload requires AWPRJ=anolisa", result.stderr)


if __name__ == "__main__":
    unittest.main()
