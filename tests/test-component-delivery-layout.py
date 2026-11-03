#!/usr/bin/env python3
"""Check the common delivery interface without invoking component toolchains."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
COMPONENTS = (
    "cosh-ng",
    "tokenless",
    "agent-sec-core",
    "agentsight",
    "skillfs",
    "ws-ckpt",
    "agent-memory",
)


class ComponentDeliveryInterface(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="component-delivery-interface-")
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name).resolve()
        self.bin = self.work / "bin"
        self.bin.mkdir()
        self.calls = self.work / "toolchain-calls"
        for command in (
            "node",
            "npm",
            "cargo",
            "rustup",
            "systemctl",
            "sudo",
            "setcap",
            "openclaw",
        ):
            script = self.bin / command
            script.write_text(
                f'#!/bin/sh\nprintf "%s\\n" {command} >> "$DELIVERY_CALL_LOG"\nexit 91\n'
            )
            script.chmod(0o755)
        self.environment = os.environ.copy()
        for name in (
            "AWPRJ",
            "PREFIX",
            "BINDIR",
            "LIBDIR",
            "LIBEXECDIR",
            "DATADIR",
            "ADAPTERDIR",
            "SKILLDIR",
            "UNITDIR",
            "INSTALL_PROFILE",
            "PROFILE",
        ):
            self.environment.pop(name, None)
        self.environment["PATH"] = str(self.bin) + os.pathsep + self.environment["PATH"]
        self.environment["DELIVERY_CALL_LOG"] = str(self.calls)

    def layout(self, component: str, *arguments: str) -> dict[str, str]:
        result = subprocess.run(
            ["make", "-s", "-C", str(ROOT / "src" / component), "layout", *arguments],
            env=self.environment,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("overriding recipe", result.stderr)
        self.assertNotIn("ignoring old recipe", result.stderr)
        self.assertFalse(self.calls.exists(), self.calls.read_text() if self.calls.exists() else "")
        return dict(line.split("=", 1) for line in result.stdout.splitlines() if "=" in line)

    def test_component_root_owns_the_make_entry(self) -> None:
        for component in COMPONENTS:
            with self.subTest(component=component):
                source = ROOT / "src" / component
                self.assertTrue((source / "Makefile").is_file())
                self.assertFalse((source / "packaging/Makefile").exists())
                self.layout(component, "INSTALL_PROFILE=system")

    def test_standalone_defaults_do_not_inherit_legacy_file_values(self) -> None:
        for component in COMPONENTS:
            with self.subTest(component=component):
                layout = self.layout(component, "INSTALL_PROFILE=system")
                self.assertEqual(layout["AWPRJ"], "")
                self.assertEqual(layout["PREFIX"], "/usr/local")
                self.assertEqual(layout["BINDIR"], "/usr/local/bin")
                self.assertEqual(layout["DATADIR"], f"/usr/local/share/{component}")

    def test_explicit_namespace_and_prefix_apply_across_components(self) -> None:
        for component in COMPONENTS:
            with self.subTest(component=component):
                layout = self.layout(
                    component,
                    "INSTALL_PROFILE=system",
                    "AWPRJ=example",
                    "PREFIX=/opt/component-suite",
                )
                self.assertEqual(layout["AWPRJ"], "example")
                self.assertEqual(layout["BINDIR"], "/opt/component-suite/bin")
                self.assertEqual(
                    layout["DATADIR"], f"/opt/component-suite/share/example/{component}"
                )

    def test_explicit_empty_namespace_overrides_environment(self) -> None:
        self.environment["AWPRJ"] = "anolisa"
        for component in COMPONENTS:
            with self.subTest(component=component):
                layout = self.layout(component, "INSTALL_PROFILE=system", "AWPRJ=")
                self.assertEqual(layout["AWPRJ"], "")
                self.assertEqual(layout["DATADIR"], f"/usr/local/share/{component}")

    def test_root_installs_real_components_into_one_tree_without_building(self) -> None:
        binaries = self.work / "prebuilt"
        binaries.mkdir()
        for name in ("cosh-cli", "cosh-core", "cosh-gateway", "cosh-shell", "skillfs"):
            binary = binaries / name
            binary.write_text("#!/bin/sh\nexit 0\n")
            binary.chmod(0o755)
        for staged in (False, True):
            with self.subTest(staged=staged):
                destination = self.work / "pkgroot" if staged else None
                prefix = "/opt/component-suite" if staged else str(self.work / "direct-prefix")
                output = Path(str(destination) + prefix) if destination else Path(prefix)
                output.mkdir(parents=True)
                unrelated = output / "another-component-link"
                unrelated.symlink_to(self.work / "unrelated")
                arguments = [
                    "make",
                    "--no-print-directory",
                    "-s",
                    "-C",
                    str(ROOT),
                    "install",
                    "COMPONENTS=cosh-ng skillfs",
                    f"PREFIX={prefix}",
                    "INSTALL_PROFILE=system",
                    "AWPRJ=example",
                    f"BIN_DIR={binaries}",
                    f"BINARY={binaries / 'skillfs'}",
                ]
                if destination:
                    arguments.append(f"DESTDIR={destination}")
                else:
                    arguments.append("DESTDIR=")
                result = subprocess.run(
                    arguments,
                    env=self.environment,
                    text=True,
                    capture_output=True,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertTrue((output / "bin/cosh-cli").is_file())
                self.assertTrue((output / "bin/skillfs").is_file())
                self.assertTrue((output / "libexec/example/cosh-ng/cosh-core").is_file())
                self.assertTrue(unrelated.is_symlink())
                self.assertNotIn(str(destination) + "/cosh-ng/", result.stdout)
                self.assertFalse(
                    self.calls.exists(), self.calls.read_text() if self.calls.exists() else ""
                )

    def test_only_applicable_install_groups_are_exposed(self) -> None:
        groups = {
            "cosh-ng": set(),
            "tokenless": {"core", "plugins"},
            "agent-sec-core": {"core", "plugins", "skills"},
            "agentsight": {"core", "plugins"},
            "skillfs": {"core", "skills"},
            "ws-ckpt": {"core", "plugins", "skills"},
            "agent-memory": {"core", "plugins"},
        }
        for component, applicable in groups.items():
            for group in ("core", "plugins", "skills"):
                with self.subTest(component=component, group=group):
                    result = subprocess.run(
                        [
                            "make",
                            "--no-print-directory",
                            "-n",
                            "-C",
                            str(ROOT / "src" / component),
                            "install-" + group,
                        ],
                        env=self.environment,
                        text=True,
                        capture_output=True,
                        check=False,
                    )
                    if group in applicable:
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    else:
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn("No rule to make target", result.stderr)
                    self.assertNotIn("overriding recipe", result.stderr)
        self.assertFalse(self.calls.exists(), self.calls.read_text() if self.calls.exists() else "")

    def test_removed_package_staging_targets_are_unavailable(self) -> None:
        for component in COMPONENTS:
            for target in (
                "stage-package",
                "stage-package-core",
                "stage-package-plugin",
                "stage-package-skills",
            ):
                with self.subTest(component=component, target=target):
                    result = subprocess.run(
                        [
                            "make",
                            "--no-print-directory",
                            "-n",
                            "-C",
                            str(ROOT / "src" / component),
                            target,
                        ],
                        env=self.environment,
                        text=True,
                        capture_output=True,
                        check=False,
                    )
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("No rule to make target", result.stderr)
                    self.assertNotIn("overriding recipe", result.stderr)
        self.assertFalse(self.calls.exists(), self.calls.read_text() if self.calls.exists() else "")


if __name__ == "__main__":
    unittest.main()
