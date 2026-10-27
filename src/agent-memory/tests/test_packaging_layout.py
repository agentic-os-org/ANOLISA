"""Check prebuilt installation without changing memory's existing runtime policy."""

import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

COMPONENT = Path(__file__).resolve().parents[1]


class PackagingLayout(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="agent-memory-layout-")
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name).resolve()
        self.source = self.work / "source"
        self.source.mkdir()
        shutil.copyfile(COMPONENT / "LICENSE", self.source / "LICENSE")
        for name in ("Makefile", "Cargo.toml"):
            shutil.copyfile(COMPONENT / name, self.source / name)
        for name in ("config", "packaging", "adapters"):
            shutil.copytree(
                COMPONENT / name,
                self.source / name,
                ignore=shutil.ignore_patterns("node_modules", "dist", "__pycache__", "coverage"),
            )
        plugin = self.source / "adapters/agent-memory/openclaw"
        (plugin / "dist").mkdir()
        (plugin / "dist/index.js").write_text("export default {};\n")
        (plugin / "node_modules").mkdir()
        (plugin / "node_modules/should-not-ship").write_text("development dependency\n")
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
        settings = {
            "DESTDIR": str(self.destination),
            "BINARY": str(self.binary),
            "PREFIX": "/usr",
            "INSTALL_PROFILE": "system",
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

    def test_core_installs_without_any_plugin_inputs(self) -> None:
        shutil.rmtree(self.source / "adapters")
        result = self.make("install-core")
        self.assertEqual(result.returncode, 0, result.stderr)
        files = {
            path.relative_to(self.destination).as_posix()
            for path in self.destination.rglob("*")
            if path.is_file()
        }
        self.assertEqual(
            files,
            {
                "usr/bin/agent-memory",
                "usr/lib/systemd/user/anolisa-memory@.service",
                "usr/share/agent-memory/default.toml",
                "usr/share/agent-memory/mcp-servers/agent-memory.json",
                "usr/share/doc/agent-memory/LICENSE",
            },
        )
        self.assertFalse((self.destination / "usr/libexec").exists())
        self.assertNotIn("unexpected-activation-or-build", result.stderr)

    def test_plugins_install_without_core_inputs(self) -> None:
        self.binary.unlink()
        (self.source / "LICENSE").unlink()
        shutil.rmtree(self.source / "config")
        shutil.rmtree(self.source / "packaging/systemd")
        result = self.make("install-plugins", AWPRJ="suite")
        self.assertEqual(result.returncode, 0, result.stderr)
        adapter = self.destination / "usr/libexec/suite/agent-memory/adapters"
        self.assertTrue((adapter / "manifest.json").is_file())
        self.assertTrue((adapter / "openclaw/dist/index.js").is_file())
        self.assertTrue(os.access(adapter / "openclaw/scripts/install.sh", os.X_OK))
        self.assertFalse((adapter / "openclaw/node_modules").exists())
        self.assertFalse((adapter / "openclaw/src").exists())
        self.assertFalse((self.destination / "usr/bin").exists())
        self.assertFalse((self.destination / "usr/lib/systemd").exists())
        self.assertFalse((self.destination / "usr/share").exists())
        self.assertNotIn("unexpected-activation-or-build", result.stderr)

    def test_missing_plugin_input_blocks_complete_install_before_core_writes(self) -> None:
        (self.source / "adapters/agent-memory/openclaw/dist/index.js").unlink()
        result = self.make("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())
        self.assertNotIn("unexpected-activation-or-build", result.stderr)

    def test_group_install_rejects_its_missing_input_without_writing(self) -> None:
        self.binary.unlink()
        result = self.make("install-core")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())
        (self.source / "adapters/agent-memory/openclaw/dist/index.js").unlink()
        result = self.make("install-plugins")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())

    def test_core_ignores_unselected_plugin_destination_links(self) -> None:
        adapter = self.destination / "usr/libexec/agent-memory/adapters"
        adapter.parent.mkdir(parents=True)
        outside = self.work / "outside-plugin"
        outside.mkdir()
        adapter.symlink_to(outside, target_is_directory=True)
        result = self.make("install-core")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.destination / "usr/bin/agent-memory").is_file())
        self.assertTrue(adapter.is_symlink())
        self.assertEqual(list(outside.iterdir()), [])

    def test_plugins_ignore_unselected_core_destination_links(self) -> None:
        binary = self.destination / "usr/bin/agent-memory"
        binary.parent.mkdir(parents=True)
        outside = self.work / "outside-binary"
        outside.write_text("preserve")
        binary.symlink_to(outside)
        result = self.make("install-plugins")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(binary.is_symlink())
        self.assertEqual(outside.read_text(), "preserve")
        self.assertTrue(
            (self.destination / "usr/libexec/agent-memory/adapters/manifest.json").is_file()
        )

    def test_selected_groups_preserve_destination_validation(self) -> None:
        for target in ("install-core", "install-plugins"):
            for destination in ("/", "relative", f"{self.work}/../escape"):
                with self.subTest(target=target, destination=destination):
                    result = self.make(target, DESTDIR=destination)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(self.destination.exists())
        self.assertNotEqual(self.make("install-skills").returncode, 0)

    def test_install_copies_complete_prebuilt_payload_and_keeps_runtime_policy(self) -> None:
        result = self.make("install", AWPRJ="custom", BINDIR="/opt/memory/bin")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (self.destination / "opt/memory/bin/agent-memory").read_bytes(),
            self.binary.read_bytes(),
        )
        unit = (self.destination / "usr/lib/systemd/user/anolisa-memory@.service").read_text()
        self.assertIn("ExecStart=/opt/memory/bin/agent-memory serve", unit)
        self.assertNotIn("{bindir}", unit)
        descriptor = json.loads(
            (
                self.destination / "usr/share/custom/agent-memory/mcp-servers/agent-memory.json"
            ).read_text()
        )
        self.assertEqual(descriptor["command"], "/opt/memory/bin/agent-memory")
        self.assertEqual(
            json.loads((self.source / "config/mcp-server.json").read_text())["command"],
            "/usr/bin/agent-memory",
        )
        self.assertIn("Environment=MEMORY_SESSION_DIR=%t/anolisa/sessions/%i", unit)
        config = (self.destination / "usr/share/custom/agent-memory/default.toml").read_text()
        self.assertIn('base_dir = "~/.anolisa/memory"', config)
        adapter = self.destination / "usr/libexec/custom/agent-memory/adapters"
        self.assertTrue((adapter / "manifest.json").is_file())
        self.assertTrue((adapter / "openclaw/dist/index.js").is_file())
        self.assertTrue(os.access(adapter / "openclaw/scripts/install.sh", os.X_OK))
        self.assertFalse((adapter / "openclaw/node_modules").exists())
        self.assertFalse((adapter / "openclaw/src").exists())
        self.assertTrue((self.destination / "usr/share/doc/agent-memory/LICENSE").is_file())

    def test_native_unit_matches_the_default_template_rendering(self) -> None:
        native = (COMPONENT / "packaging/systemd/anolisa-memory@.service").read_text()
        template = (COMPONENT / "packaging/systemd/anolisa-memory@.service.in").read_text()
        self.assertIn("ExecStart=/usr/bin/agent-memory serve", native)
        self.assertIn("ExecStart={bindir}/agent-memory serve", template)
        self.assertEqual(template.replace("{bindir}", "/usr/bin"), native)
        result = self.make("install")
        self.assertEqual(result.returncode, 0, result.stderr)
        installed = self.destination / "usr/lib/systemd/user/anolisa-memory@.service"
        self.assertEqual(installed.read_text(), native)

    def test_missing_service_template_fails_before_installation(self) -> None:
        (self.source / "packaging/systemd/anolisa-memory@.service.in").unlink()
        result = self.make("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("anolisa-memory@.service.in", result.stderr)
        self.assertFalse(self.destination.exists())

    def test_explicit_legacy_layout_retains_resource_paths(self) -> None:
        result = self.make("install", PROFILE="legacy-anolisa")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(
            (self.destination / "usr/share/anolisa/agent-memory/default.toml").is_file()
        )
        self.assertTrue(
            (
                self.destination / "usr/share/anolisa/adapters/agent-memory/openclaw/dist/index.js"
            ).is_file()
        )

    def test_rejects_linked_payload_without_overwriting_existing_install(self) -> None:
        index = self.source / "adapters/agent-memory/openclaw/dist/index.js"
        index.unlink()
        index.symlink_to(self.binary)
        self.destination.mkdir()
        marker = self.destination / "marker"
        marker.write_text("preserve me\n")
        result = self.make("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symlink", result.stderr)
        self.assertEqual(marker.read_text(), "preserve me\n")
        self.assertFalse((self.destination / "usr").exists())

    def test_explicit_legacy_install_retains_user_profile(self) -> None:
        result = self.make("-n install-legacy", INSTALL_PROFILE="user", PREFIX="/opt/legacy-user")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("/opt/legacy-user/share/anolisa/adapters/agent-memory", result.stdout)

    def test_direct_install_uses_prefix_and_namespace_without_scanning_root(self) -> None:
        prefix = self.work / "direct"
        result = self.make("install", DESTDIR="", PREFIX=str(prefix), AWPRJ="owner")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((prefix / "bin" / COMPONENT.name).read_bytes(), self.binary.read_bytes())
        self.assertTrue((prefix / "share/owner" / COMPONENT.name).is_dir())
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

    def test_user_install_uses_xdg_resources_and_explicit_binary_prefix(self) -> None:
        home = self.work / "home"
        prefix = self.work / "custom-prefix"
        data = self.work / "xdg-data"
        result = self.make(
            "install",
            INSTALL_PROFILE="user",
            PREFIX=str(prefix),
            AWPRJ="suite",
            HOME=str(home),
            XDG_DATA_HOME=str(data),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        staged_prefix = self.destination / str(prefix).lstrip("/")
        staged_data = self.destination / str(data).lstrip("/")
        self.assertTrue((staged_prefix / "bin/agent-memory").is_file())
        self.assertFalse((staged_prefix / "libexec").exists())
        self.assertTrue(
            (staged_data / "suite/agent-memory/adapters/openclaw/dist/index.js").is_file()
        )
        self.assertTrue((staged_data / "suite/agent-memory/default.toml").is_file())
        self.assertTrue((staged_data / "doc/agent-memory/LICENSE").is_file())
        unit = (staged_data / "systemd/user/anolisa-memory@.service").read_text()
        self.assertIn(f"ExecStart={prefix}/bin/agent-memory serve", unit)
        descriptor = json.loads(
            (staged_data / "suite/agent-memory/mcp-servers/agent-memory.json").read_text()
        )
        self.assertEqual(descriptor["command"], f"{prefix}/bin/agent-memory")
        self.assertNotIn(str(self.destination), unit)
        self.assertFalse((staged_prefix / "share").exists())
        for path in (home, prefix, data):
            self.assertFalse(path.exists(), str(path))

    def test_user_defaults_use_home_prefix_and_xdg_data_fallback(self) -> None:
        home = self.work / "home"
        for xdg in (None, ""):
            with self.subTest(xdg=xdg):
                environment = {
                    key: value
                    for key, value in os.environ.items()
                    if key
                    not in ("PREFIX", "XDG_DATA_HOME", "INSTALL_PROFILE", "AWPRJ", "MAKEFLAGS")
                }
                environment.update(HOME=str(home), PATH=f"{self.traps}:{os.environ['PATH']}")
                if xdg is not None:
                    environment["XDG_DATA_HOME"] = xdg
                result = subprocess.run(
                    [
                        "make",
                        "-s",
                        "-C",
                        str(self.source),
                        "install",
                        "layout",
                        "INSTALL_PROFILE=user",
                        f"DESTDIR={self.destination}",
                        f"BINARY={self.binary}",
                    ],
                    env=environment,
                    text=True,
                    capture_output=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"PREFIX={home}/.local", result.stdout)
                self.assertIn(f"DATADIR={home}/.local/share/agent-memory", result.stdout)
                staged_home = self.destination / str(home).lstrip("/")
                self.assertTrue((staged_home / ".local/bin/agent-memory").is_file())
                self.assertTrue(
                    (
                        staged_home / ".local/share/agent-memory/adapters/openclaw/dist/index.js"
                    ).is_file()
                )
                self.assertTrue(
                    (staged_home / ".local/share/systemd/user/anolisa-memory@.service").is_file()
                )
                self.assertFalse(home.exists())
                shutil.rmtree(self.destination)

    def test_user_plugin_destination_override_takes_precedence_over_xdg(self) -> None:
        data = self.work / "xdg-data"
        result = self.make(
            "install-plugins",
            INSTALL_PROFILE="user",
            XDG_DATA_HOME=str(data),
            ADAPTERDIR="/opt/custom-memory-plugin",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(
            (self.destination / "opt/custom-memory-plugin/openclaw/dist/index.js").is_file()
        )
        self.assertFalse((self.destination / str(data).lstrip("/")).exists())
        self.assertFalse((self.destination / "usr/libexec").exists())
        self.assertFalse(data.exists())

    def test_system_install_ignores_user_xdg_roots(self) -> None:
        home = self.work / "home"
        data = self.work / "xdg-data"
        result = self.make(
            "install",
            INSTALL_PROFILE="system",
            PREFIX="/usr",
            HOME=str(home),
            XDG_DATA_HOME=str(data),
            XDG_CONFIG_HOME=str(self.work / "xdg-config"),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(
            (self.destination / "usr/lib/systemd/user/anolisa-memory@.service").is_file()
        )
        self.assertTrue(
            (self.destination / "usr/share/agent-memory/mcp-servers/agent-memory.json").is_file()
        )
        self.assertFalse((self.destination / str(data).lstrip("/")).exists())
        self.assertFalse(home.exists())
        self.assertFalse(data.exists())

    def test_default_profile_follows_effective_uid_and_accepts_overrides(self) -> None:
        home = self.work / "home"
        home.mkdir()
        data = self.work / "user-data"
        tools = self.work / "uid-tools"
        tools.mkdir()
        identity = tools / "id"
        identity.write_text('#!/bin/sh\nprintf "%s\\n" "$DELIVERY_TEST_UID"\n')
        identity.chmod(0o755)
        environment = {
            key: value
            for key, value in os.environ.items()
            if key not in ("PREFIX", "INSTALL_PROFILE", "AWPRJ", "MAKEFLAGS", "MFLAGS")
        }
        environment.update(HOME=str(home), XDG_DATA_HOME=str(data))
        for uid in (0, 1001):
            for explicit in (None, "system", "user"):
                with self.subTest(uid=uid, explicit=explicit):
                    arguments = [] if explicit is None else [f"INSTALL_PROFILE={explicit}"]
                    staged = self.work / f"uid-{uid}-{explicit or 'default'}"
                    result = subprocess.run(
                        [
                            "make",
                            "-s",
                            "-C",
                            str(self.source),
                            "install-core",
                            "layout",
                            f"DESTDIR={staged}",
                            f"BINARY={self.binary}",
                            *arguments,
                        ],
                        env={
                            **environment,
                            "PATH": f"{tools}:{os.environ['PATH']}",
                            "DELIVERY_TEST_UID": str(uid),
                        },
                        text=True,
                        capture_output=True,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    selected = explicit or ("system" if uid == 0 else "user")
                    prefix = "/usr/local" if selected == "system" else str(home / ".local")
                    resource = (
                        "/usr/local/share/agent-memory"
                        if selected == "system"
                        else str(data / "agent-memory")
                    )
                    self.assertIn(f"INSTALL_PROFILE={selected}\n", result.stdout)
                    self.assertIn(f"PREFIX={prefix}\n", result.stdout)
                    self.assertIn(f"DATADIR={resource}\n", result.stdout)
                    self.assertTrue((staged / prefix.lstrip("/") / "bin/agent-memory").is_file())
                    self.assertTrue(
                        (staged / resource.lstrip("/") / "mcp-servers/agent-memory.json").is_file()
                    )
        result = subprocess.run(
            ["make", "-s", "-C", str(self.source), "layout"],
            env=environment,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        selected = "system" if os.geteuid() == 0 else "user"
        self.assertIn(f"INSTALL_PROFILE={selected}\n", result.stdout)

    def test_invalid_profile_or_relative_user_data_root_fails_before_writes(self) -> None:
        for values in (
            {"INSTALL_PROFILE": "unknown"},
            {"INSTALL_PROFILE": "user", "XDG_DATA_HOME": "relative"},
        ):
            with self.subTest(values=values):
                result = self.make("install", **values)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.destination.exists())

    def test_explicit_environment_overrides_and_empty_namespace(self) -> None:
        environment = {
            **os.environ,
            "AWPRJ": "owner",
            "PREFIX": "/opt/owner",
            "INSTALL_PROFILE": "system",
        }
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
        identity = self.traps / "id"
        identity.write_text("#!/bin/sh\nprintf '0\\n'\n")
        identity.chmod(0o755)
        environment = {
            **os.environ,
            "HOME": str(self.work / "home"),
            "PATH": f"{self.traps}:{os.environ['PATH']}",
        }
        environment.pop("INSTALL_PROFILE", None)
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
                "INSTALL_PROFILE=system",
                "PREFIX=/usr",
                f"DESTDIR={staged}",
                f"BINARY={self.binary}",
            ],
            env={**os.environ, "PATH": f"{self.traps}:{os.environ['PATH']}"},
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((staged / "usr/bin" / COMPONENT.name).is_file())
        self.assertTrue((unpacked / "component/packaging/check-install.py").is_file())
        self.assertFalse((unpacked / "component/packaging/Makefile").exists())

    def mock_plugin_tools(self) -> None:
        import tomllib

        version = tomllib.loads((self.source / "Cargo.toml").read_text())["package"]["version"]
        jq = self.traps / "jq"
        jq.write_text(f"#!/bin/sh\nprintf '%s\\n' '{version}'\n")
        jq.chmod(0o755)
        npm = self.traps / "npm"
        npm.write_text(
            "#!/bin/sh\n"
            'if [ "$1" = run ] && [ "$2" = build ]; then\n'
            "  mkdir -p dist; printf 'export default {};\\n' > dist/index.js\n"
            "fi\nexit 0\n"
        )

    def test_standalone_plugin_build_does_not_require_anolisa_contracts(self) -> None:
        self.mock_plugin_tools()
        shutil.rmtree(self.source / "packaging/anolisa")
        result = self.make("build-openclaw-plugin")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.source / ".anolisa").exists())
        self.assertFalse((self.source / "target/packaging/anolisa").exists())
        self.assertNotIn("Generating component contract", result.stdout)

    def test_explicit_rpm_contract_generation_uses_build_output(self) -> None:
        result = self.make("generate-component-contract")
        self.assertEqual(result.returncode, 0, result.stderr)
        contract = self.source / "target/packaging/anolisa/rpm/component.toml"
        self.assertTrue(contract.is_file())
        self.assertNotIn("@VERSION@", contract.read_text())
        self.assertFalse((self.source / "packaging/anolisa/rpm/component.toml").exists())
        self.assertFalse((self.source / ".anolisa").exists())

    def test_dist_exports_generated_rpm_metadata_into_source_archive(self) -> None:
        self.mock_plugin_tools()
        for name in ("Cargo.lock", "CHANGELOG.md", ".gitignore"):
            shutil.copyfile(COMPONENT / name, self.source / name)
        for name in ("src", "examples", "tests"):
            shutil.copytree(
                COMPONENT / name, self.source / name, ignore=shutil.ignore_patterns("__pycache__")
            )
        cargo = self.traps / "cargo"
        cargo.write_text(
            "#!/bin/sh\n"
            'case "$1" in\n'
            "clean) exit 0;;\n"
            'vendor) mkdir -p "$2"; printf fixture > "$2/fixture";;\n'
            "*) exit 91;;\nesac\n"
        )
        result = self.make("dist")
        self.assertEqual(result.returncode, 0, result.stderr)
        archive = next(
            path for path in (self.source / "dist").glob("*.tar.gz") if "vendor" not in path.name
        )
        unpacked = self.work / "dist-source"
        with tarfile.open(archive) as source:
            source.extractall(unpacked, filter="data")
        root = next(path for path in unpacked.iterdir() if path.is_dir())
        rpm_contract = root / "packaging/anolisa/rpm/component.toml"
        self.assertTrue(rpm_contract.is_file())
        self.assertNotIn("@VERSION@", rpm_contract.read_text())
        self.assertTrue((root / "packaging/anolisa/raw/component.toml.in").is_file())
        self.assertFalse((root / ".anolisa").exists())
        result = subprocess.run(
            ["make", "-s", "-C", str(root), "layout"], text=True, capture_output=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_raw_archive_rejects_independent_namespace(self) -> None:
        result = subprocess.run(
            [
                sys.executable,
                str(COMPONENT / "packaging/raw/package.py"),
                "--bin-dir",
                str(self.work),
                "--plugin-dir",
                str(self.work),
                "--metadata",
                str(self.work / "unused.toml"),
                "--target-arch",
                "x86_64",
                "--output-dir",
                str(self.destination),
                "--epoch",
                "0",
            ],
            env={**os.environ, "AWPRJ": ""},
            text=True,
            capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("raw payload requires AWPRJ=anolisa", result.stderr)
        self.assertFalse(self.destination.exists())


if __name__ == "__main__":
    unittest.main()
