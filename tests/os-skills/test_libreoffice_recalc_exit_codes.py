#!/usr/bin/env python3
"""Regression tests for libreoffice_recalc.py exit-code classification.

The documented contract (references/validate.md) is:
    0 — recalculation succeeded
    2 — LibreOffice not found (Tier 2 unavailable, not a hard failure)
    1 — LibreOffice found but the recalculation failed

main() used to classify failures by sniffing the message for
"not found" / "not available", so a real LibreOffice failure whose own
stderr mentions "filter not found" was reported as exit 2 and silently
downgraded to a skip. These tests pin the classification to the
availability probe with a stub soffice binary — no LibreOffice needed.
"""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "others"
    / "xlsx"
    / "scripts"
    / "libreoffice_recalc.py"
)

# A soffice stub that fails the conversion while quoting "not found" in
# its stderr — exactly the LibreOffice wording (missing / broken import
# filter) the old message sniff misclassified.
FAILING_SOFFICE = """\
#!/usr/bin/env python3
import sys

args = sys.argv[1:]
if "--version" in args:
    print("LibreOffice 24.8.4.2 (stub)")
    sys.exit(0)

sys.stderr.write("Error: source file could not be loaded: filter not found\\n")
sys.exit(1)
"""

# A soffice stub that emulates `--convert-to xlsx --outdir DIR INPUT`:
# writes a "recalculated" workbook next to the input under the same stem.
CONVERTING_SOFFICE = """\
#!/usr/bin/env python3
import os
import sys

args = sys.argv[1:]
if "--version" in args:
    print("LibreOffice 24.8.4.2 (stub)")
    sys.exit(0)

outdir = args[args.index("--outdir") + 1]
src = args[-1]
stem = os.path.splitext(os.path.basename(src))[0]
target = os.path.join(outdir, stem + ".xlsx")
with open(target, "wb") as fh:
    fh.write(b"PK\\x05\\x06recalculated-stub")
sys.exit(0)
"""


def load_module():
    spec = importlib.util.spec_from_file_location("libreoffice_recalc", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def write_stub(directory, body):
    path = directory / "soffice"
    path.write_text(body, encoding="utf-8")
    path.chmod(0o755)
    return str(path)


class ExitCodeClassification(unittest.TestCase):
    def setUp(self):
        self.module = load_module()
        self._argv = sys.argv
        self._find_soffice = self.module.find_soffice

    def tearDown(self):
        sys.argv = self._argv
        self.module.find_soffice = self._find_soffice

    def run_main(self, argv):
        sys.argv = ["libreoffice_recalc.py", *argv]
        try:
            self.module.main()
        except SystemExit as exc:
            return exc.code
        raise AssertionError("main() returned without SystemExit")

    def test_failure_quoting_not_found_exits_1(self):
        """A real recalculation failure must exit 1, not be downgraded to 2.

        LibreOffice's own error text ("filter not found") used to match the
        "not installed" sniff, so a hard failure was reported as Tier 2
        unavailable and callers skipped reporting it.
        """
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            stub = write_stub(tmp_path, FAILING_SOFFICE)
            self.module.find_soffice = lambda: stub
            src = tmp_path / "in.xlsx"
            src.write_bytes(b"PK\x05\x06" + b"\x00" * 18)
            code = self.run_main([str(src), str(tmp_path / "out.xlsx")])
            self.assertEqual(code, 1)

    def test_succeeded_recalc_exits_0(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            stub = write_stub(tmp_path, CONVERTING_SOFFICE)
            self.module.find_soffice = lambda: stub
            src = tmp_path / "in.xlsx"
            src.write_bytes(b"PK\x05\x06" + b"\x00" * 18)
            out = tmp_path / "out.xlsx"
            code = self.run_main([str(src), str(out)])
            self.assertEqual(code, 0)
            self.assertTrue(out.is_file())

    def test_missing_libreoffice_exits_2(self):
        self.module.find_soffice = lambda: None
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            src = tmp_path / "in.xlsx"
            src.write_bytes(b"PK\x05\x06" + b"\x00" * 18)
            code = self.run_main([str(src), str(tmp_path / "out.xlsx")])
            self.assertEqual(code, 2)


class RealCliContract(unittest.TestCase):
    """The real CLI keeps the documented --check exit codes."""

    def test_check_mode_exit_code_is_0_or_2(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--check"],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertIn(result.returncode, (0, 2))


if __name__ == "__main__":
    unittest.main()
