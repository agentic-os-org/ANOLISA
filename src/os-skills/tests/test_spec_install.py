"""Regression checks for the RPM %install skill copy loop of os-skills (#4480).

The %install stage of ``os-skills.spec.in`` copies every skill directory into
the buildroot through a ``find | while`` pipeline whose exit status is only the
status of the loop's last command. rpmbuild runs the stage as ``/bin/sh -e``,
so errexit does trip inside the loop body and a failing copy aborts the build -
but two failure modes still escape it:

* ``cp -rp <skill> <buildroot>/.../skills/`` exits 0 when that skill's basename
  is already present in the tree - it merges into the existing directory - so
  two categories carrying the same skill name silently lose one of them and the
  RPM is built as a success;
* the recipe is not self-guarding: without errexit (extracted into a script, or
  copied into another recipe such as the component Makefile) a mid-loop failure
  is masked by the next successful copy and only cp's own message hints at it.

Each test extracts the real %install recipe from the spec, macro-expands
``%{buildroot}`` / ``%{_datadir}`` the way rpm does, and runs it under
``/bin/sh -e`` over a fixture component tree with a ``cp`` stub that fails one
chosen skill.

Baseline on unchanged main: 5 of the 7 tests fail - the three injected copy
failures abort without the recipe naming the offender, the same injection
without errexit exits 0 with a skill missing, and a basename collision exits 0
with a skill missing.
"""

import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = ROOT / "os-skills.spec.in"
DATADIR = "/usr/share"

# Fails the copy of the skill named by FAIL_SKILL_DIR (matched against any
# argument, so the stub does not depend on the position cp is called with) and
# passes every other invocation through to the real binary.
CP_STUB = """\
#!/bin/sh
if [ -n "${FAIL_SKILL_DIR:-}" ]; then
    for arg in "$@"; do
        if [ "$arg" = "$FAIL_SKILL_DIR" ]; then
            echo "cp: cannot copy '$arg': simulated failure" >&2
            exit 1
        fi
    done
fi
exec @REAL_CP@ "$@"
"""


def install_recipe():
    """Return the body of the spec's %install section, macros intact."""
    lines = SPEC.read_text().splitlines()
    body = []
    for line in lines[lines.index("%install") + 1:]:
        if re.match(r"^%(?!\{)", line):  # next spec section: %files, %changelog, ...
            break
        body.append(line)
    return "\n".join(body) + "\n"


class SpecInstallTestCase(unittest.TestCase):
    def setUp(self):
        self.work = Path(tempfile.mkdtemp(prefix="os-skills-spec-install-"))
        self.source = self.work / "os-skills"
        self.buildroot = self.work / "buildroot"
        self.addCleanup(shutil.rmtree, self.work, ignore_errors=True)

    def write_component(self, skills):
        """Lay out a component tree whose skill directories are `skills`."""
        self.source.mkdir(parents=True, exist_ok=True)
        (self.source / "component.toml").write_text('[component]\nname = "os-skills"\n')
        for skill in skills:
            skill_dir = self.source / skill
            skill_dir.mkdir(parents=True)
            (skill_dir / "SKILL.md").write_text(f"# {Path(skill).name}\n")
            (skill_dir / "run.sh").write_text("#!/bin/sh\necho payload\n")
            (skill_dir / "notes.md").write_text("payload\n")
        return self.source

    def run_install(self, source, fail_skill=None, errexit=True):
        """Run the spec's %install recipe over `source` like rpmbuild does.

        `errexit=False` drops the `-e` rpm passes to /bin/sh, which is how the
        recipe behaves once it is lifted out of the spec.
        """
        recipe = install_recipe()
        recipe = recipe.replace("%{buildroot}", str(self.buildroot))
        recipe = recipe.replace("%{_datadir}", DATADIR)
        stub_dir = self.work / "cp-stub"
        stub_dir.mkdir(exist_ok=True)
        stub = stub_dir / "cp"
        stub.write_text(CP_STUB.replace("@REAL_CP@", shutil.which("cp")))
        stub.chmod(0o755)
        script = self.work / "install.sh"
        script.write_text(recipe)
        env = dict(os.environ)
        env["PATH"] = f"{stub_dir}{os.pathsep}{env['PATH']}"
        env.pop("FAIL_SKILL_DIR", None)
        if fail_skill is not None:
            env["FAIL_SKILL_DIR"] = fail_skill
        # rpm runs a scriptlet as `/bin/sh -e <file>`: errexit is in force, so
        # a bare failing command aborts %install.
        shell = ["/bin/sh", "-e"] if errexit else ["/bin/sh"]
        return subprocess.run(
            shell + [str(script)],
            cwd=str(source),
            env=env,
            capture_output=True,
            text=True,
        )

    def installed_skills(self):
        skills = self.buildroot / DATADIR.lstrip("/") / "anolisa/skills"
        return {path.name for path in skills.iterdir()} if skills.is_dir() else set()


