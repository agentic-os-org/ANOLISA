"""Regression checks for the `make install` target of the os-skills Makefile.

The install loop used to ignore failures: it ran `install -d` and `cp -pr`
per skill without checking either, so a copy that failed mid-loop (read-only
or full `DESTDIR` staging root, permissions, ENOSPC) was masked by the next
successful iteration. `make install` then exited 0 and went on to install
`component.toml`, shipping a partial skill set as a success — and this target
is the packaging path used by `os-skills.spec.in` and the prebuilt action.

These tests stub `cp`/`install` on `PATH` so an early, a mid-loop and a late
failure can each be proven to fail the target, while the clean path still
installs every skill and the component manifest.
"""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

SKILLS = ("alpha", "beta", "gamma")

STUB_TEMPLATE = """#!/bin/sh
# Test stub for {name}: fail on the invocation numbered by $FAIL_ON,
# otherwise behave like the real binary.
state="${{STUB_STATE:?STUB_STATE must be set}}"
count=$(cat "$state" 2>/dev/null || echo 0)
count=$((count + 1))
printf '%s\\n' "$count" > "$state"
if [ -n "${{FAIL_ON:-}}" ] && [ "$count" -eq "$FAIL_ON" ]; then
    echo "{name}: stubbed failure on invocation $count: $*" >&2
    exit 7
fi
exec {real} "$@"
"""


def write_stub(bin_dir: Path, name: str) -> Path:
    real = shutil.which(name)
    if real is None:
        raise unittest.SkipTest(f"{name} not available on PATH")
    stub = bin_dir / name
    stub.write_text(STUB_TEMPLATE.format(name=name, real=real))
    stub.chmod(0o755)
    return stub


def build_component(directory: Path) -> Path:
    """A minimal component tree: the real Makefile plus three fake skills."""
    directory.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / "Makefile", directory / "Makefile")
    shutil.copy2(ROOT / "component.toml", directory / "component.toml")
    for name in SKILLS:
        skill = directory / name
        skill.mkdir()
        (skill / "SKILL.md").write_text(f"# {name}\n")
        (skill / "payload.txt").write_text(f"{name} payload\n")
    return directory


@unittest.skipUnless(shutil.which("make"), "make not available")
class MakeInstallTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)
        self.bin_dir = self.tmp / "bin"
        self.bin_dir.mkdir()
        self.component = build_component(self.tmp / "component")
        self.destdir = self.tmp / "dest"

    def run_install(self, stub=None, fail_on=None):
        """Run `make install` with an optionally failing stub on PATH."""
        env = dict(os.environ)
        env["STUB_STATE"] = str(self.tmp / "stub-count")
        if stub is not None:
            write_stub(self.bin_dir, stub)
            env["PATH"] = f"{self.bin_dir}{os.pathsep}{env.get('PATH', '')}"
            env["FAIL_ON"] = str(fail_on)
        else:
            env.pop("FAIL_ON", None)
        return subprocess.run(
            ["make", "install", f"DESTDIR={self.destdir}"],
            cwd=str(self.component),
            env=env,
            capture_output=True,
            text=True,
        )

    def installed_skills(self):
        skills_dir = self.destdir / "usr/share/anolisa/skills"
        if not skills_dir.is_dir():
            return {}
        return {
            path.parent.name: sorted(p.name for p in path.parent.iterdir())
            for path in skills_dir.glob("*/SKILL.md")
        }

    def manifest_installed(self):
        return (self.destdir / "usr/share/anolisa/components/os-skills/component.toml").is_file()

    def test_clean_install_still_installs_everything(self):
        result = self.run_install()
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(set(SKILLS), set(self.installed_skills()))
        for name in SKILLS:
            payload = self.destdir / f"usr/share/anolisa/skills/{name}/payload.txt"
            self.assertTrue(payload.is_file(), payload)
        self.assertTrue(self.manifest_installed())

    def test_first_copy_failure_fails_the_target(self):
        result = self.run_install(stub="cp", fail_on=1)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("ERROR: failed to install skill", result.stderr)
        self.assertFalse(self.manifest_installed(),
                         "component.toml must not be installed over a partial tree")

    def test_mid_loop_copy_failure_is_not_masked(self):
        """The regression: a later successful iteration used to hide this failure."""
        result = self.run_install(stub="cp", fail_on=2)
        self.assertNotEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("ERROR: failed to install skill", result.stderr)
        installed = self.installed_skills()
        self.assertLess(len(installed), len(SKILLS),
                        f"the failing skill must not look installed: {installed}")
        self.assertFalse(self.manifest_installed())

    def test_last_copy_failure_fails_the_target(self):
        result = self.run_install(stub="cp", fail_on=len(SKILLS))
        self.assertNotEqual(0, result.returncode)
        self.assertIn("ERROR: failed to install skill", result.stderr)
        self.assertFalse(self.manifest_installed())

    def test_directory_creation_failure_fails_the_target(self):
        result = self.run_install(stub="install", fail_on=1)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("ERROR: failed to create install directory for skill", result.stderr)
        self.assertFalse(self.manifest_installed())

    def test_real_component_tree_installs_every_skill(self):
        """The shipped tree installs completely, with the manifest, via DESTDIR."""
        result = subprocess.run(
            ["make", "install", f"DESTDIR={self.destdir}"],
            cwd=str(ROOT),
            capture_output=True,
            text=True,
        )
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        expected = {path.parent.name for path in ROOT.rglob("SKILL.md")}
        installed = {
            path.parent.name
            for path in (self.destdir / "usr/share/anolisa/skills").glob("*/SKILL.md")
        }
        self.assertEqual(expected, installed)
        self.assertTrue(self.manifest_installed())


if __name__ == "__main__":
    unittest.main()
