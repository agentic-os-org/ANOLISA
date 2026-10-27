"""Exercise standalone file installation with prebuilt, attested fixtures only."""

import hashlib
import json
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
            "agentsight.json",
            "README.md",
            "README_zh.md",
            "packaging/build.mk",
            "packaging/install.mk",
            "packaging/delivery.mk",
            "packaging/validate-stage.py",
            "packaging/rpm/stage-rpm-payload.sh",
            "scripts/copy-enforcer.py",
            "packaging/systemd/render.py",
            "packaging/systemd/agentsight-start.sh.in",
            "packaging/systemd/agentsight.service.in",
            "packaging/systemd/agentsight-enforcer.service.in",
            "dsh-plugin/package.json",
            "dsh-plugin/cordis.patch.yml",
        ):
            destination = self.source / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / name, destination)
        self.binary = self.source / "target/release/agentsight"
        self.binary.parent.mkdir(parents=True)
        self.binary.write_text("#!/bin/sh\nexit 0\n")
        self.binary.chmod(0o755)
        self.enforcer = self.base / "private/enforcer"
        self.enforcer.parent.mkdir()
        self.enforcer.write_text("#!/bin/sh\nexit 0\n")
        self.enforcer.chmod(0o755)
        self.receipt = Path(str(self.enforcer) + ".sha256")
        self.receipt.write_text(hashlib.sha256(self.enforcer.read_bytes()).hexdigest())
        plugin = self.source / "dsh-plugin/lib/index.js"
        plugin.parent.mkdir()
        plugin.write_text("export default {};\n")
        types = plugin.parent / "types/index.d.ts"
        types.parent.mkdir()
        types.write_text("export declare const fixture: unknown;\n")

    def make(self, *arguments, env_overrides=None, default_layout=False):
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
            "AGENTSIGHT_BIN",
            "ENFORCER_BIN",
            "INSTALL_PROFILE",
            "SERVICE_BINDIR",
            "START_BINARY",
            "START_CONFIG",
            "SYSTEMD_SYSTEM_DIR",
            "AGENTSIGHT_CONFIG_DIR",
        ):
            environment.pop(name, None)
        if env_overrides:
            environment.update(env_overrides)
        # Installation fixtures explicitly exercise system-package /usr paths.
        if (
            not default_layout
            and any(
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
        self.assertIn("BINDIR=/usr/local/bin\n", result.stdout)
        self.assertIn("LIBDIR=/usr/local/lib/agentsight\n", result.stdout)
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
        self.assertIn("DATADIR=/opt/env/share/suite/agentsight\n", result.stdout)
        self.assertIn("ADAPTERDIR=/env/hooks\n", result.stdout)
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
            self.assertNotEqual(self.make("layout", env_overrides={name: ""}).returncode, 0)
        self.assertEqual(self.make("layout", "AWPRJ=").returncode, 0)
        for profile in ("", "user"):
            self.assertNotEqual(self.make("layout", f"INSTALL_PROFILE={profile}").returncode, 0)
            self.assertNotEqual(
                self.make("layout", env_overrides={"INSTALL_PROFILE": profile}).returncode,
                0,
            )
        default = self.make("-n")
        self.assertEqual(default.returncode, 0, default.stderr)
        self.assertIn("cargo build --release", default.stdout)
        self.assertIn("./scripts/build-enforcer.sh", default.stdout)
        self.assertNotIn("validate-stage.py", default.stdout)

    def test_stage_core_and_inactive_plugin_without_the_distribution_tree(self):
        stage = self.base / "stage"
        result = self.make("install", f"DESTDIR={stage}", f"ENFORCER_BIN={self.enforcer}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            (stage / "usr/bin/agentsight-enforcer").read_bytes(),
            self.enforcer.read_bytes(),
        )
        self.assertIn(
            "ExecStart=/usr/bin/agentsight-start",
            (stage / "usr/lib/systemd/system/agentsight.service").read_text(),
        )
        self.assertIn(
            "/usr/bin/agentsight trace",
            (stage / "usr/bin/agentsight-start").read_text(),
        )
        self.assertTrue((stage / "etc/agentsight/config.json").is_file())
        launcher = (stage / "usr/bin/agentsight-start").read_text()
        self.assertIn("trace --config /etc/agentsight/config.json &", launcher)
        self.assertIn("serve --host 0.0.0.0 --config /etc/agentsight/config.json &", launcher)
        self.assertNotIn(str(stage), launcher)
        self.assertTrue((stage / "usr/share/agentsight/adapters/dsh/lib/index.js").is_file())
        self.assertFalse(
            (stage / "etc/systemd/system/multi-user.target.wants/agentsight.service").exists()
        )
        self.assertFalse(any(path.name == "anolisa" for path in stage.rglob("*")))
        self.assertFalse((self.source / "packaging/anolisa").exists())

    def test_namespace_and_independent_binary_resource_roots(self):
        stage = self.base / "custom"
        result = self.make(
            "install",
            f"DESTDIR={stage}",
            f"ENFORCER_BIN={self.enforcer}",
            "AWPRJ=work",
            "PREFIX=/opt/vendor",
            "BINDIR=/srv/bin",
            "ADAPTERDIR=/srv/adapters",
            "UNITDIR=/srv/units",
            "CONFIGDIR=/srv/config",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((stage / "opt/vendor/share/work/agentsight/LICENSE").is_file())
        self.assertTrue((stage / "srv/adapters/dsh/lib/index.js").is_file())
        self.assertIn(
            "ExecStart=/srv/bin/agentsight-enforcer",
            (stage / "srv/units/agentsight-enforcer.service").read_text(),
        )
        self.assertIn(
            "/srv/bin/agentsight serve",
            (stage / "srv/bin/agentsight-start").read_text(),
        )
        self.assertTrue((stage / "srv/config/config.json").is_file())
        launcher = (stage / "srv/bin/agentsight-start").read_text()
        self.assertIn("trace --config /srv/config/config.json &", launcher)
        self.assertIn("serve --host 0.0.0.0 --config /srv/config/config.json &", launcher)
        self.assertNotIn(str(stage), launcher)

    def test_default_uninstall_matches_install_without_removing_system_package_files(self):
        stage = self.base / "default-install"
        other_files = (
            "usr/bin/agentsight",
            "usr/lib/systemd/system/agentsight.service",
            "usr/lib/systemd/system/agentsight-enforcer.service",
            "etc/agentsight/config.json",
            "usr/share/agentsight/adapters/dsh/lib/index.js",
            "usr/share/agentsight/adapters/dsh/lib/types/index.d.ts",
            "home/user/.config/dsh/config.yml",
        )
        for name in other_files:
            destination = stage / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text("separate system installation\n")
        result = self.make(
            "install",
            f"DESTDIR={stage}",
            f"ENFORCER_BIN={self.enforcer}",
            default_layout=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        selected = (
            [
                stage / "usr/local/bin" / name
                for name in ("agentsight", "agentsight-enforcer", "agentsight-start")
            ]
            + [
                stage / "usr/local/lib/systemd/system" / name
                for name in ("agentsight.service", "agentsight-enforcer.service")
            ]
            + [
                stage / "usr/local/etc/agentsight/config.json",
                stage / "usr/local/share/agentsight/LICENSE",
            ]
        )
        plugin = stage / "usr/local/share/agentsight/adapters/dsh"
        selected += [
            plugin / name
            for name in ("package.json", "cordis.patch.yml", "lib/index.js", "lib/types/index.d.ts")
        ]
        self.assertTrue(all(path.is_file() for path in selected))
        self.binary.unlink()
        self.enforcer.unlink()
        self.receipt.unlink()
        (self.source / "agentsight.json").unlink()
        for _ in range(2):
            result = self.make("uninstall", f"DESTDIR={stage}")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse(any(path.exists() for path in selected))
            self.assertFalse(plugin.exists())
            for name in other_files:
                self.assertEqual((stage / name).read_text(), "separate system installation\n")

    def test_uninstall_honors_selected_directories_and_leaves_sibling_files(self):
        stage = self.base / "custom-uninstall"
        arguments = (
            f"DESTDIR={stage}",
            "PREFIX=/opt/owner",
            "BINDIR=/srv/bin",
            "UNITDIR=/srv/units",
            "CONFIGDIR=/srv/config",
        )
        result = self.make("install", *arguments, f"ENFORCER_BIN={self.enforcer}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for directory in ("srv/bin", "srv/units", "srv/config"):
            (stage / directory / "other-component").write_text("preserve\n")
        plugin = stage / "opt/owner/share/agentsight/adapters/dsh"
        extra = plugin / "lib/user/nested/custom.json"
        extra.parent.mkdir(parents=True)
        extra.write_text("user content\n")
        outside = self.base / "user-link-target"
        outside.write_text("outside content\n")
        link = plugin / "lib/user-link"
        link.symlink_to(outside)
        result = self.make("uninstall", *arguments)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for directory in ("srv/bin", "srv/units", "srv/config"):
            self.assertEqual(
                [path.name for path in (stage / directory).iterdir()], ["other-component"]
            )
        self.assertEqual(extra.read_text(), "user content\n")
        self.assertTrue(link.is_symlink())
        self.assertEqual(outside.read_text(), "outside content\n")
        for name in ("package.json", "cordis.patch.yml", "lib/index.js", "lib/types/index.d.ts"):
            self.assertFalse((plugin / name).exists())

    def test_uninstall_preserves_legacy_user_and_explicit_system_path_arguments(self):
        for profile in ("user", "system"):
            with self.subTest(profile=profile):
                stage = self.base / ("legacy-uninstall-" + profile)
                bindir = "/opt/legacy/bin"
                unitdir = "/opt/legacy/units"
                configdir = "/opt/legacy/settings"
                paths = [
                    f"{bindir}/{name}"
                    for name in ("agentsight", "agentsight-enforcer", "agentsight-start")
                ]
                paths += [
                    f"{unitdir}/{name}"
                    for name in ("agentsight.service", "agentsight-enforcer.service")
                ]
                paths += [f"{configdir}/config.json"]
                for name in paths:
                    target = stage / name.lstrip("/")
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text("legacy fixture\n")
                result = self.make(
                    "uninstall",
                    f"DESTDIR={stage}",
                    f"INSTALL_PROFILE={profile}",
                    "PREFIX=/opt/legacy",
                    f"BINDIR={bindir}",
                    f"SYSTEMD_SYSTEM_DIR={unitdir}",
                    f"AGENTSIGHT_CONFIG_DIR={configdir}",
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertFalse(any(path.is_file() for path in stage.rglob("*")))

    def test_uninstall_preflights_linked_ancestors_before_removing_any_file(self):
        for directory, payload in (
            ("usr/local/lib/systemd/system", "agentsight.service"),
            ("usr/local/share/agentsight/adapters/dsh/lib", "index.js"),
        ):
            with self.subTest(directory=directory):
                stage = self.base / ("linked-" + payload)
                result = self.make(
                    "install",
                    f"DESTDIR={stage}",
                    f"ENFORCER_BIN={self.enforcer}",
                    default_layout=True,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                selected = stage / directory
                outside = self.base / ("outside-" + payload)
                selected.rename(outside)
                selected.symlink_to(outside, target_is_directory=True)
                result = self.make("uninstall", f"DESTDIR={stage}")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("symlink", result.stderr)
                self.assertTrue((stage / "usr/local/bin/agentsight").is_file())
                self.assertTrue((outside / payload).is_file())

    def test_legacy_user_uninstall_keeps_its_original_default_paths(self):
        stage = self.base / "legacy-user-defaults"
        paths = [
            f"opt/legacy/bin/{name}"
            for name in ("agentsight", "agentsight-enforcer", "agentsight-start")
        ]
        paths += [
            f"opt/legacy/lib/systemd/system/{name}"
            for name in ("agentsight.service", "agentsight-enforcer.service")
        ]
        paths += ["etc/agentsight/config.json"]
        for name in paths:
            target = stage / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text("legacy user fixture\n")
        other = stage / "opt/legacy/etc/agentsight/config.json"
        other.parent.mkdir(parents=True)
        other.write_text("different layout\n")
        result = self.make(
            "uninstall", f"DESTDIR={stage}", "INSTALL_PROFILE=user", "PREFIX=/opt/legacy"
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(any((stage / name).exists() for name in paths))
        self.assertEqual(other.read_text(), "different layout\n")

    def test_explicit_legacy_install_keeps_its_layout_and_supervisor(self):
        stage = self.base / "legacy"
        environment = os.environ.copy()
        for name in (
            "MAKEFLAGS",
            "MFLAGS",
            "MAKEOVERRIDES",
            "PREFIX",
            "BINDIR",
            "START_BINARY",
        ):
            environment.pop(name, None)
        result = subprocess.run(
            [
                "make",
                "--no-print-directory",
                "-C",
                str(self.source),
                "-o",
                "build-binary",
                "-o",
                "build-frontend",
                "install-legacy",
                f"DESTDIR={stage}",
                f"ENFORCER_BIN={self.enforcer}",
                "SETCAP=0",
            ],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        launcher = (stage / "usr/local/bin/agentsight-start").read_text()
        self.assertIn("    agentsight trace &", launcher)
        self.assertNotIn("/usr/bin/agentsight", launcher)
        self.assertIn(
            "ExecStart=/usr/local/bin/agentsight-start",
            (stage / "usr/lib/systemd/system/agentsight.service").read_text(),
        )

    def test_default_service_assets_match_explicit_templates(self):
        generated = self.base / "default-services"
        result = subprocess.run(
            [
                "python3",
                str(self.source / "packaging/systemd/render.py"),
                "--bindir",
                "/usr/bin",
                "--binary",
                "/usr/bin/agentsight",
                "--launcher",
                str(generated / "agentsight-start.sh"),
                "--unitdir",
                str(generated),
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        launcher = (generated / "agentsight-start.sh").read_text()
        self.assertIn("/usr/bin/agentsight trace &", launcher)
        self.assertIn("/usr/bin/agentsight serve --host 0.0.0.0 &", launcher)
        self.assertEqual((generated / "agentsight-start.sh").stat().st_mode & 0o777, 0o755)
        for name, binary in (
            ("agentsight.service", "agentsight-start"),
            ("agentsight-enforcer.service", "agentsight-enforcer"),
        ):
            with self.subTest(asset=name):
                unit = generated / name
                self.assertIn(f"ExecStart=/usr/bin/{binary}", unit.read_text())
                self.assertEqual(unit.stat().st_mode & 0o777, 0o644)

    def test_source_launcher_workers_read_the_installed_configuration(self):
        prefix = self.base / "source-install"
        calls = self.base / "worker-calls"
        calls.mkdir()
        self.binary.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, pathlib, sys\n"
            "arguments = sys.argv[1:]\n"
            "config = pathlib.Path(arguments[arguments.index('--config') + 1])\n"
            "json.loads(config.read_text())\n"
            "calls = pathlib.Path(os.environ['WORKER_CALLS'])\n"
            "(calls / arguments[0]).write_text(json.dumps(arguments))\n"
        )
        result = self.make("install-core", f"PREFIX={prefix}", f"ENFORCER_BIN={self.enforcer}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        launcher = (prefix / "bin/agentsight-start").read_text()
        commands = [
            shlex.split(line.strip())[:-1]
            for line in launcher.splitlines()
            if line.strip().startswith(str(prefix / "bin/agentsight") + " ")
        ]
        self.assertEqual(len(commands), 2)
        # Run the rendered worker commands without requiring the supervisor's Bash 4.3 wait -n.
        for command in commands:
            result = subprocess.run(
                command,
                env={**os.environ, "WORKER_CALLS": str(calls)},
                capture_output=True,
                text=True,
                check=False,
                timeout=5,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        config = str(prefix / "etc/agentsight/config.json")
        self.assertEqual(json.loads((calls / "trace").read_text()), ["trace", "--config", config])
        self.assertEqual(
            json.loads((calls / "serve").read_text()),
            ["serve", "--host", "0.0.0.0", "--config", config],
        )

    def test_relocated_rpm_stager_keeps_the_compact_payload_contract(self):
        destination = self.base / "rpm-payload"
        environment = os.environ.copy()
        environment.pop("AGENTSIGHT_PROJECT_ROOT", None)
        environment["ENFORCER_BIN"] = str(self.enforcer)
        result = subprocess.run(
            [
                "bash",
                str(self.source / "packaging/rpm/stage-rpm-payload.sh"),
                str(destination),
            ],
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            {path.name for path in destination.iterdir()},
            {
                "agentsight",
                "agentsight-enforcer",
                "packaging",
                "agentsight.json",
                "README.md",
                "README_zh.md",
                "LICENSE",
                "dsh-plugin",
            },
        )
        self.assertEqual(
            (destination / "agentsight-enforcer").read_bytes(),
            self.enforcer.read_bytes(),
        )
        self.assertTrue((destination / "dsh-plugin/lib/index.js").is_file())
        self.assertTrue((destination / "dsh-plugin/package.json").is_file())
        self.assertTrue((destination / "dsh-plugin/cordis.patch.yml").is_file())
        self.assertFalse((destination / "dsh-plugin/node_modules").exists())
        for name in (
            "render.py",
            "agentsight-start.sh.in",
            "agentsight.service.in",
            "agentsight-enforcer.service.in",
        ):
            self.assertEqual(
                (destination / "packaging/systemd" / name).read_bytes(),
                (self.source / "packaging/systemd" / name).read_bytes(),
            )

    def test_enforcer_handoff_is_explicit_and_tampering_is_rejected(self):
        stage = self.base / "rejected"
        result = self.make("install", f"DESTDIR={stage}")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing prebuilt input", result.stderr)
        self.enforcer.write_text("tampered\n")
        result = self.make("install", f"DESTDIR={stage}", f"ENFORCER_BIN={self.enforcer}")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("receipt mismatch", result.stderr)
        self.assertFalse((stage / "usr/bin/agentsight-enforcer").exists())

    def test_stage_rejects_missing_destdir_and_unsupported_raw_namespace(self):
        for destination in ("/", "//", "///"):
            self.assertNotEqual(self.make("install", f"DESTDIR={destination}").returncode, 0)
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

    def test_stage_does_not_follow_destination_or_resource_links(self):
        outside = self.base / "outside"
        outside.mkdir()
        linked = self.base / "linked"
        linked.symlink_to(outside, target_is_directory=True)
        for destination in (linked, linked / "nested"):
            result = self.make(
                "install",
                f"DESTDIR={destination}",
                f"ENFORCER_BIN={self.enforcer}",
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("symlink", result.stderr)
        stage = self.base / "stage-links"
        stage.mkdir()
        (stage / "usr").symlink_to(outside, target_is_directory=True)
        result = self.make("install", f"DESTDIR={stage}", f"ENFORCER_BIN={self.enforcer}")
        self.assertNotEqual(result.returncode, 0)
        (self.source / "dsh-plugin/lib/escape").symlink_to(outside, target_is_directory=True)
        result = self.make("install", f"DESTDIR={self.base}/plugins")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(outside.iterdir()))

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
            f"ENFORCER_BIN={self.enforcer}",
            env_overrides={"PATH": str(tools) + os.pathsep + os.environ["PATH"]},
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((prefix / "bin/agentsight").is_file())
        self.assertTrue((prefix / "etc/agentsight/config.json").is_file())
        self.assertTrue((prefix / "lib/systemd/system/agentsight.service").is_file())
        self.assertTrue((prefix / "share/suite/agentsight/LICENSE").is_file())
        self.assertNotIn("overriding recipe", result.stderr)
        self.assertNotIn("cargo build", result.stdout)

    def test_install_preflights_missing_inputs_before_any_file_write(self):
        missing = self.source / "dsh-plugin/lib/index.js"
        missing.unlink()
        stage = self.base / "missing"
        result = self.make("install", f"DESTDIR={stage}", f"ENFORCER_BIN={self.enforcer}")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing prebuilt input", result.stderr)
        self.assertFalse(stage.exists())

    def test_shared_destdir_ignores_other_components_symlinks(self):
        stage = self.base / "shared"
        stage.mkdir()
        (stage / "another-component").symlink_to(self.base / "unrelated")
        result = self.make("install", f"DESTDIR={stage}", f"ENFORCER_BIN={self.enforcer}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((stage / "another-component").is_symlink())

    def test_core_install_does_not_require_or_copy_plugin_inputs(self):
        shutil.rmtree(self.source / "dsh-plugin")
        stage = self.base / "core-only"
        result = self.make(
            "install-core", f"DESTDIR={stage}", f"ENFORCER_BIN={self.enforcer}", "AWPRJ=suite"
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for name in ("agentsight", "agentsight-enforcer", "agentsight-start"):
            self.assertTrue((stage / "usr/bin" / name).is_file())
        self.assertTrue((stage / "etc/agentsight/config.json").is_file())
        self.assertIn(
            "ExecStart=/usr/bin/agentsight-start",
            (stage / "usr/lib/systemd/system/agentsight.service").read_text(),
        )
        self.assertTrue((stage / "usr/share/suite/agentsight/LICENSE").is_file())
        self.assertFalse((stage / "usr/share/suite/agentsight/adapters").exists())
        self.assertNotIn("overriding recipe", result.stderr)

    def test_plugins_install_requires_no_core_inputs_or_enforcer_receipt(self):
        self.binary.unlink()
        self.enforcer.unlink()
        self.receipt.unlink()
        (self.source / "LICENSE").unlink()
        (self.source / "agentsight.json").unlink()
        shutil.rmtree(self.source / "packaging/systemd")
        prefix = self.base / "plugins-prefix"
        result = self.make("install-plugins", f"PREFIX={prefix}", "AWPRJ=suite")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        plugin = prefix / "share/suite/agentsight/adapters/dsh"
        self.assertTrue((plugin / "lib/index.js").is_file())
        self.assertTrue((plugin / "package.json").is_file())
        self.assertTrue((plugin / "cordis.patch.yml").is_file())
        self.assertFalse((prefix / "bin").exists())
        self.assertFalse((prefix / "etc").exists())
        self.assertFalse((prefix / "share/suite/agentsight/LICENSE").exists())

    def test_each_group_preflights_inputs_before_any_write(self):
        for group, missing in (
            ("core", self.receipt),
            ("plugins", self.source / "dsh-plugin/lib/index.js"),
        ):
            saved = self.base / "saved-input"
            missing.replace(saved)
            stage = self.base / (group + "-missing")
            try:
                result = self.make(
                    "install-" + group, f"DESTDIR={stage}", f"ENFORCER_BIN={self.enforcer}"
                )
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("missing", result.stderr)
                self.assertFalse(stage.exists())
            finally:
                saved.replace(missing)

    def test_no_empty_skills_install_group_is_advertised(self):
        result = self.make("install-skills")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("No rule to make target", result.stderr)

    def test_build_and_grouped_installs_exclude_retired_stage_entrypoints(self):
        result = self.make("-n", "build")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cargo build --release", result.stdout)
        self.assertIn("cd dsh-plugin && npm install && npm run build", result.stdout)
        self.assertIn("target/delivery/agentsight-enforcer", result.stdout)
        for target in (
            "stage-package",
            "stage-package-core",
            "stage-package-plugin",
            "stage-package-skills",
        ):
            result = self.make(target)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("No rule to make target", result.stderr)

    def test_build_enforcer_records_the_default_install_input(self):
        result = self.make("build-enforcer", f"ENFORCER_BUILD=printf '%s\\n' '{self.enforcer}'")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(result.stdout.strip(), str(self.enforcer))
        handoff = self.source / "target/delivery/agentsight-enforcer"
        self.assertEqual(handoff.read_bytes(), self.enforcer.read_bytes())
        self.assertEqual(Path(str(handoff) + ".sha256").read_text(), self.receipt.read_text())
        result = self.make("install", f"PREFIX={self.base / 'handoff-install'}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_direct_install_checks_written_paths_without_scanning_shared_parents(self):
        prefix = self.base / "direct-links"
        bindir = prefix / "bin"
        bindir.mkdir(parents=True)
        outside = self.base / "outside-file"
        outside.write_text("leave unchanged\n")
        (bindir / "unrelated-link").symlink_to(outside)
        result = self.make("install", f"PREFIX={prefix}", f"ENFORCER_BIN={self.enforcer}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        (bindir / "agentsight").unlink()
        (bindir / "agentsight").symlink_to(outside)
        result = self.make("install", f"PREFIX={prefix}", f"ENFORCER_BIN={self.enforcer}")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symlink", result.stderr)
        self.assertEqual(outside.read_text(), "leave unchanged\n")


if __name__ == "__main__":
    unittest.main()
