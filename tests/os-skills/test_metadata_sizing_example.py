#!/usr/bin/env python3
"""Regression tests for the aliyun-ecs metadata sizing example (#3620).

Runs the fenced ``bash`` block under "### 2. 根据实例规格动态配置" in
``src/os-skills/aliyun/aliyun-ecs/references/metadata-api.md`` verbatim, with a
stub ``curl`` on a private ``PATH`` so the documented snippet is executed
exactly as published and no metadata service is contacted.

Baseline on unchanged main: the two xlarge cases fail. ``case`` takes the first
matching pattern and ``*large*`` is listed before ``*xlarge*``; every xlarge
instance type also contains the substring ``large`` (``ecs.g7.xlarge``,
``ecs.c6.2xlarge``), so the ``*xlarge*) WORKERS=8`` arm is dead code for all
real inputs and an operator following the example provisions 4 workers on
xlarge-sized machines.
"""

import os
import re
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DOC = REPO / "src" / "os-skills" / "aliyun" / "aliyun-ecs" / "references" / "metadata-api.md"
HEADING = "### 2. 根据实例规格动态配置"

CURL_STUB = """#!/bin/sh
printf '%s\\n' "${STUB_INSTANCE_TYPE:-}"
"""


def documented_snippet():
    """Return the bash block published under the sizing heading, verbatim."""
    text = DOC.read_text(encoding="utf-8")
    section = text.split(HEADING, 1)
    assert len(section) == 2, f"heading not found in {DOC}: {HEADING}"
    match = re.search(r"```bash\n(.*?)```", section[1], re.DOTALL)
    assert match, f"no fenced bash block after {HEADING!r}"
    return match.group(1)


class MetadataSizingExampleTestCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.snippet = documented_snippet()
        # Guard against a doc restructure silently turning this into a no-op.
        assert "case $INSTANCE_TYPE in" in cls.snippet, cls.snippet

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="metadata-sizing-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.bin_dir = self.tmp / "bin"
        self.bin_dir.mkdir()
        curl = self.bin_dir / "curl"
        curl.write_text(CURL_STUB, encoding="utf-8")
        curl.chmod(curl.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    def workers_for(self, instance_type):
        env = dict(os.environ)
        env["PATH"] = f"{self.bin_dir}{os.pathsep}{env.get('PATH', '')}"
        env["STUB_INSTANCE_TYPE"] = instance_type
        result = subprocess.run(
            ["bash", "-c", self.snippet + '\nprintf "%s\\n" "${WORKERS-unset}"'],
            cwd=self.tmp,
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()


class TestSizingExamplePicksTheSpecificArm(MetadataSizingExampleTestCase):
    def test_xlarge_instance_gets_eight_workers(self):
        self.assertEqual(self.workers_for("ecs.g7.xlarge"), "8")

    def test_multiplied_xlarge_instance_gets_eight_workers(self):
        self.assertEqual(self.workers_for("ecs.c6.2xlarge"), "8")

    def test_plain_large_instance_keeps_four_workers(self):
        self.assertEqual(self.workers_for("ecs.g7.large"), "4")

    def test_unrelated_instance_matches_no_arm(self):
        self.assertEqual(self.workers_for("ecs.t5-lc1m1.small"), "unset")

    def test_specific_arm_is_listed_before_the_broader_one(self):
        specific = self.snippet.index("*xlarge*)")
        broader = self.snippet.index("*large*)")
        self.assertLess(
            specific,
            broader,
            "case matches the first pattern, so *xlarge* must precede *large*",
        )


if __name__ == "__main__":
    unittest.main()
