#!/usr/bin/env python3
"""Regression tests for the kernel-dev setup gcc probe (#3638).

``setup.sh`` used to read ``GCC_VERSION=$(gcc --version | head -1)`` with no
presence guard. With gcc missing from PATH the pipeline status is ``head``'s
0, so ``set -e`` never fires, the script prints an empty "Compiler:" line and
then "Setup Complete!" and exits 0 -- certifying a kernel build environment
that cannot compile a module.

These tests drive a path-redirected copy of the real script (fake
``/etc/os-release``, fake ``/lib/modules/$KERNEL_VER/build``, stubbed
``uname``/``yum``/``sudo``) under a mock PATH that contains only the tools the
script needs, so the presence or absence of gcc is fully controlled. No
network, no root, no package installation.

Baseline on unchanged main: 1 failure (missing gcc exits 0 and still reports
"Setup Complete!") / 1 passing control (gcc present).
"""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SETUP_SH = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "devops"
    / "kernel-dev"
    / "scripts"
    / "setup.sh"
)

KERNEL_VER = "6.6.102-5.3.1.alnx4.x86_64"
GCC_VERSION_LINE = "gcc (GCC) 12.3.1 20230508 (Anolis 12.3.1-1.alnx4)"

# External commands the script calls besides the ones stubbed below.
REAL_TOOLS = ("grep", "cut", "head", "readlink")

UNAME_STUB = """#!/bin/sh
case "$1" in
    -m) echo x86_64 ;;
    -r) echo %s ;;
    *)  echo unknown ;;
esac
""" % KERNEL_VER

YUM_STUB = """#!/bin/sh
exit 0
"""

SUDO_STUB = """#!/bin/sh
exec "$@"
"""

GCC_STUB = """#!/bin/sh
echo '%s'
echo 'Copyright (C) 2022 Free Software Foundation, Inc.'
""" % GCC_VERSION_LINE


def build_env(root: Path, with_gcc: bool) -> Path:
    """Create the redirected filesystem plus a hermetic mock PATH."""
    bindir = root / "bin"
    bindir.mkdir()

    for name, body in (
        ("uname", UNAME_STUB),
        ("yum", YUM_STUB),
        ("sudo", SUDO_STUB),
    ):
        stub = bindir / name
        stub.write_text(body)
        stub.chmod(0o755)

    if with_gcc:
        gcc = bindir / "gcc"
        gcc.write_text(GCC_STUB)
        gcc.chmod(0o755)

    for tool in REAL_TOOLS:
        real = shutil.which(tool)
        if real:
            os.symlink(real, bindir / tool)

    etc = root / "etc"
    etc.mkdir()
    (etc / "os-release").write_text(
        'NAME="Alibaba Cloud Linux"\n'
        'ID="alinux"\n'
        f'PRETTY_NAME="Alibaba Cloud Linux (alinux) 4.0"\n'
        'VERSION_ID="4.0"\n'
    )

    build_dir = root / "lib" / "modules" / KERNEL_VER / "build"
    build_dir.mkdir(parents=True)
    (build_dir / "Makefile").write_text("# stub kernel build tree\n")

    return bindir


def redirected_script(root: Path) -> Path:
    """Copy setup.sh with its absolute paths pointed into the fake root."""
    source = SETUP_SH.read_text()
    source = source.replace("/etc/os-release", str(root / "etc" / "os-release"))
    source = source.replace("/lib/modules/", str(root / "lib" / "modules") + os.sep)
    target = root / "setup-under-test.sh"
    target.write_text(source)
    target.chmod(0o755)
    return target


def run_setup(root: Path, with_gcc: bool, bash: str):
    """Run the redirected setup.sh with PATH limited to the mock bin dir."""
    bindir = build_env(root, with_gcc)
    script = redirected_script(root)
    env = {
        "PATH": str(bindir),
        "HOME": str(root),
        "LC_ALL": "C",
        "TERM": "dumb",
    }
    return subprocess.run(
        [bash, str(script)],
        cwd=str(root),
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=60,
    )


class TestSetupGccGuard(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not SETUP_SH.is_file():
            raise unittest.SkipTest(f"missing script: {SETUP_SH}")
        missing = [t for t in REAL_TOOLS if not shutil.which(t)]
        if missing:
            raise unittest.SkipTest(f"missing host tools: {', '.join(missing)}")
        cls.bash = shutil.which("bash")
        if not cls.bash:
            raise unittest.SkipTest("bash not available")

    def test_missing_gcc_fails_setup(self):
        """No gcc on PATH must abort setup instead of reporting success."""
        with tempfile.TemporaryDirectory() as tmp:
            proc = run_setup(Path(tmp), with_gcc=False, bash=self.bash)

        self.assertNotEqual(
            proc.returncode,
            0,
            "setup.sh exited 0 with no gcc on PATH; output:\n" + proc.stdout,
        )
        self.assertNotIn(
            "Setup Complete",
            proc.stdout,
            "setup.sh declared success over a missing compiler",
        )
        self.assertIn("gcc not found", proc.stdout + proc.stderr)

    def test_present_gcc_completes_setup(self):
        """With gcc on PATH the compiler line and the summary still appear."""
        with tempfile.TemporaryDirectory() as tmp:
            proc = run_setup(Path(tmp), with_gcc=True, bash=self.bash)

        self.assertEqual(
            proc.returncode,
            0,
            "setup.sh failed with gcc available; output:\n"
            + proc.stdout
            + proc.stderr,
        )
        self.assertIn(GCC_VERSION_LINE, proc.stdout)
        self.assertIn("Setup Complete", proc.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
