#!/usr/bin/env python3
"""Exercise component release notes with real Git history and a stub GitHub CLI."""

import json
import os
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

from generate_release_notes import version_key

GITHUB = Path(__file__).resolve().parents[1]
WORKFLOW = GITHUB / "workflows/release.yaml"
SEC_PATH = "src/agent-sec-core/change.txt"
TOKENLESS_PATH = "src/tokenless/change.txt"


class VersionPrecedenceTest(unittest.TestCase):
    def test_semver_precedence(self) -> None:
        versions = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0-rc.10",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "1.10.0",
            "2.0.0",
        ]
        for earlier, later in zip(versions, versions[1:]):
            self.assertLess(version_key(earlier), version_key(later))
        self.assertEqual(version_key("1.0.0+build.1"), version_key("1.0.0+build.2"))

    def test_invalid_semver(self) -> None:
        for version in ("1.0", "01.0.0", "1.0.0-01", "1.0.0-rc..1", "1.0.0+"):
            with self.subTest(version=version):
                self.assertIsNone(version_key(version))


class ReleaseNotesTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="anolisa-release-notes-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.git("init", "--quiet")
        self.git("config", "user.name", "kongche-jbw")
        self.git("config", "user.email", "kongche.jbw@alibaba-inc.com")
        self.commit()
        scripts = self.root / ".github/scripts"
        scripts.mkdir(parents=True)
        shutil.copy2(GITHUB / "scripts/generate_release_notes.py", scripts)
        shutil.copy2(GITHUB / "components.json", scripts.parent)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        gh = self.bin / "gh"
        gh.write_text(
            textwrap.dedent("""\
                #!/usr/bin/env python3
                import json
                import os
                import sys

                with open(os.environ["GH_CALLS"], "a") as output:
                    output.write(json.dumps(sys.argv[1:]) + "\\n")
                if sys.argv[1:3] == ["release", "create"]:
                    sys.exit(int(os.environ.get("CREATE_EXIT", "0")))
                if "--json" in sys.argv:
                    print("https://github.com/example/anolisa/releases/tag/test")
                else:
                    sys.exit(int(os.environ.get("VIEW_EXIT", "1")))
                """),
            encoding="utf-8",
        )
        gh.chmod(0o755)

    def git(self, *arguments: str) -> str:
        return subprocess.check_output(
            ["git", *arguments], cwd=self.root, text=True, timeout=10
        ).strip()

    def commit(self, message: str = "fixture", *paths: str) -> str:
        for name in paths:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(message, encoding="utf-8")
        if paths:
            self.git("add", "--", *paths)
        self.git("-c", "commit.gpgsign=false", "commit", "--quiet", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")

    def publish(
        self, component: str = "agent-sec-core", tag: str = "sec-core/v1.2.3", **overrides: str
    ) -> tuple[subprocess.CompletedProcess, list[list[str]]]:
        source = WORKFLOW.read_text(encoding="utf-8")
        step = source.split("      - name: Create GitHub Release\n", 1)[1]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        for key, value in {
            "needs.package.outputs.component": component,
            "needs.package.outputs.version": tag.split("/v", 1)[1],
            "needs.package.outputs.tag": tag,
            "github.repository": "example/anolisa",
        }.items():
            script = script.replace("${{ " + key + " }}", value)
        script = script.replace("/tmp/", str(self.root) + "/")
        calls_file = self.root / "gh-calls.jsonl"
        calls_file.write_text("", encoding="utf-8")
        result = subprocess.run(
            ["bash", "-eo", "pipefail", "-c", script],
            cwd=self.root,
            env=dict(
                os.environ,
                PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                GH_CALLS=str(calls_file),
                GITHUB_STEP_SUMMARY=str(self.root / "summary"),
                GITHUB_REPOSITORY="example/anolisa",
                RUNNER_TEMP=str(self.root),
                PREBUILT_ENABLED="false",
                **overrides,
            ),
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        calls = [json.loads(line) for line in calls_file.read_text().splitlines()]
        return result, calls

    def notes(self) -> str:
        return (self.root / "release-notes.md").read_text(encoding="utf-8")

    def test_component_baseline_excludes_newer_and_unmerged_tags(self) -> None:
        self.commit("old security change", SEC_PATH)
        self.git("tag", "-a", "sec-core/v1.0.0", "-m", "previous release")
        self.git("tag", "sec-core/v1.2.30")
        self.git("tag", "tokenless/v9.0.0")
        previous = self.git("rev-parse", "HEAD")
        self.commit("unmerged release", SEC_PATH)
        self.git("tag", "sec-core/v1.2.2")
        self.git("checkout", "--quiet", "--detach", previous)
        sha = self.commit("new security change", SEC_PATH)
        self.git("tag", "sec-core/v1.2.3")

        result, calls = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        create = next(call for call in calls if call[:2] == ["release", "create"])
        self.assertEqual(
            create[create.index("--notes-file") + 1], str(self.root / "release-notes.md")
        )
        self.assertEqual(create[create.index("--title") + 1], "agent-sec-core v1.2.3")
        self.assertEqual(create[-1], str(self.root / "archives/agent-sec-core-1.2.3.tar.gz"))
        self.assertNotIn("--generate-notes", create)
        self.assertIn("new security change", self.notes())
        self.assertIn(f"/commit/{sha}", self.notes())
        self.assertNotIn("old security change", self.notes())
        self.assertNotIn("unmerged release", self.notes())
        self.assertIn("sec-core/v1.0.0...sec-core/v1.2.3", self.notes())
        self.assertIn("sec-core/v1.0.0...sec-core/v1.2.3", (self.root / "summary").read_text())

    def test_paths_filter_changes_instead_of_commit_subjects(self) -> None:
        self.git("tag", "sec-core/v1.2.2")
        self.commit("security fix", SEC_PATH)
        self.commit("fix(sec-core): misleading subject", TOKENLESS_PATH)
        self.commit("shared change", SEC_PATH, TOKENLESS_PATH)
        self.commit("sibling directory change", "src/agent-sec-core-extra/change.txt")
        self.git("tag", "sec-core/v1.2.3")
        result, _ = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("security fix", self.notes())
        self.assertEqual(self.notes().count("shared change"), 1)
        self.assertNotIn("misleading subject", self.notes())
        self.assertNotIn("sibling directory change", self.notes())

    def test_first_release_only_includes_component_history(self) -> None:
        self.commit("initial security feature", SEC_PATH)
        self.commit("tokenless feature", TOKENLESS_PATH)
        self.git("tag", "tokenless/v9.0.0")
        self.commit("next security feature", SEC_PATH)
        self.git("tag", "sec-core/v1.2.3")
        result, _ = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("initial security feature", self.notes())
        self.assertIn("next security feature", self.notes())
        self.assertNotIn("tokenless feature", self.notes())
        self.assertIn("/commits/sec-core/v1.2.3", self.notes())

    def test_rc_to_stable_and_next_patch_baselines(self) -> None:
        self.commit("previous stable", TOKENLESS_PATH)
        self.git("tag", "tokenless/v0.8.9")
        self.commit("rc two", TOKENLESS_PATH)
        self.git("tag", "tokenless/v0.9.0-rc.2")
        self.commit("rc ten", TOKENLESS_PATH)
        self.git("tag", "tokenless/v0.9.0-rc.10")
        self.commit("stable change", TOKENLESS_PATH)
        self.git("tag", "tokenless/v0.9.0")
        result, _ = self.publish(component="tokenless", tag="tokenless/v0.9.0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("tokenless/v0.9.0-rc.10...tokenless/v0.9.0", self.notes())
        self.assertIn("stable change", self.notes())
        self.assertNotIn("rc ten", self.notes())
        self.commit("patch change", TOKENLESS_PATH)
        self.git("tag", "tokenless/v0.9.1")
        result, _ = self.publish(component="tokenless", tag="tokenless/v0.9.1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("tokenless/v0.9.0...tokenless/v0.9.1", self.notes())
        self.assertIn("patch change", self.notes())
        self.assertNotIn("stable change", self.notes())

    def test_merge_is_summarized_once(self) -> None:
        self.git("tag", "sec-core/v1.2.2")
        main = self.git("rev-parse", "HEAD")
        self.git("checkout", "--quiet", "-b", "feature")
        self.commit("feature implementation", SEC_PATH)
        self.commit("feature tests", "src/agent-sec-core/test.txt")
        self.git("checkout", "--quiet", "--detach", main)
        self.commit("unrelated main change", TOKENLESS_PATH)
        self.git(
            "-c",
            "commit.gpgsign=false",
            "merge",
            "--quiet",
            "--no-ff",
            "feature",
            "-m",
            "land security feature (#42)",
        )
        self.git("tag", "sec-core/v1.2.3")
        result, _ = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("land security feature (#42)", self.notes())
        self.assertNotIn("feature implementation", self.notes())
        self.assertNotIn("unrelated main change", self.notes())

    def test_all_release_components_use_metadata_paths(self) -> None:
        metadata = json.loads((GITHUB / "components.json").read_text())
        for component in metadata["components"]:
            prefix = component["release_tag_prefix"]
            if not prefix:
                continue
            with self.subTest(component=component["id"]):
                self.commit(
                    component["display_name"] + " feature",
                    component["path_prefixes"][0] + "feature.txt",
                )
                tag = prefix + "0.1.0"
                self.git("tag", tag)
                result = subprocess.run(
                    [
                        "python3",
                        ".github/scripts/generate_release_notes.py",
                        "--tag",
                        tag,
                        "--prefix",
                        prefix,
                        "--repository",
                        "example/anolisa",
                        "--output",
                        str(self.root / "release-notes.md"),
                    ],
                    cwd=self.root,
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(component["display_name"] + " feature", self.notes())
                self.assertEqual(self.notes().count("/commit/"), 1)

    def test_no_component_changes_is_explicit(self) -> None:
        self.git("tag", "sec-core/v1.2.2")
        self.commit("only tokenless changed", TOKENLESS_PATH)
        self.git("tag", "sec-core/v1.2.3")
        result, _ = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("No changes in the component paths.", self.notes())
        self.assertNotIn("only tokenless changed", self.notes())

    def test_vendor_asset_is_preserved(self) -> None:
        self.git("tag", "skillfs/v0.5.1")
        self.commit("SkillFS change", "src/skillfs/change.txt")
        self.git("tag", "skillfs/v0.5.2")
        vendor = self.root / "archives/skillfs-0.5.2-vendor.tar.gz"
        vendor.parent.mkdir()
        vendor.touch()
        result, calls = self.publish(component="skillfs", tag="skillfs/v0.5.2")
        self.assertEqual(result.returncode, 0, result.stderr)
        create = next(call for call in calls if call[:2] == ["release", "create"])
        self.assertIn(str(vendor), create)

    def test_generation_failure_stops_before_creating_release(self) -> None:
        self.git("tag", "sec-core/vinvalid")
        result, calls = self.publish(tag="sec-core/vinvalid")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Release tag is not SemVer", result.stderr)
        self.assertEqual(calls, [])

    def test_create_failure_stops_publish(self) -> None:
        self.git("tag", "sec-core/v1.2.3")
        result, calls = self.publish(CREATE_EXIT="42")
        self.assertEqual(result.returncode, 42)
        self.assertEqual(calls[-1][:2], ["release", "create"])
        self.assertFalse((self.root / "summary").exists())

    def test_existing_release_is_not_overwritten(self) -> None:
        self.git("tag", "sec-core/v1.2.3")
        result, calls = self.publish(VIEW_EXIT="0")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 1)
        self.assertIn("already exists", result.stdout)


if __name__ == "__main__":
    unittest.main()
