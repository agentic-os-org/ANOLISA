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
SEC_CHANGELOG = "src/agent-sec-core/CHANGELOG.md"
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
        self.git(
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            message,
        )
        return self.git("rev-parse", "HEAD")

    def publish(
        self,
        component: str = "agent-sec-core",
        tag: str = "sec-core/v1.2.3",
        **overrides: str,
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
                PREBUILT_ENABLED=overrides.pop("PREBUILT_ENABLED", "false"),
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

    def changelog(self, content: str, path: str = SEC_CHANGELOG) -> None:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
        self.git("add", "--", path)
        self.commit("chore(ci): maintain curated version notes")

    def release(
        self,
        tag: str = "sec-core/v1.2.3",
        body: str = "- Curated user-facing change.",
        path: str = SEC_CHANGELOG,
    ) -> None:
        self.changelog(f"# Changelog\n\n## {tag.split('/v', 1)[1]}\n\n{body}\n", path)
        self.git("tag", tag)

    def test_component_baseline_excludes_newer_and_unmerged_tags(self) -> None:
        self.commit("old security change", SEC_PATH)
        self.git("tag", "-a", "sec-core/v1.0.0", "-m", "previous release")
        self.git("tag", "sec-core/v1.2.30")
        self.git("tag", "tokenless/v9.0.0")
        previous = self.git("rev-parse", "HEAD")
        self.commit("unmerged release", SEC_PATH)
        self.git("tag", "sec-core/v1.2.2")
        self.git("checkout", "--quiet", "--detach", previous)
        self.release(body="- Curated security explanation.")

        result, calls = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        create = next(call for call in calls if call[:2] == ["release", "create"])
        self.assertEqual(
            create[create.index("--notes-file") + 1],
            str(self.root / "release-notes.md"),
        )
        self.assertEqual(create[create.index("--title") + 1], "agent-sec-core v1.2.3")
        self.assertEqual(create[-1], str(self.root / "archives/agent-sec-core-1.2.3.tar.gz"))
        self.assertNotIn("--generate-notes", create)
        self.assertIn("Curated security explanation.", self.notes())
        self.assertNotIn("old security change", self.notes())
        self.assertNotIn("unmerged release", self.notes())
        self.assertIn("sec-core/v1.0.0...sec-core/v1.2.3", self.notes())
        self.assertIn("sec-core/v1.0.0...sec-core/v1.2.3", (self.root / "summary").read_text())

    def test_curated_entry_is_the_only_content_source(self) -> None:
        self.commit("internal security implementation", SEC_PATH)
        self.commit("fix(sec-core): misleading subject", TOKENLESS_PATH)
        self.changelog("## 9.0.0\n\n- Tokenless changes.\n", "src/tokenless/CHANGELOG.md")
        body = "### Breaking changes\n\n- Upgrade callers together.\n\n### Migration\n\nRun `agent-sec-cli capabilities` before enabling the hook."
        self.changelog(
            "# Changelog\n\n## [Unreleased]\n\n- Future work.\n\n## [1.2.3] - 2026-10-15\n\n"
            + body
            + "\n\n## 1.2.2\n\n- Old changes.\n"
        )
        self.git("tag", "sec-core/v1.2.3")
        result, _ = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(body, self.notes())
        for excluded in (
            "internal security implementation",
            "misleading subject",
            "Tokenless changes",
            "Future work",
            "Old changes",
            "maintain curated",
        ):
            self.assertNotIn(excluded, self.notes())

    def test_heading_formats_match_exact_version(self) -> None:
        for heading in (
            "1.2.3",
            "[1.2.3] - 2026-10-15",
            "[1.2.3] — 2026-10-15",
            "v1.2.3",
            "[v1.2.3]",
        ):
            with self.subTest(heading=heading):
                self.changelog(
                    f"## 1.2.30\n\n- Wrong version.\n\n## {heading}\n\n- Exact version.\n\n## 1.2.2\n\n- Old version.\n"
                )
                self.git("tag", "-f", "sec-core/v1.2.3")
                result, _ = self.publish()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("Exact version.", self.notes())
                self.assertNotIn("Wrong version.", self.notes())
                self.assertNotIn("Old version.", self.notes())

    def test_fenced_examples_preserve_headings_and_links(self) -> None:
        body = "### Migration\n\n````markdown\n## 1.2.2\n[example](README.md)\n```\n````\n\n~~~sh\n## another example\n~~~\n\n- Complete explanation after examples."
        self.release(body=body)
        result, _ = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(body, self.notes())

    def test_tag_contents_win_over_checkout_and_uncommitted_edits(self) -> None:
        self.release(body="- Tagged explanation.")
        self.changelog("## 1.2.3\n\n- Later explanation.\n")
        (self.root / SEC_CHANGELOG).write_text("## 1.2.3\n\n- Uncommitted explanation.\n")
        result, _ = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Tagged explanation.", self.notes())
        self.assertNotIn("Later explanation.", self.notes())
        self.assertNotIn("Uncommitted explanation.", self.notes())

    def test_first_release_uses_curated_notes_and_history_link(self) -> None:
        self.commit("tokenless feature", TOKENLESS_PATH)
        self.git("tag", "tokenless/v9.0.0")
        self.release(body="- Initial security feature.")
        result, _ = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Initial security feature.", self.notes())
        self.assertNotIn("tokenless feature", self.notes())
        self.assertIn("/commits/sec-core/v1.2.3", self.notes())

    def test_rc_to_stable_and_next_patch_baselines(self) -> None:
        path = "src/tokenless/CHANGELOG.md"
        self.release("tokenless/v0.8.9", path=path)
        self.release("tokenless/v0.9.0-rc.2", path=path)
        self.release("tokenless/v0.9.0-rc.10", path=path)
        self.release("tokenless/v0.9.0", "- Stable explanation.", path)
        result, _ = self.publish(component="tokenless", tag="tokenless/v0.9.0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("tokenless/v0.9.0-rc.10...tokenless/v0.9.0", self.notes())
        self.assertIn("Stable explanation.", self.notes())
        self.release("tokenless/v0.9.1", "- Patch explanation.", path)
        result, _ = self.publish(component="tokenless", tag="tokenless/v0.9.1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("tokenless/v0.9.0...tokenless/v0.9.1", self.notes())
        self.assertIn("Patch explanation.", self.notes())
        self.assertNotIn("Stable explanation.", self.notes())

    def test_all_release_components_use_metadata_paths(self) -> None:
        metadata = json.loads((GITHUB / "components.json").read_text())
        for component in metadata["components"]:
            prefix = component["release_tag_prefix"]
            if not prefix:
                continue
            with self.subTest(component=component["id"]):
                body = "- " + component["display_name"] + " curated explanation."
                tag = prefix + "0.1.0"
                self.release(tag, body, component["path_prefixes"][0] + "CHANGELOG.md")
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
                self.assertIn(body, self.notes())
                self.assertNotIn("/commit/", self.notes())

    def test_relative_links_and_reference_definitions_use_tag_commit(self) -> None:
        body = (
            "- Read [migration](docs/migration.md#upgrade), "
            "[root](/README.md), [anchor](#migration), [detail](docs/upgrade(v2).md), "
            "![diagram](<docs/image name.png>), ![reference][picture], ![picture][], "
            "![picture], and [guide][upgrade].\n"
            "- Keep [external](https://example.com/path), `command [text](README.md)`, "
            "``nested ` tick [example](README.md)`` and `[ref]: literal`, "
            "and [email](mailto:maintainer@example.com) unchanged.\n"
        )
        self.changelog(
            "## 1.2.3\n\n"
            + body
            + '\n## 1.2.2\n\n- Old content.\n\n[upgrade]: ../guide.md "Migration guide"\n'
            + '[picture]: docs/diagram.png "Architecture"\n'
        )
        self.git("tag", "sec-core/v1.2.3")
        commit = self.git("rev-parse", "HEAD")
        result, _ = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        base = f"https://github.com/example/anolisa/blob/{commit}/"
        self.assertIn(
            f"[migration]({base}src/agent-sec-core/docs/migration.md#upgrade)",
            self.notes(),
        )
        self.assertIn(f"[detail]({base}src/agent-sec-core/docs/upgrade%28v2%29.md)", self.notes())
        self.assertIn("``nested ` tick [example](README.md)``", self.notes())
        self.assertIn("`[ref]: literal`", self.notes())
        self.assertIn(f"[root]({base}README.md)", self.notes())
        self.assertIn(f"[anchor]({base}{SEC_CHANGELOG}#migration)", self.notes())
        self.assertIn(
            f"![diagram](<{base.replace('/blob/', '/raw/')}src/agent-sec-core/docs/image%20name.png>)",
            self.notes(),
        )
        self.assertIn(
            f'[picture]: {base.replace("/blob/", "/raw/")}src/agent-sec-core/docs/diagram.png "Architecture"',
            self.notes(),
        )
        self.assertIn(f'[upgrade]: {base}src/guide.md "Migration guide"', self.notes())
        self.assertIn("`command [text](README.md)`", self.notes())
        self.assertIn("[external](https://example.com/path)", self.notes())
        self.assertIn("[email](mailto:maintainer@example.com)", self.notes())
        self.assertNotIn("Old content.", self.notes())

    def test_missing_changelog_fails_before_release_creation(self) -> None:
        self.git("tag", "sec-core/v1.2.3")
        result, calls = self.publish()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"Cannot read {SEC_CHANGELOG} at sec-core/v1.2.3", result.stderr)
        self.assertEqual(calls, [])
        self.assertFalse((self.root / "release-notes.md").exists())

    def test_missing_or_duplicate_version_fails_before_release_creation(self) -> None:
        for content, count in (
            ("## 1.2.30\n\n- Wrong version.\n", 0),
            ("## 1.2.3\n\n- One.\n\n## [1.2.3]\n\n- Two.\n", 2),
        ):
            with self.subTest(count=count):
                self.changelog(content)
                self.git("tag", "-f", "sec-core/v1.2.3")
                result, calls = self.publish()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(
                    f"Expected exactly one CHANGELOG section for 1.2.3; found {count}",
                    result.stderr,
                )
                self.assertEqual(calls, [])

    def test_empty_version_fails_before_release_creation(self) -> None:
        for body in (
            "",
            " \n",
            "<!-- Add release notes here -->",
            "### Fixed\n\n<!-- TODO -->\n",
            "---",
        ):
            with self.subTest(body=body):
                self.changelog(f"## 1.2.3\n\n{body}\n\n## 1.2.2\n\n- Previous explanation.\n")
                self.git("tag", "-f", "sec-core/v1.2.3")
                result, calls = self.publish()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Empty CHANGELOG section for 1.2.3", result.stderr)
                self.assertEqual(calls, [])

    def test_vendor_and_prebuilt_assets_are_preserved(self) -> None:
        self.release("skillfs/v0.5.2", path="src/skillfs/CHANGELOG.md")
        vendor = self.root / "archives/skillfs-0.5.2-vendor.tar.gz"
        vendor.parent.mkdir()
        vendor.touch()
        raw = self.root / "prebuilt/skillfs-aarch64/skillfs-0.5.2-linux-aarch64.tar.gz"
        raw.parent.mkdir(parents=True)
        raw.touch()
        result, calls = self.publish(
            component="skillfs", tag="skillfs/v0.5.2", PREBUILT_ENABLED="true"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        create = next(call for call in calls if call[:2] == ["release", "create"])
        self.assertIn(str(vendor), create)
        self.assertIn(str(raw), create)

    def test_first_ktuner_release_requires_a_versioned_changelog_entry(self) -> None:
        path = "src/ktuner/CHANGELOG.md"
        self.changelog("# Changelog\n\n## [Unreleased]\n\n- Tune the system.\n", path)
        self.git("tag", "ktuner/v0.1.0")
        result, calls = self.publish(component="ktuner", tag="ktuner/v0.1.0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Expected exactly one CHANGELOG section for 0.1.0; found 0", result.stderr)
        self.assertEqual(calls, [])
        self.changelog(
            "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - 2026-10-15\n\n- Tune the system.\n",
            path,
        )
        self.git("tag", "-f", "ktuner/v0.1.0")
        result, calls = self.publish(component="ktuner", tag="ktuner/v0.1.0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("- Tune the system.", self.notes())
        self.assertTrue(any(call[:2] == ["release", "create"] for call in calls))

    def test_generation_failure_stops_before_creating_release(self) -> None:
        self.git("tag", "sec-core/vinvalid")
        result, calls = self.publish(tag="sec-core/vinvalid")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Release tag is not SemVer", result.stderr)
        self.assertEqual(calls, [])

    def test_create_failure_stops_publish(self) -> None:
        self.release()
        result, calls = self.publish(CREATE_EXIT="42")
        self.assertEqual(result.returncode, 42)
        self.assertEqual(calls[-1][:2], ["release", "create"])
        self.assertFalse((self.root / "summary").exists())

    def test_existing_release_is_not_overwritten(self) -> None:
        self.release()
        result, calls = self.publish(VIEW_EXIT="0")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 1)
        self.assertIn("already exists", result.stdout)


if __name__ == "__main__":
    unittest.main()
