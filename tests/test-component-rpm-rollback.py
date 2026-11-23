"""Run old/split/old RPM transactions against private files and a fake systemd.

The RPM engine determines scriptlet ordering. No host package database, service,
mount or component data is accessed. Product payloads are not built here.
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

ROOT = Path(__file__).resolve().parents[1]
SYSTEMCTL = r"""import json, sys
from pathlib import Path
path = Path(sys.argv[1])
unit = sys.argv[2]
args = sys.argv[3:]
state = json.loads(path.read_text())
state['calls'].append(args)
command = args[0]
result = 0
output = ''
if command in ('is-enabled', 'is-active') and args[-1] != unit:
    output = 'not-found' if command == 'is-enabled' else 'inactive'
    result = 4 if command == 'is-enabled' else 3
elif any(arg.endswith('.service') and arg != unit for arg in args):
    pass
elif command == 'is-enabled':
    output = state['enabled']
    result = 1 if output in ('disabled', 'masked', 'masked-runtime') else 0
elif command == 'is-active':
    output = state['active']
    result = 0 if output == 'active' else 3
elif command == 'disable':
    if not state['enabled'].startswith('masked'):
        state['enabled'] = 'disabled'
    if '--now' in args:
        state['active'] = 'inactive'
elif command in ('enable', 'preset'):
    if state['enabled'].startswith('masked'):
        result = 1
    else:
        state['enabled'] = 'enabled-runtime' if '--runtime' in args else 'enabled'
        if '--now' in args:
            state['active'] = 'active'
elif command == 'unmask':
    state['enabled'] = 'disabled'
elif command == 'mask':
    state['enabled'] = 'masked-runtime' if '--runtime' in args else 'masked'
elif command in ('start', 'restart', 'try-restart'):
    if state['enabled'].startswith('masked'):
        result = 1
    elif command != 'try-restart' or state['active'] == 'active':
        state['active'] = 'active'
elif command == 'stop':
    state['active'] = 'inactive'
path.write_text(json.dumps(state))
if output:
    print(output)
sys.exit(result)
"""

# The distro's RPM bindings may belong to platform-python rather than the test
# interpreter. Keep this small driver compatible with both Python versions.
TRANSACTION = r"""import os, rpm, sys
rpm.addMacro('_dbpath', sys.argv[1])
ts = rpm.TransactionSet()
ts.setProbFilter(rpm.RPMPROB_FILTER_OLDPACKAGE)
for operation in sys.argv[2:]:
    if operation.startswith('erase:'):
        ts.addErase(operation[6:])
    else:
        fd = os.open(operation, os.O_RDONLY)
        header = ts.hdrFromFdno(fd)
        os.close(fd)
        ts.addInstall(header, operation, 'u')
ts.order()
opened = []
def callback(what, amount, total, key, data):
    if what == rpm.RPMCALLBACK_INST_OPEN_FILE:
        opened.append(os.open(key, os.O_RDONLY))
        return opened[-1]
    if what == rpm.RPMCALLBACK_INST_CLOSE_FILE:
        os.close(opened.pop())
errors = ts.run(callback, None)
if errors:
    raise RuntimeError(errors)
