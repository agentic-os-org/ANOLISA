"""Exercise the actual RPM scriptlets around an old CLI final erase.

The mocked old systemd_preun models stop/disable at $1=0. Target-distribution
macro expansion still needs testing with the previously published binary RPM.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_rpm_split import HOOKS, SPEC, sections  # noqa: E402

SYSTEMCTL = r"""import json
import os
import signal
import sys
from pathlib import Path
path = Path(sys.argv[1])
args = sys.argv[2:]
state = json.loads(path.read_text())
state['calls'].append(args)
command = args[0]
if state.get('interrupt') == command:
    path.write_text(json.dumps(state))
    os.kill(os.getppid(), signal.SIGTERM)
    sys.exit(1)
failed = command in state.get('fail', [])
result = 0
output = ''
if failed:
    result = 1
elif command == 'is-enabled':
    output = state['enabled']
    result = 1 if output in ('disabled', 'masked', 'masked-runtime') else 0
elif command == 'is-active':
    output = state['active']
    result = 0 if output in ('active', 'reloading') else 3
elif command == 'disable':
    if not state['enabled'].startswith('masked'):
        state['enabled'] = 'disabled'
    if '--now' in args:
        state['active'] = 'inactive'
elif command in ('enable', 'preset'):
    state['enabled'] = 'enabled-runtime' if '--runtime' in args else 'enabled'
    if '--now' in args:
        state['active'] = 'active'
elif command == 'unmask':
    state['enabled'] = 'disabled'
elif command == 'mask':
    state['enabled'] = 'masked-runtime' if '--runtime' in args else 'masked'
elif command == 'start':
    if state['enabled'].startswith('masked'):
        result = 1
    else:
        state['active'] = 'active'
elif command == 'stop':
    state['active'] = 'inactive'
path.write_text(json.dumps(state))
if output:
    print(output)
