#!/usr/bin/env python3
"""Exercise Windows action npm packaging for current and tagged source layouts."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from test_verify_wheels import binary_payload


ACTION = Path(__file__).with_name("windows.sh")
REPOSITORY = Path(__file__).resolve().parents[3]
IMAGE_ID = "sha256:7649bcb55d8c5632232c7f27e3c2191b71fb7c31149de87412c447fc7a997303"


class WindowsEntrypointTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.output = self.root / "artifacts"
        self.wheels = self.root / "wheels"
        self.tools = self.root / "tools"
        for path in (self.source, self.output, self.wheels, self.tools):
            path.mkdir()
        self.marker = self.root / "packer.log"
        self.calls = self.root / "python.log"
        binary = self.root / "windows.exe"
        binary.write_bytes(binary_payload("windows", "x86_64"))
        docker = self.tools / "docker"
        docker.write_text(
            "#!/bin/bash\nset -eu\n"
            f"if [[ $1 = image ]]; then echo {IMAGE_ID}; exit 0; fi\n"
            'if [[ " $* " == *" /action/windows-container.sh build "* ]]; then\n'
            '  d="$TEST_OUTPUT.build/out/build/x86_64-pc-windows-msvc/release"\n'
            '  mkdir -p "$d"; cp "$TEST_BINARY" "$d/tokenless.exe"; cp "$TEST_BINARY" "$d/rtk.exe"\n'
            "fi\n"
        )
        python = self.tools / "python3"
        python.write_text(
            "#!/bin/bash\nset -eu\n"
            'printf "%s\\n" "$1" >> "$TEST_PYTHON_LOG"\n'
            'if [[ "$1" == */wheel-version.py ]]; then echo 1.2.3; exit 0; fi\n'
            'if [[ "$1" == */package-windows.py ]]; then\n'
            '  test -f "$2/npm/dist/tokenless/anolisa-tokenless-$4.tgz"\n'
            '  test -f "$2/npm/dist/tokenless-win32-x64/anolisa-tokenless-win32-x64-$4.tgz"\n'
            '  touch "$3/tokenless-$4-windows-x86_64.zip"\n'
            "fi\n"
        )
        make = self.tools / "make"
        make.write_text("#!/bin/bash\nexit 0\n")
        npm = self.tools / "npm"
        npm.write_text(
            "#!/bin/bash\nset -eu\n"
            'if [[ "$PWD" == */tokenless-win32-x64 ]]; then\n'
            '  touch anolisa-tokenless-win32-x64-1.2.3.tgz\n'
            'else touch anolisa-tokenless-1.2.3.tgz; fi\n'
        )
        for path in (docker, python, make, npm):
            path.chmod(0o755)
        self.environment = {
            **os.environ,
            "PATH": f"{self.tools}{os.pathsep}{os.environ['PATH']}",
            "SOURCE_DATE_EPOCH": "1700000000",
            "TOKENLESS_WINDOWS_CACHE": str(self.root / "cache"),
            "TEST_OUTPUT": str(self.output),
            "TEST_SOURCE": str(self.source),
            "TEST_BINARY": str(binary),
            "TEST_PYTHON_LOG": str(self.calls),
            "PACKER_MARKER": str(self.marker),
        }

    def packer(self, relative, label):
        path = self.source / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            "const fs = require('node:fs');\n"
            "if (process.argv.slice(2).join(' ') !== '--target win32-x64') process.exit(9);\n"
            f"fs.appendFileSync(process.env.PACKER_MARKER, '{label}\\n');\n"
            "for (const name of ['tokenless', 'tokenless-win32-x64']) {\n"
            "  const dir = `${process.env.TEST_SOURCE}/npm/dist/${name}`;\n"
            "  fs.mkdirSync(dir, { recursive: true });\n"
            "  fs.writeFileSync(`${dir}/anolisa-${name}-1.2.3.tgz`, 'fixture');\n"
            "}\n"
        )

    def run_action(self):
        return subprocess.run(
            ["bash", str(ACTION), str(self.source), str(self.output), str(self.wheels), "1.2.3"],
            env=self.environment,
            capture_output=True,
            text=True,
        )

    def test_current_source_packer_takes_precedence(self):
        current = self.source / "packaging/npm"
        current.mkdir(parents=True)
        production = REPOSITORY / "src/tokenless/packaging/npm"
        for name in ("package-npm.js", "package.json"):
            shutil.copyfile(production / name, current / name)
        (self.source / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n')
        for relative in (
            "npm/scripts/postinstall.js",
            "adapters/tokenless/openclaw/dist/index.js",
            "adapters/tokenless/dsh/dist/index.js",
            "adapters/tokenless/dsh/package.json",
            "adapters/tokenless/dsh/cordis.patch.yml",
        ):
            path = self.source / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fixture\n")
        self.packer("npm/scripts/package-npm.js", "legacy")
        result = self.run_action()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.marker.exists())
        self.assertTrue((self.source / "npm/dist/tokenless-win32-x64/package.json").is_file())
        self.assertIn("package-windows.py", self.calls.read_text())
        self.assertIn("verify-wheels.py", self.calls.read_text())

    def test_historical_source_uses_original_packer(self):
        self.packer("npm/scripts/package-npm.js", "legacy")
        result = self.run_action()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.marker.read_text(), "legacy\n")
        self.assertIn("package-windows.py", self.calls.read_text())

    def test_missing_packer_stops_before_zip_assembly(self):
        result = self.run_action()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("package-npm.js", result.stderr)
        self.assertFalse(self.calls.exists())


if __name__ == "__main__":
    if not shutil.which("node"):
        raise SystemExit("node is required to test the Windows packaging entrypoint")
    unittest.main()
