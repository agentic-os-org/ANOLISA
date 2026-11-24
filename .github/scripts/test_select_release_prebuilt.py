"""Exercise release job ownership and the direct component triggers it complements."""

import subprocess
import tempfile
import unittest
from contextlib import chdir
from pathlib import Path
from unittest.mock import patch

import yaml

import select_release_prebuilt as scope


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / scope.WORKFLOWS
RELEASE = (ROOT / scope.RELEASE).read_text()
FIXTURE = """name: Publish
permissions:
  actions: read
jobs:
  aw_assets:
    if: needs.package.outputs.component == 'aw'
    uses: ./.github/workflows/aw-release.yml
    permissions:
      actions: read
  build_prebuilt:
    name: Build precompiled packages
"""


class ReleaseScopeTests(unittest.TestCase):
    def test_aw_job_and_shared_edits(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=directory, text=True).strip()

            git("init", "-q")
            git("config", "user.email", "test@example.org")
            git("config", "user.name", "Test")
            path = Path(directory) / scope.RELEASE
            path.parent.mkdir(parents=True)
            path.write_text(FIXTURE)
            git("add", ".")
            git("commit", "-qm", "base")
            base = git("rev-parse", "HEAD")
            before, job = FIXTURE.split("\n  aw_assets:\n", 1)
            aw_edit = before + "\n  aw_assets:\n" + job.replace("actions: read", "actions: none", 1)
            for name, candidate, expected in (
                ("AW caller permissions", aw_edit, False),
                ("global permissions", FIXTURE.replace("actions: read", "actions: none", 1), True),
                ("mixed ownership", aw_edit.replace("name: Build precompiled", "name: Build shared"), True),
                ("changed job scope", aw_edit.replace("if: needs.package.outputs.component == 'aw'",
                                                      "if: always()", 1), True),
                ("removed workflow", None, True),
            ):
                with self.subTest(name=name):
                    if candidate is None:
                        path.unlink()
                    else:
                        path.write_text(candidate)
                    git("commit", "-qam", name)
                    with chdir(directory):
                        self.assertEqual(scope.shared_release_changed(base, "HEAD"), expected)

    def test_alias_can_expose_an_aw_value_to_another_job(self):
        source = FIXTURE.replace("  aw_assets:", "  aw_assets: &config").replace(
            "  build_prebuilt:", "  consumer: *config\n  build_prebuilt:"
        )
        self.assertIsNone(scope.shared_sections(source))

    def test_component_triggers_and_the_shared_router_agree(self):
        router = yaml.load((WORKFLOWS / "release-prebuilt-scope.yml").read_text(), Loader=yaml.BaseLoader)
        owners = {path.name.removesuffix("-prebuilt-ci.yaml"): path for path in WORKFLOWS.glob("*-prebuilt-ci.yaml")}
        self.assertEqual(set(router["jobs"]) - {"scope"}, owners.keys())
        for owner, path in owners.items():
            with self.subTest(owner=owner):
                job = router["jobs"][owner]
                self.assertEqual(job["uses"], "./.github/workflows/" + path.name)
                self.assertEqual(job["if"], f"contains(fromJSON(needs.scope.outputs.components), '{owner}')")
                triggers = yaml.load(path.read_text(), Loader=yaml.BaseLoader)["on"]
                self.assertIn("workflow_call", triggers)
                for event in {"push", "pull_request"} & triggers.keys():
                    config = triggers[event]
                    self.assertLessEqual(config.keys(), {"paths", "branches"})
                    for pattern in config["paths"] + config.get("branches", []):
                        scope.matches("", pattern)
                    self.assertNotIn(scope.RELEASE, config["paths"])
                    self.assertIn(".github/workflows/release-preview.yaml", triggers[event]["paths"])

    def test_direct_component_checks_cover_mixed_changes(self):
        with patch.object(scope, "WORKFLOWS", WORKFLOWS):
            paths = {scope.RELEASE}
            all_owners = scope.select("pull_request", "main", paths)
            self.assertEqual(len(all_owners), 9)
            paths.add("src/tokenless/Makefile")
            self.assertEqual(scope.select("pull_request", "main", paths),
                             [owner for owner in all_owners if owner != "tokenless"])
            self.assertEqual(scope.select("pull_request", "main", paths | {".github/workflows/release-preview.yaml"}), [])
            self.assertEqual(scope.select("push", "release/test", {scope.RELEASE}),
                             [owner for owner in all_owners if owner != "os-skills"])
            self.assertNotIn("tokenless", scope.select("pull_request", "maintenance", {scope.RELEASE}))
            self.assertEqual(scope.select("pull_request", "main", None), all_owners)
            self.assertEqual(scope.select("pull_request", "main", paths | {str(i) for i in range(300)}), all_owners)

    def test_shared_caller_preserves_aw_acceptance(self):
        jobs = yaml.load(RELEASE, Loader=yaml.BaseLoader)["jobs"]
        self.assertEqual(jobs["aw_assets"]["uses"], "./.github/workflows/aw-release.yml")
        self.assertEqual(jobs["aw_assets"]["if"], "needs.package.outputs.component == 'aw'")
        self.assertEqual(jobs["aw_assets"]["permissions"], {"actions": "read", "contents": "read"})
        self.assertIn("aw_assets", jobs["publish"]["needs"])
        self.assertIn("needs.aw_assets.result == 'success'", jobs["publish"]["if"])


if __name__ == "__main__":
    unittest.main()