sys.exit(result)
"""


class MigrationFixture:
    def __init__(self, base):
        self.base = base
        self.bin = base / "bin"
        self.bin.mkdir()
        self.manager = base / "manager"
        self.manager.mkdir()
        self.marker = base / "migration"
        self.legacy_unit = base / "legacy-user-unit"
        self.data = base / "systemctl.json"
        self.rpm_data = base / "rpm.json"
        self.systemctl = self.bin / "systemctl"
        self.systemctl.write_text(
            f"#!/bin/sh\nexec '{sys.executable}' '{self.bin / 'systemctl.py'}' '{self.data}' \"$@\"\n"
        )
        (self.bin / "systemctl.py").write_text(SYSTEMCTL)
        self.systemctl.chmod(0o755)
        self.rpm = self.bin / "rpm"
        self.rpm.write_text(
            f"#!{sys.executable}\nimport json, sys\nfrom pathlib import Path\n"
            f"state=json.loads(Path({str(self.rpm_data)!r}).read_text())\n"
            "if state.get('fail'): sys.exit(2)\n"
            "name='agent-sec-cli' if state['old'] else 'agent-sec-core'\n"
            "print(name)\nsys.exit(0)\n"
        )
        self.rpm.chmod(0o755)
        self.environment = {
            **os.environ,
            "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
        }
        self.parsed = sections(SPEC.read_text())
        self.reset()

    def reset(self, enabled="enabled", active="active", old=True, fail=()):
        self.data.write_text(
            json.dumps(
                {"enabled": enabled, "active": active, "calls": [], "fail": list(fail)}
            )
        )
        self.rpm_data.write_text(json.dumps({"old": old}))

    def state(self):
        return json.loads(self.data.read_text())

    def render(self, phase, for_rpm=False):
        body = self.parsed[phase, "agent-sec-core"]
        for old, new in (
            ("/run/agent-sec-core-rpm", str(self.marker)),
            ("/run/systemd/system", str(self.manager)),
            ("/usr/lib/systemd/user/agent-sec-core.service", str(self.legacy_unit)),
            ("/usr/bin/systemctl", str(self.systemctl)),
        ):
            body = body.replace(old, new)
        body = re.sub(
            r"%systemd_post agent-sec-core.service",
            f'if [ "$1" -eq 1 ]; then "{self.systemctl}" preset agent-sec-core.service; fi',
            body,
        )
        body = re.sub(
            r"%systemd_preun agent-sec-core.service",
            f'if [ "$1" -eq 0 ]; then "{self.systemctl}" disable --now agent-sec-core.service; fi',
            body,
        )
        body = re.sub(
            r"%systemd_postun_with_restart agent-sec-core.service",
            f'"{self.systemctl}" daemon-reload',
            body,
        )
        # RPM resets scriptlet PATH. Bind the fixture's private rpmdb query
        # explicitly so a real transaction never inspects the host database.
        body = body.replace("$(rpm ", f'$("{self.rpm}" ')
        if not for_rpm:
            body = body.replace("%%{NAME}", "%{NAME}")
        return body

    def run(self, phase, count=2):
        return subprocess.run(
            ["sh", "-c", self.render(phase), "rpm-scriptlet", str(count)],
            env=self.environment,
            text=True,
            capture_output=True,
            check=False,
        )

    def old_final_erase(self):
        subprocess.run(
            [str(self.systemctl), "disable", "--now", "agent-sec-core.service"],
            check=True,
            env=self.environment,
        )
        self.rpm_data.write_text(json.dumps({"old": False}))


class ServiceMigrationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="asc-rpm-service-")
        self.addCleanup(self.temporary.cleanup)
        self.fixture = MigrationFixture(Path(self.temporary.name).resolve())

    def phase(self, name, count=2):
        result = self.fixture.run(name, count)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_enabled_active_inactive_disabled_and_runtime_states_survive_final_erase(
        self,
    ):
        for enabled, active in (
            ("enabled", "active"),
            ("enabled", "inactive"),
            ("disabled", "active"),
            ("disabled", "inactive"),
            ("disabled", "failed"),
            ("enabled-runtime", "active"),
            ("masked", "inactive"),
            ("masked-runtime", "inactive"),
            ("masked", "active"),
            ("masked-runtime", "active"),
        ):
            with self.subTest(enabled=enabled, active=active):
                self.fixture.reset(enabled, active)
                self.phase("pre")
                self.assertEqual(self.fixture.marker.stat().st_mode & 0o777, 0o700)
                self.assertEqual(
                    (self.fixture.marker / "state").stat().st_mode & 0o777, 0o600
                )
                self.phase("post", 1)
                self.assertFalse(
                    any(
                        call[0] in ("preset", "enable", "start")
                        for call in self.fixture.state()["calls"]
                    )
                )
                self.fixture.old_final_erase()
                self.phase("posttrans")
                state = self.fixture.state()
                self.assertEqual(state["enabled"], enabled)
                self.assertEqual(
                    state["active"], "inactive" if active == "failed" else active
                )
                self.assertFalse(self.fixture.marker.exists())

    def test_fresh_install_ignores_virtual_provides_and_keeps_initial_activation(self):
        self.fixture.reset("disabled", "inactive", old=False)
        self.phase("pre", 1)
        self.assertFalse(self.fixture.marker.exists())
        self.phase("post", 1)
        self.phase("posttrans", 1)
        self.assertEqual(self.fixture.state()["active"], "active")
        self.assertEqual(self.fixture.state()["enabled"], "enabled")

    def test_second_upgrade_clears_stale_marker_without_replaying_state(self):
        self.fixture.reset("disabled", "inactive", old=False)
        self.fixture.marker.mkdir()
        (self.fixture.marker / "state").write_text("system\nenabled\nactive\n")
        self.phase("pre")
        self.phase("post")
        self.phase("posttrans")
        self.assertEqual(self.fixture.state()["active"], "inactive")
        self.assertFalse(
            any(
                call[0] in ("start", "enable", "preset")
                for call in self.fixture.state()["calls"]
            )
        )
        self.assertFalse(self.fixture.marker.exists())

    def test_offline_transfer_restores_enablement_after_old_erase_without_activity(
        self,
    ):
        self.fixture.manager.rmdir()
        for enabled in (
            "enabled",
            "disabled",
            "enabled-runtime",
            "masked",
            "masked-runtime",
        ):
            with self.subTest(enabled=enabled):
                self.fixture.reset(enabled, "inactive")
                self.phase("pre", 1)
                self.phase("post", 1)
                self.fixture.old_final_erase()
                self.fixture.manager.mkdir()
                self.phase("posttrans", 1)
                state = self.fixture.state()
                self.assertEqual(state["enabled"], enabled)
                self.assertEqual(state["active"], "inactive")
                self.assertFalse(
                    any(
                        call[0] in ("is-active", "start", "stop", "daemon-reload")
                        for call in state["calls"]
                    )
                )
                self.assertTrue(
                    all(
                        "--now" not in call
                        for call in state["calls"]
                        if call[0] == "enable"
                    )
                )
                self.assertFalse(self.fixture.marker.exists())
                self.fixture.manager.rmdir()

    def test_offline_unknown_enablement_or_user_unit_refuses_migration(self):
        self.fixture.manager.rmdir()
        self.fixture.reset("unknown", "inactive")
        result = self.fixture.run("pre", 1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to guess", result.stderr)
        self.assertFalse((self.fixture.marker / "state").exists())
        self.fixture.reset("disabled", "inactive")
        self.fixture.legacy_unit.touch()
        result = self.fixture.run("pre", 1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("offline AgentSec user-unit migration", result.stderr)
        self.assertFalse((self.fixture.marker / "state").exists())

    def test_fresh_offline_install_never_calls_systemctl(self):
        self.fixture.reset("disabled", "inactive", old=False)
        self.fixture.manager.rmdir()
        self.phase("pre", 1)
        self.phase("post", 1)
        self.phase("posttrans", 1)
        self.assertEqual(self.fixture.state()["calls"], [])

    def test_rpm_query_failure_is_not_mistaken_for_fresh_install(self):
        self.fixture.rpm_data.write_text(json.dumps({"old": True, "fail": True}))
        result = self.fixture.run("pre", 1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to guess", result.stderr)
        self.assertEqual(self.fixture.state()["calls"], [])

    def test_interrupted_masked_restart_remasks_from_the_exit_trap(self):
        self.fixture.reset("masked-runtime", "active")
        self.phase("pre")
        self.fixture.old_final_erase()
        state = self.fixture.state()
        state["interrupt"] = "start"
        self.fixture.data.write_text(json.dumps(state))
        result = self.fixture.run("posttrans")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("interrupted", result.stderr)
        self.assertEqual(self.fixture.state()["enabled"], "masked-runtime")
        self.assertFalse(self.fixture.marker.exists())

    def test_legacy_user_unit_has_its_own_migration_mode(self):
        self.fixture.reset("not-found", "unknown")
        self.fixture.legacy_unit.touch()
        self.phase("pre")
        self.assertIn("legacy-user", (self.fixture.marker / "state").read_text())
        self.phase("post")
        self.fixture.old_final_erase()
        self.phase("posttrans")
        self.assertEqual(self.fixture.state()["enabled"], "enabled")
        self.assertEqual(self.fixture.state()["active"], "active")
        self.assertFalse(
            any(call[0] == "stop" for call in self.fixture.state()["calls"])
        )

    def test_query_failure_unknown_state_and_lost_manager_never_start(self):
        for fail, enabled in (
            (["is-enabled"], "enabled"),
            (["is-active"], "enabled"),
            ([], "unknown"),
        ):
            with self.subTest(fail=fail, enabled=enabled):
                self.fixture.reset(enabled, "active", fail=fail)
                result = self.fixture.run("pre")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("refusing to guess", result.stderr)
                self.assertFalse((self.fixture.marker / "state").exists())
                self.assertFalse(
                    any(call[0] == "start" for call in self.fixture.state()["calls"])
                )
        self.fixture.reset()
        self.phase("pre")
        self.fixture.old_final_erase()
        self.fixture.manager.rmdir()
        result = self.fixture.run("posttrans")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unavailable", result.stderr)
        self.assertFalse(self.fixture.marker.exists())
        self.assertFalse(
            any(call[0] == "start" for call in self.fixture.state()["calls"])
        )

    def test_failed_restore_does_not_start_and_masked_failure_remasks_in_cleanup(self):
        for enabled, failure in (
            ("enabled", "enable"),
            ("masked", "start"),
            ("masked-runtime", "start"),
        ):
            with self.subTest(enabled=enabled, failure=failure):
                self.fixture.reset(enabled, "active")
                self.phase("pre")
                self.fixture.old_final_erase()
                state = self.fixture.state()
                state["fail"] = [failure]
                self.fixture.data.write_text(json.dumps(state))
                result = self.fixture.run("posttrans")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("restoration failed", result.stderr)
                self.assertEqual(self.fixture.state()["active"], "inactive")
                if enabled.startswith("masked"):
                    self.assertEqual(self.fixture.state()["enabled"], enabled)
                else:
                    self.assertFalse(
                        any(
                            call[0] == "start" for call in self.fixture.state()["calls"]
                        )
                    )
                self.assertFalse(self.fixture.marker.exists())

    def test_remask_failure_warns_and_cleanup_retries_without_enabling(self):
        self.fixture.reset("masked", "active")
        self.phase("pre")
        self.fixture.old_final_erase()
        state = self.fixture.state()
        state["fail"] = ["mask"]
        self.fixture.data.write_text(json.dumps(state))
        result = self.fixture.run("posttrans")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not restore the original mask", result.stderr)
        calls = self.fixture.state()["calls"]
        self.assertEqual(sum(call[0] == "mask" for call in calls), 2)
        self.assertFalse(any(call[0] == "enable" for call in calls))
        self.assertFalse(self.fixture.marker.exists())

    def test_daemon_reload_failure_prevents_start(self):
        self.phase("pre")
        self.fixture.old_final_erase()
        state = self.fixture.state()
        state["fail"] = ["daemon-reload"]
        self.fixture.data.write_text(json.dumps(state))
        result = self.fixture.run("posttrans")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("service was not started", result.stderr)
        self.assertFalse(
            any(call[0] == "start" for call in self.fixture.state()["calls"])
        )
        self.assertFalse(self.fixture.marker.exists())

    def test_untrusted_marker_directory_and_invalid_state_are_not_consumed_as_commands(
        self,
    ):
        outside = self.fixture.base / "outside"
        outside.mkdir()
        self.fixture.marker.symlink_to(outside, target_is_directory=True)
        self.assertNotEqual(self.fixture.run("pre").returncode, 0)
        self.fixture.marker.unlink()
        self.phase("pre")
        (self.fixture.marker / "state").write_text("system\ninvalid\nactive\n")
        result = self.fixture.run("posttrans")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.fixture.marker.exists())
        self.assertFalse(
            any(call[0] == "start" for call in self.fixture.state()["calls"])
        )


@unittest.skipUnless(
    sys.platform.startswith("linux")
    and os.geteuid() == 0
    and shutil.which("rpm")
    and shutil.which("rpmbuild"),
    "Linux root with rpm and rpmbuild is required",
)
class RealRpmTransactionTests(unittest.TestCase):
    def test_renamed_cli_final_erase_and_three_real_packages(self):
        # This isolated rpmdb and all fixture payloads live below one temp dir.
        # No host service or system rpmdb is touched; no Rust build is performed.
        with tempfile.TemporaryDirectory(prefix="asc-rpm-transaction-") as temporary:
            base = Path(temporary).resolve()
            fixture = MigrationFixture(base)
            real_rpm = shutil.which("rpm")
            database = base / "rpmdb"
            database.mkdir()
            fixture.rpm.write_text(
                f'#!/bin/sh\nexec "{real_rpm}" --dbpath "{database}" "$@"\n'
            )
            fixture.rpm.chmod(0o755)
            subprocess.run(
                [real_rpm, "--dbpath", str(database), "--initdb"],
                check=True,
                capture_output=True,
            )
            top = base / "rpmbuild"
            for name in ("BUILD", "BUILDROOT", "RPMS", "SOURCES", "SPECS", "SRPMS"):
                (top / name).mkdir(parents=True)
            payload = base / "payload"
            old = f"""Name: agent-sec-cli