"""


def rpm_python():
    for interpreter in (sys.executable, "/usr/libexec/platform-python", "/usr/bin/python3"):
        if (
            Path(interpreter).exists()
            and subprocess.run(
                [interpreter, "-c", "import rpm"], capture_output=True, check=False
            ).returncode
            == 0
        ):
            return interpreter
    return None


@unittest.skipUnless(
    sys.platform.startswith("linux") and shutil.which("rpmbuild") and shutil.which("rpmspec"),
    "Linux RPM tools are required",
)
class RollbackTransactions(unittest.TestCase):
    def run_command(self, command):
        result = subprocess.run(command, text=True, capture_output=True, check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("scriptlet failed", result.stderr, result.stderr)
        return result.stdout

    def prepare(self, component):
        self.interpreter = rpm_python()
        if self.interpreter is None:
            self.skipTest("RPM Python bindings are required for a mixed install/erase transaction")
        temporary = tempfile.TemporaryDirectory(prefix="component-rpm-rollback-")
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name).resolve()
        for name in ("BUILD", "BUILDROOT", "RPMS", "SOURCES", "SPECS", "db", "manager", "bin"):
            (self.base / name).mkdir()
        self.component = component
        self.unit = "ws-ckpt.service" if component == "ws-ckpt" else "agent-sec-core.service"
        self.unit_path = self.base / "payload" / self.unit
        self.state = self.base / "systemctl.json"
        self.systemctl = self.base / "bin/systemctl"
        (self.base / "systemctl.py").write_text(SYSTEMCTL)
        self.systemctl.write_text(
            f'#!/bin/sh\nexec "{sys.executable}" "{self.base}/systemctl.py" "{self.state}" "{self.unit}" "$@"\n'
        )
        self.systemctl.chmod(0o755)
        self.rpm = self.base / "bin/rpm"
        self.rpm.write_text(
            f'#!/bin/sh\nexec "{shutil.which("rpm")}" --dbpath "{self.base}/db" "$@"\n'
        )
        self.rpm.chmod(0o755)
        (self.base / "transaction.py").write_text(TRANSACTION)
        self.run_command([str(self.rpm), "--initdb"])

    def render(self, relative):
        spec = self.base / "input.spec"
        spec.write_text((ROOT / relative).read_text().replace("@VERSION@", "9.0.0"))
        text = self.run_command(
            [
                "rpmspec",
                "-P",
                "--define",
                "systemd_post() systemctl preset %1 || :",
                "--define",
                'systemd_preun() if [ "$1" -eq 0 ]; then systemctl disable --now %1; fi',
                "--define",
                'systemd_postun_with_restart() systemctl daemon-reload; if [ "$1" -ge 1 ]; then systemctl try-restart %1 || :; fi',
                str(spec),
            ]
        )
        # Redirect every external operation used by the production scriptlets.
        for source, target in (
            ("/usr/bin/systemctl", str(self.systemctl)),
            ("/usr/lib/systemd/system/" + self.unit, str(self.unit_path)),
            ("/usr/lib/systemd/user/" + self.unit, str(self.base / "legacy-unit")),
            ("/run/systemd/system", str(self.base / "manager")),
            ("/run/", str(self.base / "run") + "/"),
            ("/var/lib/ws-ckpt", str(self.base / "data")),
            ("/data/ws-ckpt", str(self.base / "legacy-data")),
            ("/mnt/btrfs-workspace", str(self.base / "mount")),
            ("/usr/bin/ws-ckpt", str(self.base / "absent-ws-binary")),
        ):
            text = text.replace(source, target)
        text = re.sub(r"(?<![/\w-])systemctl\b", str(self.systemctl), text)
        text = text.replace("$(rpm ", f'$("{self.rpm}" ')
        text = text.replace("    umount ", "    : ")
        text = text.replace("        losetup ", "        : ")
        text = text.replace("xargs -r losetup -d", "xargs -r true")
        return text

    def build(self, name, version, scripts="", requires="", owns_unit=False):
        # Use a distinct build filename for the old/new package of the same NAME.
        spec = self.base / "SPECS" / f"{name}-{version}.spec"
        body = f"""Name: {name}
