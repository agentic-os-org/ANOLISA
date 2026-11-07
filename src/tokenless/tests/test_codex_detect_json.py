#!/usr/bin/env python3
"""Regression tests for the JSON contract of codex detect.sh.

detect.sh promises JSON on stdout. Its jq branch gets string escaping from
`jq --arg`, but jq is an optional runtime, and the no-jq fallback used to
interpolate the selected binary path into the JSON string verbatim. A POSIX
path may legally contain double quotes, backslashes and control characters, so
that output either failed to parse or -- worse -- parsed to a *different*
path: a binary under a directory named ``a\\tb`` was reported as ``a<TAB>b``.

Every "fallback" case below runs the detector in a sandbox whose PATH holds
only a stub `tokenless` and symlinks to the three external commands the script
needs (grep, head, cat), so jq is unavailable and the fallback is what produces
the bytes under test. The jq-backed controls add jq to that same PATH.
"""

from __future__ import annotations

import json
import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

DETECT_SCRIPT = (
    Path(__file__).resolve().parent.parent
    / "adapters"
    / "tokenless"
    / "codex"
    / "scripts"
    / "detect.sh"
)

BASH = shutil.which("bash")

# The external commands detect.sh runs while detecting: the version pipeline
# (grep, head) and the fallback's `cat <<EOF`. jq is deliberately absent --
# that absence is what selects the branch under test.
SANDBOX_TOOLS = ("grep", "head", "cat")

# Hard-coded fallbacks detect.sh probes when PATH has no tokenless. A sandbox
# cannot shadow these, so the "not installed" control skips on a machine that
# really has the CLI installed system-wide.
SYSTEM_FALLBACKS = ("/usr/local/bin/tokenless", "/usr/bin/tokenless")

# Directory names for the stub binary's parent directory: path shapes a POSIX
# filesystem allows and a JSON string must escape for.
PATH_CASES = (
    ("ordinary path", "ordinary"),
    ("double quote", 'with"quote'),
    ("backslash", "with\\backslash"),
    # Two characters, backslash and 't' -- not a tab. Interpolated verbatim
    # into JSON this decodes to a tab, i.e. to a path that does not exist.
    ("literal backslash-t", "with\\tescape"),
    ("tab", "with\ttab"),
    ("escape character", "with\x1bescape"),
    ("delete character", "with\x7fdelete"),
    ("unicode", "\u4e2d\u6587-\u00e9-\U0001f389"),
)

VERSION_CASES = (
    ("release candidate", "tokenless-cli 0.9.0-rc.1 (build 7)", "0.9.0"),
    ("surrounding prose", "tokenless version 1.2.3 from rust", "1.2.3"),
    ("no digits at all", "tokenless", "unknown"),
)


def _make_stub(directory: Path, version_line: str = "tokenless 1.2.3") -> Path:
    """Create an executable `tokenless` stub under *directory*."""
    directory.mkdir(parents=True, exist_ok=True)
    stub = directory / "tokenless"
    stub.write_text(f"#!/bin/sh\necho '{version_line}'\n", encoding="utf-8")
    stub.chmod(stub.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return stub


def _link_tools(directory: Path, names: tuple) -> Path:
    """Populate *directory* with symlinks to the ambient *names* binaries."""
    directory.mkdir(parents=True, exist_ok=True)
    for name in names:
        source = shutil.which(name)
        if source is None:
            raise unittest.SkipTest(f"{name} is required to run detect.sh")
        link = directory / name
        if not link.exists():
            link.symlink_to(source)
    return directory


def _run_detect(path_entries, home: Path, extra_env=None):
    """Run detect.sh with a PATH built only from *path_entries*."""
    env = {
        "PATH": os.pathsep.join(str(entry) for entry in path_entries),
        "HOME": str(home),
    }
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [BASH, str(DETECT_SCRIPT)],
        capture_output=True,
        encoding="utf-8",
        errors="surrogateescape",
        env=env,
        timeout=30,
    )