Version: 1.1.1
Release: 1
Summary: Legacy CLI transaction fixture
License: MIT
%description
Legacy service owner.
%install
mkdir -p %{{buildroot}}{payload}
echo old > %{{buildroot}}{payload}/core
%preun
if [ "$1" -eq 0 ]; then
    "{fixture.systemctl}" disable --now agent-sec-core.service
fi
%postun
"{fixture.systemctl}" daemon-reload
%files
{payload}/core
"""
            header = SPEC.read_text().split("%description", 1)[0]
            renamed = "\n".join(
                line
                for line in header.splitlines()
                if line.startswith(("Provides:", "Obsoletes:"))
            )
            new = f"""Name: agent-sec-core
Version: 1.1.1
Release: 1
Summary: Three-package transaction fixture
License: MIT
{renamed}
%description
Core fixture.
"""
            for name in ("agent-sec-plugins", "agent-sec-skills"):
                new += (
                    "%package -n "
                    + name
                    + "\n"
                    + fixture.parsed["package", name]
                    + "\n%description -n "
                    + name
                    + "\nRole fixture.\n"
                )
            for phase in ("pre", "post", "posttrans", "preun", "postun"):
                new += "%" + phase + "\n" + fixture.render(phase, for_rpm=True) + "\n"
            new += f"%install\nmkdir -p %{{buildroot}}{payload}\n"
            for name in ("core", "plugins", "skills"):
                new += f"echo new > %{{buildroot}}{payload}/{name}\n"
            new += f"%files\n{payload}/core\n%files -n agent-sec-plugins\n{payload}/plugins\n%files -n agent-sec-skills\n{payload}/skills\n"
            for name, text in (("old", old), ("new", new)):
                spec = top / "SPECS" / (name + ".spec")
                spec.write_text(text)
                subprocess.run(
                    [
                        "rpmbuild",
                        "-bb",
                        "--define",
                        f"_topdir {top}",
                        "--define",
                        "debug_package %{nil}",
                        str(spec),
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                )
            old_rpm = next((top / "RPMS").rglob("agent-sec-cli-1.1.1-1.*.rpm"))
            new_rpms = sorted(
                path for path in (top / "RPMS").rglob("*.rpm") if path != old_rpm
            )
            self.assertEqual(len(new_rpms), 3)
            command = [real_rpm, "--dbpath", str(database), "--nodeps", "-Uvh"]
            subprocess.run(
                command + [str(old_rpm)],
                check=True,
                capture_output=True,
                env=fixture.environment,
            )
            # A refused capture must leave the old owner and its running service.
            state = fixture.state()
            state["fail"] = ["is-enabled"]
            fixture.data.write_text(json.dumps(state))
            core_rpm = next(
                path for path in new_rpms if path.name.startswith("agent-sec-core-")
            )
            refused = subprocess.run(
                command + [str(core_rpm)],
                check=False,
                capture_output=True,
                env=fixture.environment,
            )
            self.assertNotEqual(refused.returncode, 0)
            previous = subprocess.run(
                [real_rpm, "--dbpath", str(database), "-qa", "--qf", "%{NAME}\n"],
                check=True,
                text=True,
                capture_output=True,
            ).stdout.splitlines()
            self.assertEqual(set(previous), {"agent-sec-cli"})
            self.assertEqual(fixture.state()["enabled"], "enabled")
            self.assertEqual(fixture.state()["active"], "active")
            state = fixture.state()
            state["fail"] = []
            fixture.data.write_text(json.dumps(state))
            subprocess.run(
                command + [str(path) for path in new_rpms],
                check=True,
                capture_output=True,
                env=fixture.environment,
            )
            self.assertEqual(fixture.state()["enabled"], "enabled")
            self.assertEqual(fixture.state()["active"], "active")
            self.assertFalse(fixture.marker.exists())
            installed = subprocess.run(
                [real_rpm, "--dbpath", str(database), "-qa", "--qf", "%{NAME}\n"],
                check=True,
                text=True,
                capture_output=True,
            ).stdout.splitlines()
            self.assertEqual(
                set(installed),
                {"agent-sec-core", "agent-sec-plugins", "agent-sec-skills"},
            )
            for name, owner in (
                ("agent-sec-cli", "agent-sec-core"),
                *((name, "agent-sec-plugins") for name in HOOKS),
            ):
                provided = subprocess.run(
                    [
                        real_rpm,
                        "--dbpath",
                        str(database),
                        "-q",
                        "--whatprovides",
                        name,
                        "--qf",
                        "%{NAME}",
                    ],
                    check=True,
                    text=True,
                    capture_output=True,
                )
                self.assertEqual(provided.stdout, owner)
            self.assertEqual((payload / "core").read_text().strip(), "new")


if __name__ == "__main__":
    unittest.main()
