# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Wheel-version contract of the QwenPaw requirements pin, both sides.

The Cargo workspace version keeps its original spelling (0.9.0-rc.1) while
wheel filenames carry the PEP 440 normalized form (0.9.0rc1) that the
pinned maturin 1.15.0 produces, and pip rejects filenames that keep the
unnormalized spelling. The gate must accept the normalized filename, reject
the unnormalized one, and still reject real version drift.

The producer side has to emit exactly that pair: `make stamp-adapter-templates`
stamps the release-tag path with the raw workspace version and the wheel
filenames with the spelling packaging/raw/wheel-version.py normalizes through
this gate's own `_normalize_pep440`. The pipeline tests below run the real
Makefile rule over the real templates and then the real verifier, so a drift
between the two sides fails here instead of in a prerelease publish.

Runs under a bare `python3` (the Makefile's test-integration target and the
CI job behind it call the file directly), so it is stdlib unittest -- which
pytest collects as well. The pipeline tests additionally need `make`, which
every environment that stamps templates has; they skip without it.
"""

from __future__ import annotations

import importlib.util
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_COMPONENT = Path(__file__).resolve().parents[1]
_SCRIPT = _COMPONENT / "packaging" / "raw" / "verify-release.py"
_spec = importlib.util.spec_from_file_location("verify_release", _SCRIPT)
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)

_referenced_wheel_versions = _module._referenced_wheel_versions
_normalize_pep440 = _module._normalize_pep440
_verify_requirements_references = _module._verify_requirements_references


def _requirements_text(path_version: str, wheel_version: str) -> str:
    """Build a requirements.txt body shaped like the qwenpaw template."""
    return (
        "# Installed by QwenPaw's plugin loader into its Python environment.\n"
        "anolisa-tokenless @ https://github.com/alibaba/anolisa/releases/download/"
        f"tokenless/v{path_version}/anolisa_tokenless-{wheel_version}-cp311-abi3-"
        "manylinux_2_17_x86_64.manylinux2014_x86_64.whl ; "
        'sys_platform == "linux" and platform_machine == "x86_64"\n'
        "anolisa-tokenless @ https://github.com/alibaba/anolisa/releases/download/"
        f"tokenless/v{path_version}/anolisa_tokenless-{wheel_version}-cp311-abi3-"
        "manylinux_2_17_aarch64.manylinux2014_aarch64.whl ; "
        'sys_platform == "linux" and platform_machine == "aarch64"\n'
    )


class TestReferencedWheelVersions(unittest.TestCase):
    def test_prerelease_version_captured_whole(self):
        """A pre-release version must not be truncated at the first dash."""
        text = (
            "anolisa_tokenless-0.9.0-rc.1-cp311-cp311-"
            "manylinux_2_28_x86_64.whl\n"
        )
        self.assertEqual(_referenced_wheel_versions(text), {"0.9.0-rc.1"})

    def test_normalized_prerelease_version(self):
        """The normalized filename maturin produces is captured as-is."""
        text = "anolisa_tokenless-0.9.0rc1-cp311-abi3-win_amd64.whl\n"
        self.assertEqual(_referenced_wheel_versions(text), {"0.9.0rc1"})

    def test_plain_version(self):
        text = "anolisa_tokenless-0.9.0-cp311-cp311-win_amd64.whl\n"
        self.assertEqual(_referenced_wheel_versions(text), {"0.9.0"})

    def test_multiple_wheels(self):
        text = (
            "anolisa_tokenless-0.9.0-rc.1-cp311-cp311-manylinux_2_28_x86_64.whl\n"
            "anolisa_tokenless-0.9.0-rc.1-cp312-cp312-manylinux_2_28_x86_64.whl\n"
        )
        self.assertEqual(_referenced_wheel_versions(text), {"0.9.0-rc.1"})

    def test_no_wheel_reference(self):
        self.assertEqual(_referenced_wheel_versions("some other content\n"), set())


class TestNormalizePep440(unittest.TestCase):
    def test_prerelease_separators_fold(self):
        """-, _ and . around a pre-release segment all normalize away."""
        self.assertEqual(_normalize_pep440("0.9.0-rc.1"), "0.9.0rc1")
        self.assertEqual(_normalize_pep440("0.9.0_rc.2"), "0.9.0rc2")
        self.assertEqual(_normalize_pep440("1.0.0-alpha.3"), "1.0.0a3")

    def test_already_normalized_spelling_round_trips(self):
        for version in ("0.9.0rc1", "9.8.7", "0.8.4", "1.0.0rc1.dev2"):
            self.assertEqual(_normalize_pep440(version), version)

    def test_case_and_spelling_aliases_fold(self):
        self.assertEqual(_normalize_pep440("1.0.0-RC1"), "1.0.0rc1")
        self.assertEqual(_normalize_pep440("1.0.0Preview3"), "1.0.0rc3")
        self.assertEqual(_normalize_pep440("1.0.0B2"), "1.0.0b2")

    def test_post_and_dev_segments(self):
        self.assertEqual(_normalize_pep440("1.0.0-post2"), "1.0.0.post2")
        self.assertEqual(_normalize_pep440("1.0.0.rev3"), "1.0.0.post3")
        self.assertEqual(_normalize_pep440("1.0.0-dev.3"), "1.0.0.dev3")
        self.assertEqual(_normalize_pep440("1.0.0-rc.1-dev.2"), "1.0.0rc1.dev2")

    def test_local_version_separators(self):
        self.assertEqual(_normalize_pep440("1.0.0+Ubuntu_1"), "1.0.0+ubuntu.1")

    def test_unparseable_version_returned_unchanged(self):
        """Exotic Cargo tags fall back to exact-string comparison."""
        self.assertEqual(_normalize_pep440("0.9.0-rc.1-meta"), "0.9.0-rc.1-meta")


class TestRequirementsReferenceContract(unittest.TestCase):
    def assert_accepts(self, expected: str, text: str) -> None:
        _verify_requirements_references(
            "adapters/tokenless/qwenpaw/requirements.txt", expected, text
        )

    def assert_rejects(self, expected: str, text: str, needle: str) -> None:
        with self.assertRaises(SystemExit) as raised:
            _verify_requirements_references(
                "adapters/tokenless/qwenpaw/requirements.txt", expected, text
            )
        self.assertIn(needle, str(raised.exception))

    def test_prerelease_release_with_normalized_wheel_filename_accepted(self):
        """Contract (a): Cargo 0.9.0-rc.1 plus wheel filename 0.9.0rc1."""
        self.assert_accepts(
            "0.9.0-rc.1", _requirements_text("0.9.0-rc.1", "0.9.0rc1")
        )

    def test_unnormalized_wheel_filename_rejected(self):
        """Contract (b): a wheel filename keeping 0.9.0-rc.1 is pip-rejected."""
        self.assert_rejects(
            "0.9.0-rc.1",
            _requirements_text("0.9.0-rc.1", "0.9.0-rc.1"),
            "pip rejects",
        )

    def test_version_drift_rejected(self):
        """Contract (c): a wheel built from a different version is drift."""
        self.assert_rejects(
            "0.9.0-rc.1",
            _requirements_text("0.9.0-rc.1", "0.9.1"),
            "does not reference tokenless 0.9.0-rc.1",
        )

    def test_plain_release_unchanged(self):
        """A plain release keeps the exact single-spelling contract."""
        self.assert_accepts("9.8.7", _requirements_text("9.8.7", "9.8.7"))

    def test_unstamped_template_rejected(self):
        self.assert_rejects(
            "0.9.0-rc.1", "anolisa-tokenless @ .../v@VERSION@/...\n", "@VERSION@"
        )

    def test_normalized_release_tag_rejected(self):
        """The normalized Python spelling must not leak into the tag path.

        The release workflow tags the workspace version verbatim, so a
        requirements URL with the normalized spelling would point
        installers at a tag the workflow never creates.
        """
        self.assert_rejects(
            "0.9.0-rc.1",
            _requirements_text("0.9.0rc1", "0.9.0rc1"),
            "release tag version(s) ['0.9.0rc1']",
        )

    def test_stale_release_path_rejected(self):
        """A fresh wheel filename cannot mask a stale release tag path."""
        self.assert_rejects(
            "0.9.0-rc.1",
            _requirements_text("0.9.0", "0.9.0rc1"),
            "pins release tag version(s) ['0.9.0']",
        )

    def test_no_wheel_url_rejected(self):
        self.assert_rejects("0.9.0-rc.1", "some other content\n", "no wheel URL")


class TestWheelVersionHelper(unittest.TestCase):
    """packaging/raw/wheel-version.py is what the Makefile rule asks."""

    def helper(self, *argv: str) -> subprocess.CompletedProcess:
        script = _COMPONENT / "packaging" / "raw" / "wheel-version.py"
        return subprocess.run(
            [sys.executable, str(script), *argv],
            capture_output=True,
            text=True,
        )

    def test_prerelease_versions_normalize(self):
        for cargo, wheel in (
            ("0.9.0-rc.1", "0.9.0rc1"),
            ("1.0.0-alpha.3", "1.0.0a3"),
            ("1.0.0-beta.2", "1.0.0b2"),
            ("1.0.0-dev.3", "1.0.0.dev3"),
        ):
            with self.subTest(cargo=cargo):
                result = self.helper(cargo)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), wheel)

    def test_plain_release_spelling_is_unchanged(self):
        """An ordinary release keeps one spelling in both URL positions."""
        result = self.helper("9.8.7")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "9.8.7")

    def test_missing_argument_is_a_usage_error(self):
        """The stamping rule must never fall back to an empty filename."""
        result = self.helper()
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage:", result.stderr)


def _stamp_templates(version: str) -> tuple:
    """Copy the production stamping inputs to a temp root and run make over them.

    The copy is the real Makefile, every real `*.in` template and the real
    packaging/raw scripts; only Cargo.toml is synthetic, because that is where
    the workspace version under test comes from. Returns the temp root and the
    completed make run, whose status the caller asserts: this suite already
    runs under python3, so a stamping failure is the defect under test and not
    a missing-dependency condition to skip over.
    """
    root = Path(tempfile.mkdtemp(prefix="tokenless-wheel-version-test."))
    shutil.copy2(_COMPONENT / "Makefile", root / "Makefile")
    (root / "packaging" / "raw").mkdir(parents=True)
    for script in (_COMPONENT / "packaging" / "raw").iterdir():
        if script.is_file():
            shutil.copy2(script, root / "packaging" / "raw" / script.name)
    for template in _COMPONENT.rglob("*.in"):
        relative = template.relative_to(_COMPONENT)
        if relative.parts[0] == "target":
            continue
        target = root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(template, target)
    (root / "Cargo.toml").write_text(
        "[workspace]\nmembers = []\n\n[workspace.package]\n"
        f'version = "{version}"\n',
        encoding="utf-8",
    )
    result = subprocess.run(
        ["make", "-B", "stamp-adapter-templates"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    return root, result


def _verify_stamped_tree(root: Path) -> subprocess.CompletedProcess:
    """Run the production raw-packaging gate over a stamped tree."""
    return subprocess.run(
        [
            sys.executable,
            "packaging/raw/verify-release.py",
            ".",
            ".anolisa/component.toml",
        ],
        cwd=root,
        capture_output=True,
        text=True,
    )


@unittest.skipUnless(shutil.which("make"), "the stamping pipeline needs make")
class TestStampedRequirementsPipeline(unittest.TestCase):
    """Producer and gate agree on both spellings of one version.

    Before the producer side was fixed, stamping a Cargo 0.9.0-rc.1 wrote the
    raw spelling into the wheel filenames too and the gate rejected its own
    component's output ("pins wheel filename version(s) ['0.9.0-rc.1'] in a
    form pip rejects"), so no prerelease raw package could be assembled.
    """

    def assert_stamps_and_verifies(self, version: str, wheel: str) -> str:
        root, stamped = _stamp_templates(version)
        self.addCleanup(shutil.rmtree, root, True)
        self.assertEqual(
            stamped.returncode,
            0,
            f"make -B stamp-adapter-templates failed for {version}: "
            f"{stamped.stdout[-400:]} {stamped.stderr[-400:]}",
        )
        requirements = (
            root / "adapters" / "tokenless" / "qwenpaw" / "requirements.txt"
        )
        text = requirements.read_text(encoding="utf-8")
        self.assertIn(f"/tokenless/v{version}/", text)
        self.assertIn(f"anolisa_tokenless-{wheel}-cp311-abi3-", text)
        # A surviving placeholder means the rule stamped only half the pair.
        self.assertNotIn("@VERSION@", text)
        self.assertNotIn("@WHEEL_VERSION@", text)
        result = _verify_stamped_tree(root)
        self.assertEqual(
            result.returncode,
            0,
            f"verify-release.py rejected the stamped tree: {result.stderr}",
        )
        self.assertEqual(result.stdout.strip(), version)
        return text

    def test_prerelease_stamps_both_spellings(self):
        """A Cargo 0.9.0-rc.1 keeps its tag and normalizes its filenames."""
        text = self.assert_stamps_and_verifies("0.9.0-rc.1", "0.9.0rc1")
        self.assertEqual(text.count("/tokenless/v0.9.0-rc.1/"), 4)
        self.assertEqual(text.count("anolisa_tokenless-0.9.0rc1-cp311-abi3-"), 4)
        self.assertIn("anolisa_tokenless-0.9.0rc1-cp311-abi3-macosx_11_0_x86_64.whl", text)

    def test_plain_release_urls_are_unchanged(self):
        """An ordinary release stamps one spelling into both positions."""
        text = self.assert_stamps_and_verifies("9.8.7", "9.8.7")
        self.assertEqual(text.count("/tokenless/v9.8.7/"), 4)
        self.assertEqual(text.count("anolisa_tokenless-9.8.7-cp311-abi3-"), 4)


if __name__ == "__main__":
    unittest.main()