Version: {version}
Release: 1
Summary: Isolated service rollback fixture
License: MIT
{requires}
%description
Tests package ownership and scriptlet order only.
{scripts}
%install
mkdir -p %{{buildroot}}{self.base}/payload
"""
        if owns_unit:
            body += f"echo unit > %{{buildroot}}{self.unit_path}\n"
        body += "%files\n"
        if owns_unit:
            body += str(self.unit_path) + "\n"
        spec.write_text(body)
        self.run_command(["rpmbuild", "-bb", "--define", f"_topdir {self.base}", str(spec)])
        return str(next((self.base / "RPMS").rglob(f"{name}-{version}-1.*.rpm")))

    def scriptlets(self, text, suffix):
        result = ""
        pattern = r"^%(pre|post|posttrans|preun|postun|triggerprein)\b([^\n]*)\n(.*?)(?=^%[a-z]|\Z)"
        for match in re.finditer(pattern, text, re.MULTILINE | re.DOTALL):
            phase, options, body = match.groups()
            # rpmspec already expanded the source; preserve literal query formats
            # when embedding these scriptlets in the fixture spec.
            body = body.replace("%", "%%")
            if phase == "triggerprein":
                if options.strip().startswith(suffix + " --" if suffix else "--"):
                    result += f'%{phase} --{options.split("--", 1)[1]}\n{body}\n'
            elif options.strip() == suffix:
                result += f"%{phase}\n{body}\n"
        return result

    def transaction(self, *packages):
        self.run_command(
            [self.interpreter, str(self.base / "transaction.py"), str(self.base / "db"), *packages]
        )

    def exercise(self, old, new, erases):
        self.state.write_text(
            json.dumps({"enabled": "disabled", "active": "inactive", "calls": []})
        )
        self.transaction(*old)
        self.transaction(*new)
        states = (
            ("enabled", "active"),
            ("disabled", "inactive"),
            ("enabled", "inactive"),
            ("disabled", "active"),
            ("enabled-runtime", "active"),
            ("masked", "inactive"),
            ("masked-runtime", "inactive"),
            ("masked", "active"),
            ("masked-runtime", "active"),
        )
        for enabled, active in states:
            with self.subTest(enabled=enabled, active=active):
                self.state.write_text(
                    json.dumps({"enabled": enabled, "active": active, "calls": []})
                )
                self.transaction(*old, *["erase:" + name for name in erases])
                state = json.loads(self.state.read_text())
                self.assertEqual((state["enabled"], state["active"]), (enabled, active))
                self.assertFalse(list((self.base / "run").glob("*/state")))
                self.transaction(*new)
        # A real uninstall must still stop/disable; no rollback state may leak.
        self.state.write_text(json.dumps({"enabled": "enabled", "active": "active", "calls": []}))
        names = self.run_command([str(self.rpm), "-qa", "--qf", "%{NAME}\n"]).splitlines()
        self.transaction(*["erase:" + name for name in names])
        state = json.loads(self.state.read_text())
        self.assertEqual((state["enabled"], state["active"]), ("disabled", "inactive"))

    def test_ws_ckpt_rollback(self):
        self.prepare("ws-ckpt")
        text = self.render("src/ws-ckpt/packaging/rpm/ws-ckpt.spec.in")
        old = self.build(
            "ws-ckpt",
            "0.6.0",
            owns_unit=True,
            scripts=f"""%post
{self.systemctl} enable {self.unit} || :
{self.systemctl} restart {self.unit} || :
%preun
if [ "$1" -eq 0 ]; then {self.systemctl} disable --now {self.unit}; fi
""",
        )
        core = self.build(
            "ws-ckpt-core", "0.6.1", owns_unit=True, scripts=self.scriptlets(text, "core")
        )
        meta = self.build("ws-ckpt", "0.6.1", requires="Requires: ws-ckpt-core = 0.6.1-1")
        self.exercise([old], [core, meta], ["ws-ckpt-core"])

    def test_sec_core_rollback(self):
        self.prepare("agent-sec-core")
        text = self.render("src/agent-sec-core/packaging/rpm/agent-sec-core.spec.v2.in")
        old_cli = self.build(
            "agent-sec-cli",
            "1.1.1",
            owns_unit=True,
            scripts=f"""%post
{self.systemctl} enable --now {self.unit} || :
%preun
if [ "$1" -eq 0 ]; then {self.systemctl} disable --now {self.unit}; fi
""",
        )
        old_meta = self.build(
            "agent-sec-core", "1.1.1", requires="Requires: agent-sec-cli = 1.1.1-1"
        )
        core = self.build(
            "agent-sec-core",
            "1.1.2",
            owns_unit=True,
            scripts=self.scriptlets(text, ""),
            requires="Provides: agent-sec-cli = 1.1.2-1\nObsoletes: agent-sec-cli < 1.1.2",
        )
        self.exercise([old_cli, old_meta], [core], [])


if __name__ == "__main__":
    unittest.main()
