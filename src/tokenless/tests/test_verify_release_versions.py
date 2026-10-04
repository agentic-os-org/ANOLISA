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

"""Wheel-version contract of verify-release: compare PEP 440 spellings.

The Cargo workspace version keeps its original spelling (0.9.0-rc.1) while
wheel filenames carry the PEP 440 normalized form (0.9.0rc1) that the
pinned maturin 1.15.0 produces, and pip rejects filenames that keep the
unnormalized spelling. The gate must accept the normalized filename, reject
the unnormalized one, and still reject real version drift.

Runs under a bare `python3` (the Makefile's test-integration target and the
CI job behind it call the file directly), so it is stdlib unittest -- which
pytest collects as well.
"""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


_SCRIPT = (
    Path(__file__).resolve().parents[1] / "packaging" / "raw" / "verify-release.py"
)
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


if __name__ == "__main__":
    unittest.main()