class TestCleanInstall(SpecInstallTestCase):
    def test_every_skill_lands_with_its_payload_and_modes(self):
        self.write_component(["ai/alpha", "devops/beta", "others/gamma"])
        result = self.run_install(self.source)
        self.assertEqual(result.returncode, 0, result.stderr)
        root = self.buildroot / DATADIR.lstrip("/") / "anolisa"
        self.assertEqual(self.installed_skills(), {"alpha", "beta", "gamma"})
        for skill in ("alpha", "beta", "gamma"):
            skill_dir = root / "skills" / skill
            self.assertEqual((skill_dir / "SKILL.md").read_text(), f"# {skill}\n")
            self.assertEqual(skill_dir.stat().st_mode & 0o777, 0o755)
            self.assertEqual((skill_dir / "run.sh").stat().st_mode & 0o777, 0o755)
            self.assertEqual((skill_dir / "notes.md").stat().st_mode & 0o777, 0o644)
            self.assertEqual((skill_dir / "SKILL.md").stat().st_mode & 0o777, 0o644)
        manifest = root / "components/os-skills/component.toml"
        self.assertEqual(manifest.stat().st_mode & 0o777, 0o644)


class TestCopyFailureAbortsTheBuild(SpecInstallTestCase):
    def assert_aborts_naming(self, fail_skill):
        self.write_component(["ai/alpha", "devops/beta", "others/gamma"])
        result = self.run_install(self.source, fail_skill=fail_skill)
        self.assertNotEqual(result.returncode, 0, f"{fail_skill}: recipe exited 0")
        self.assertIn(
            f"ERROR: os-skills: failed to copy skill: {fail_skill}", result.stderr
        )

    def test_mid_loop_failure_is_not_masked_by_later_copies(self):
        self.assert_aborts_naming("./devops/beta")
        # The loop stops at the offender: the skill sorted after it is absent.
        self.assertNotIn("gamma", self.installed_skills())

    def test_first_skill_failure_aborts(self):
        self.assert_aborts_naming("./ai/alpha")

    def test_last_skill_failure_aborts(self):
        self.assert_aborts_naming("./others/gamma")

    def test_mid_loop_failure_aborts_without_rpm_errexit(self):
        # Lifted out of the spec there is no `-e` to trip: only the guard in
        # the recipe itself can stop a mid-loop failure from being masked by
        # the copies that follow it.
        self.write_component(["ai/alpha", "devops/beta", "others/gamma"])
        result = self.run_install(self.source, fail_skill="./devops/beta", errexit=False)
        self.assertNotEqual(result.returncode, 0, "recipe exited 0")
        self.assertIn(
            "ERROR: os-skills: failed to copy skill: ./devops/beta", result.stderr
        )
        self.assertNotIn("gamma", self.installed_skills())


class TestBasenameCollisionAbortsTheBuild(SpecInstallTestCase):
    def test_two_categories_sharing_a_skill_name_fail_the_stage(self):
        self.write_component(["ai/dup", "devops/dup", "others/uniq"])
        result = self.run_install(self.source)
        # cp merges into the existing directory instead of failing, so only the
        # installed-vs-discovered count exposes the lost skill.
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("ERROR: os-skills: found 3 skills but installed 2", result.stderr)


class TestShippedComponentTree(SpecInstallTestCase):
    def test_real_tree_installs_completely(self):
        expected = {skill.parent.name for skill in ROOT.rglob("SKILL.md")}
        result = self.run_install(ROOT)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installed_skills(), expected)
        skills = self.buildroot / DATADIR.lstrip("/") / "anolisa/skills"
        for skill in sorted(expected):
            self.assertTrue((skills / skill / "SKILL.md").is_file(), skill)


if __name__ == "__main__":
    unittest.main()
