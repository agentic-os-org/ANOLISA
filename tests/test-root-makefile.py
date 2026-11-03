#!/usr/bin/env python3
"""Verify root orchestration using isolated component Makefiles and no toolchains."""

from __future__ import annotations

import json
import os
import shlex
import shutil
import subprocess
import sys
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
BUILD_TARGETS = {component: ("build",) for component in COMPONENTS}


class RootMakefile(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="root-makefile-test-")
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name).resolve()
        shutil.copyfile(ROOT / "Makefile", self.work / "Makefile")
        self.log = self.work / "calls.jsonl"
        self.failure = self.work / "failure.json"
        recorder = self.work / "record.py"
        recorder.write_text(
            "import json, os, sys\n"
            "from pathlib import Path\n"
            f"log = Path({str(self.log)!r})\n"
            f"failure = Path({str(self.failure)!r})\n"
            "event = {'component': sys.argv[1], 'target': sys.argv[2]}\n"
            "event.update({key: os.environ.get(key, '') for key in "
            "('AWPRJ', 'PREFIX', 'DESTDIR', 'BINDIR', 'INSTALL_PROFILE', 'PROFILE', "
            "'MAKEFLAGS', 'MAKEOVERRIDES')})\n"
            "with log.open('a') as output:\n"
            "    output.write(json.dumps(event) + '\\n')\n"
            "if failure.exists():\n"
            "    rule = json.loads(failure.read_text())\n"
            "    if event['component'] == rule['component'] "
            "and event['target'] == rule['target']:\n"
            "        raise SystemExit(23)\n"
        )
        self.recorder = recorder
        for component in COMPONENTS:
            self.write_component(self.work / "src" / component, component)
            implementation = self.work / "src" / component / "packaging/private-fixture.mk"
            implementation.parent.mkdir()
            implementation.write_text(
                "$(error root must not include component implementation modules)\n"
            )
        # An invalid ../escape selection can execute only this fixture, never a
        # Makefile elsewhere on the host, if the root validation regresses.
        self.write_component(self.work / "escape", "escaped")
        self.environment = os.environ.copy()
        for name in (
            "AWPRJ",
            "PREFIX",
            "COMPONENTS",
            "DESTDIR",
            "MAKEFLAGS",
            "MFLAGS",
            "MAKEOVERRIDES",
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

    def write_component(self, directory: Path, component: str) -> None:
        directory.mkdir(parents=True, exist_ok=True)
        targets = "layout test-packaging build install"
        directory.joinpath("Makefile").write_text(
            "AWPRJ ?= child-default\n"
            "PREFIX ?= $(if $(filter user,$(INSTALL_PROFILE)),$(HOME)/.local,/child/default)\n"
            "BINDIR ?= /child/bin\n"
            "INSTALL_PROFILE ?= child-profile\n"
            "PROFILE ?= child-layout\n"
            "export AWPRJ PREFIX DESTDIR BINDIR INSTALL_PROFILE PROFILE MAKEFLAGS MAKEOVERRIDES\n"
            f".PHONY: {targets}\n"
            f"{targets}:\n"
            f"\t@{shlex.quote(sys.executable)} {shlex.quote(str(self.recorder))} "
            f"{shlex.quote(component)} '$@'\n"
        )

    def invoke(
        self, *arguments: str, environment: dict[str, str] | None = None
    ) -> subprocess.CompletedProcess[str]:
        self.log.unlink(missing_ok=True)
        result = subprocess.run(
            ["make", "--no-print-directory", "-s", *arguments],
            cwd=self.work,
            env={**self.environment, **(environment or {})},
            text=True,
            capture_output=True,
            check=False,
            timeout=20,
        )
        self.assertNotIn("overriding recipe", result.stderr)
        self.assertNotIn("ignoring old recipe", result.stderr)
        return result

    def events(self) -> list[dict[str, str]]:
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def assert_success(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def assert_rejected(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.events(), [], "invalid requests must fail before dispatch")

    def listed_components(self) -> list[str]:
        result = self.invoke("list-components")
        self.assert_success(result)
        self.assertEqual(self.events(), [])
        names = result.stdout.split()
        self.assertEqual(len(names), len(COMPONENTS))
        self.assertEqual(set(names), set(COMPONENTS))
        return names

    def test_default_help_and_listing_do_not_enter_components(self) -> None:
        default = self.invoke()
        self.assert_success(default)
        self.assertTrue(default.stdout.strip())
        self.assertEqual(self.events(), [])
        explicit = self.invoke("help")
        self.assert_success(explicit)
        self.assertEqual(default.stdout, explicit.stdout)
        self.assertEqual(self.events(), [])
        self.listed_components()

    def test_layout_forwards_namespace_and_preserves_component_defaults(self) -> None:
        names = self.listed_components()
        self.assert_success(self.invoke("layout"))
        events = self.events()
        self.assertEqual([event["component"] for event in events], names)
        for event in events:
            self.assertEqual(event["target"], "layout")
            self.assertEqual(event["AWPRJ"], "anolisa")
            self.assertEqual(event["PREFIX"], "/child/default")
            self.assertEqual(event["INSTALL_PROFILE"], "child-profile")

    def test_user_install_forwards_profile_and_user_prefix_to_selected_components(self) -> None:
        home = self.work / "home"
        self.assert_success(
            self.invoke(
                "install",
                "COMPONENTS=cosh-ng tokenless agent-memory",
                "INSTALL_PROFILE=user",
                environment={"HOME": str(home)},
            )
        )
        events = self.events()
        self.assertEqual(len(events), 3)
        for event in events:
            self.assertEqual(event["target"], "install")
            self.assertEqual(event["INSTALL_PROFILE"], "user")
            self.assertEqual(event["PREFIX"], str(home / ".local"))

    def test_selection_preserves_requested_order_and_skips_other_components(self) -> None:
        self.assert_success(self.invoke("layout", "COMPONENTS=agent-memory cosh-ng tokenless"))
        self.assertEqual(
            [(event["component"], event["target"]) for event in self.events()],
            [(component, "layout") for component in ("agent-memory", "cosh-ng", "tokenless")],
        )

    def test_environment_selection_and_namespace_prefix_overrides(self) -> None:
        self.assert_success(
            self.invoke(
                "layout",
                environment={
                    "COMPONENTS": "ws-ckpt skillfs",
                    "AWPRJ": "environment-suite",
                    "PREFIX": "/opt/environment",
                },
            )
        )
        self.assertEqual([event["component"] for event in self.events()], ["ws-ckpt", "skillfs"])
        for event in self.events():
            self.assertEqual(event["AWPRJ"], "environment-suite")
            self.assertEqual(event["PREFIX"], "/opt/environment")
        self.assert_success(
            self.invoke(
                "layout",
                environment={
                    "COMPONENTS": "tokenless",
                    "AWPRJ": "",
                    "PREFIX": "/opt/environment-empty",
                },
            )
        )
        self.assertEqual(self.events()[0]["AWPRJ"], "")
        self.assertEqual(self.events()[0]["PREFIX"], "/opt/environment-empty")
        self.assert_success(
            self.invoke(
                "layout",
                "COMPONENTS=tokenless",
                "AWPRJ=",
                "PREFIX=/opt/command line",
                environment={"AWPRJ": "environment-suite", "PREFIX": "/opt/environment"},
            )
        )
        self.assertEqual(self.events()[0]["AWPRJ"], "")
        self.assertEqual(self.events()[0]["PREFIX"], "/opt/command line")

    def test_test_packaging_defaults_to_all_with_isolated_child_defaults(self) -> None:
        names = self.listed_components()
        self.assert_success(self.invoke("test-packaging"))
        events = self.events()
        self.assertEqual([event["component"] for event in events], names)
        for event in events:
            self.assertEqual(event["target"], "test-packaging")
            self.assertEqual(event["AWPRJ"], "child-default")
            self.assertEqual(event["PREFIX"], "/child/default")

    def test_test_packaging_clears_explicit_composition_values_and_make_overrides(self) -> None:
        self.assert_success(
            self.invoke(
                "test-packaging",
                "COMPONENTS=tokenless cosh-ng",
                "AWPRJ=root-suite",
                "PREFIX=/opt/root-suite",
                "BINDIR=/opt/other",
                "INSTALL_PROFILE=user",
                "PROFILE=legacy-anolisa",
                environment={"AWPRJ": "environment-suite", "PREFIX": "/opt/environment"},
            )
        )
        self.assertEqual([event["component"] for event in self.events()], ["tokenless", "cosh-ng"])
        for event in self.events():
            self.assertEqual(event["AWPRJ"], "child-default")
            self.assertEqual(event["PREFIX"], "/child/default")
            self.assertEqual(event["BINDIR"], "/child/bin")
            self.assertEqual(event["INSTALL_PROFILE"], "child-profile")
            self.assertEqual(event["PROFILE"], "child-layout")
            self.assertNotIn("AWPRJ", event["MAKEFLAGS"])
            self.assertNotIn("PREFIX", event["MAKEFLAGS"])
            self.assertNotIn("COMPONENTS", event["MAKEFLAGS"])
            self.assertNotIn("BINDIR", event["MAKEFLAGS"])
            self.assertEqual(event["MAKEOVERRIDES"], "")

    def test_test_packaging_preserves_recursive_dry_run_without_running_fixtures(self) -> None:
        self.assert_success(
            self.invoke(
                "-n",
                "test-packaging",
                "COMPONENTS=tokenless cosh-ng",
                "AWPRJ=root-suite",
                "PREFIX=/opt/root-suite",
            )
        )
        self.assertEqual(self.events(), [], "recursive dry-run must not execute child fixtures")

    def test_build_requires_explicit_nonempty_selection(self) -> None:
        for arguments in ((), ("COMPONENTS=",), ("COMPONENTS=   ",)):
            with self.subTest(arguments=arguments):
                self.assert_rejected(self.invoke("build", *arguments))

    def test_build_dispatches_uniform_entrypoints_and_forwards_empty_namespace(self) -> None:
        self.assert_success(
            self.invoke(
                "build",
                "COMPONENTS=" + " ".join(COMPONENTS),
                "AWPRJ=",
                "PREFIX=/opt/build",
            )
        )
        expected = [
            (component, target) for component in COMPONENTS for target in BUILD_TARGETS[component]
        ]
        self.assertEqual(
            [(event["component"], event["target"]) for event in self.events()],
            expected,
        )
        for event in self.events():
            self.assertEqual(event["AWPRJ"], "")
            self.assertEqual(event["PREFIX"], "/opt/build")

    def test_install_requires_selection_and_rejects_invalid_package_roots(self) -> None:
        self.assert_rejected(self.invoke("install", f"DESTDIR={self.work / 'stage'}"))
        self.assert_rejected(
            self.invoke(
                "install",
                "COMPONENTS=",
                f"DESTDIR={self.work / 'stage'}",
            )
        )
        for destination in ("/", "//", "///", "relative", "/tmp/../escape"):
            arguments = [] if destination is None else [f"DESTDIR={destination}"]
            with self.subTest(destination=destination):
                self.assert_rejected(
                    self.invoke(
                        "install",
                        "COMPONENTS=tokenless",
                        *arguments,
                    )
                )

    def test_install_shares_one_package_root_and_forwards_layout(self) -> None:
        base = self.work / "stage outputs"
        self.assert_success(
            self.invoke(
                "install",
                "COMPONENTS=tokenless agent-memory",
                f"DESTDIR={base}",
                "AWPRJ=custom-suite",
                "PREFIX=/opt/staged suite",
            )
        )
        events = self.events()
        self.assertEqual([event["component"] for event in events], ["tokenless", "agent-memory"])
        for event in events:
            self.assertEqual(event["target"], "install")
            self.assertEqual(event["DESTDIR"], str(base))
            self.assertEqual(event["AWPRJ"], "custom-suite")
            self.assertEqual(event["PREFIX"], "/opt/staged suite")

    def test_direct_install_forwards_empty_destination(self) -> None:
        for arguments in ((), ("DESTDIR=",)):
            with self.subTest(arguments=arguments):
                self.assert_success(
                    self.invoke(
                        "install",
                        "COMPONENTS=tokenless agent-memory",
                        "PREFIX=/opt/direct",
                        *arguments,
                    )
                )
                for event in self.events():
                    self.assertEqual(event["target"], "install")
                    self.assertEqual(event["DESTDIR"], "")
                    self.assertEqual(event["PREFIX"], "/opt/direct")

    def test_removed_staging_interfaces_do_not_dispatch(self) -> None:
        for target in (
            "stage-package",
            "stage-package-core",
            "stage-package-plugin",
            "stage-package-skills",
        ):
            with self.subTest(target=target):
                self.assert_rejected(self.invoke(target, "COMPONENTS=tokenless"))

    def test_unknown_and_traversal_selections_fail_before_any_dispatch(self) -> None:
        for component in ("unknown", "../escape", "tokenless/../cosh-ng", "tokenless/", ".."):
            for target in ("layout", "test-packaging", "build", "install"):
                with self.subTest(component=component, target=target):
                    self.assert_rejected(
                        self.invoke(
                            target,
                            f"COMPONENTS=tokenless {component}",
                            f"DESTDIR={self.work / 'stage'}",
                        )
                    )

    def test_component_failure_stops_sequential_dispatch(self) -> None:
        for target, failure_target in (
            ("layout", "layout"),
            ("test-packaging", "test-packaging"),
            ("build", "build"),
            ("install", "install"),
        ):
            self.failure.write_text(
                json.dumps(
                    {
                        "component": "agentsight",
                        "target": failure_target,
                    }
                )
            )
            with self.subTest(target=target):
                result = self.invoke(
                    target,
                    "COMPONENTS=tokenless agentsight agent-memory",
                    f"DESTDIR={self.work / 'stage'}",
                )
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(
                    [event["component"] for event in self.events()],
                    ["tokenless", "agentsight"],
                )
                self.assertEqual(self.events()[-1]["target"], failure_target)


if __name__ == "__main__":
    unittest.main()