def _first_utf8_locale():
    """Return a UTF-8 locale this machine actually has, else None."""
    locale_bin = shutil.which("locale")
    if locale_bin is None:
        return None
    try:
        listed = subprocess.run(
            [locale_bin, "-a"],
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    available = listed.stdout.split()
    for preferred in ("C.UTF-8", "C.utf8", "en_US.UTF-8", "en_US.utf8"):
        if preferred in available:
            return preferred
    return next(
        (name for name in available if "utf" in name.lower().replace("-", "")),
        None,
    )


def _system_tokenless_present() -> bool:
    return any(
        Path(path).is_file() and os.access(path, os.X_OK) for path in SYSTEM_FALLBACKS
    )


@unittest.skipIf(os.name != "posix", "detect.sh is a POSIX shell script")
@unittest.skipIf(BASH is None, "bash is required to run detect.sh")
@unittest.skipUnless(DETECT_SCRIPT.is_file(), f"missing {DETECT_SCRIPT}")
class CodexDetectJsonTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        self.tools = _link_tools(self.root / "tools", SANDBOX_TOOLS)
        self.home = self.root / "home"
        self.home.mkdir()

    def _decode(self, stdout: str) -> dict:
        try:
            return json.loads(stdout)
        except ValueError as exc:
            self.fail(f"stdout is not valid JSON ({exc}): {stdout!r}")

    def _assert_detected(self, result, stub: Path, version: str = "1.2.3") -> dict:
        self.assertEqual(result.returncode, 0, result.stderr)
        data = self._decode(result.stdout)
        self.assertIs(data["installed"], True)
        self.assertEqual(data["version"], version)
        # The whole point of the contract: the decoded path is byte-for-byte
        # the binary the detector selected.
        self.assertEqual(data["path"], str(stub))
        self.assertEqual(set(data), {"installed", "version", "path"})
        return data

    def test_sandbox_really_has_no_jq(self) -> None:
        """Guard the premise of every fallback case below."""
        result = subprocess.run(
            [BASH, "-c", "command -v jq"],
            capture_output=True,
            text=True,
            env={"PATH": str(self.tools), "HOME": str(self.home)},
            timeout=10,
        )
        self.assertNotEqual(result.returncode, 0, "jq leaked into the sandbox PATH")

    def test_fallback_roundtrips_every_path(self) -> None:
        """Without jq, each path survives a JSON encode/decode unchanged."""
        for label, dirname in PATH_CASES:
            with self.subTest(case=label):
                stub = _make_stub(self.root / "bin-fallback" / dirname)
                result = _run_detect([stub.parent, self.tools], self.home)
                self._assert_detected(result, stub)

    def test_jq_branch_roundtrips_every_path(self) -> None:
        """The jq-backed path keeps the behavior it already had."""
        if shutil.which("jq") is None:
            self.skipTest("jq is not installed")
        tools = _link_tools(self.root / "tools-jq", SANDBOX_TOOLS + ("jq",))
        for label, dirname in PATH_CASES:
            with self.subTest(case=label):
                stub = _make_stub(self.root / "bin-jq" / dirname)
                result = _run_detect([stub.parent, tools], self.home)
                self._assert_detected(result, stub)

    def test_fallback_and_jq_agree(self) -> None:
        """Both branches describe the same installation, with or without jq."""
        if shutil.which("jq") is None:
            self.skipTest("jq is not installed")
        tools = _link_tools(self.root / "tools-parity", SANDBOX_TOOLS + ("jq",))
        for label, dirname in PATH_CASES:
            with self.subTest(case=label):
                stub = _make_stub(self.root / "bin-parity" / dirname)
                plain = _run_detect([stub.parent, self.tools], self.home)
                with_jq = _run_detect([stub.parent, tools], self.home)
                self.assertEqual(
                    self._decode(plain.stdout),
                    self._decode(with_jq.stdout),
                )

    def test_fallback_version_extraction_is_unchanged(self) -> None:
        """Escaping the value must not change which value is extracted."""
        for index, (label, version_line, expected) in enumerate(VERSION_CASES):
            with self.subTest(case=label):
                stub = _make_stub(
                    self.root / "bin-version" / f"case{index}",
                    version_line,
                )
                result = _run_detect([stub.parent, self.tools], self.home)
                self._assert_detected(result, stub, version=expected)

    def test_fallback_unicode_path_under_utf8_locale(self) -> None:
        """A UTF-8 locale must not change how multi-byte paths round-trip."""
        locale_name = _first_utf8_locale()
        if locale_name is None:
            self.skipTest("no UTF-8 locale is available")
        stub = _make_stub(self.root / "bin-locale" / PATH_CASES[-1][1])
        result = _run_detect(
            [stub.parent, self.tools],
            self.home,
            extra_env={"LC_ALL": locale_name, "LANG": locale_name},
        )
        self._assert_detected(result, stub)

    @unittest.skipIf(
        _system_tokenless_present(),
        "tokenless is installed in one of detect.sh's system fallback paths",
    )
    def test_missing_binary_reports_not_installed(self) -> None:
        """The negative answer stays valid JSON."""
        result = _run_detect([self.tools], self.home)
        self.assertEqual(result.returncode, 0, result.stderr)
        data = self._decode(result.stdout)
        self.assertIs(data["installed"], False)


if __name__ == "__main__":
    unittest.main(verbosity=2)
