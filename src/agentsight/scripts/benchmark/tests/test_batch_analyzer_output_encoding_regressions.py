"""The analyzer's own output must not depend on the host locale codec.

``batch-analyze.py`` runs the analyzer with ``universal_newlines=True`` and no
``encoding``, so both pipes are decoded with ``locale.getpreferredencoding``.
Under the C locale that is ASCII, while the analyzer prints JSON built with
``ensure_ascii=False``; a single non-ASCII character then raised
``UnicodeDecodeError`` outside the three existing ``except`` arms and aborted
the whole batch instead of counting one failed file.
"""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from subprocess_encoding_support import c_locale_env
from test_batch_analyze import batch_analyze

# The analyzer's stdout and stderr are decoded when the child is reaped, before
# any of the script's own error handling sees them. Write raw UTF-8 bytes so
# the fixture does not depend on its own console encoding either.
ANALYZER = (
    f"#!{sys.executable}\n"
    "import json,sys\n"
    "from pathlib import Path\n"
    "doc=json.loads(Path(sys.argv[1]).read_text(encoding='utf-8'))\n"
    "text=doc['steps'][0]['message']\n"
    "sys.stdout.buffer.write(json.dumps({'text':text},ensure_ascii=False).encode('utf-8'))\n"
    "sys.stderr.buffer.write(('checked ' + text).encode('utf-8'))\n"
)


class AnalyzerOutputEncodingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.analyzer = self.root / "analyzer.py"
        self.analyzer.write_text(ANALYZER, encoding="utf-8")
        self.analyzer.chmod(0o700)

    def run_cli(self, text: str) -> subprocess.CompletedProcess:
        session = self.root / "session.jsonl"
        session.write_text(
            json.dumps({"type": "user", "message": {"role": "user", "content": text}})
            + "\n",
            encoding="utf-8",
        )
        return subprocess.run(
            [
                sys.executable,
                batch_analyze.__file__,
                "--file",
                str(session),
                "--dim",
                "perf",
                "--min-steps",
                "1",
                "--binary",
                str(self.analyzer),
            ],
            env=c_locale_env(TMPDIR=str(self.root)),
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=30,
        )

    def assert_analyzed(self, text: str) -> None:
        result = self.run_cli(text)
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Done: 1 analyzed, 0 skipped, 0 failed", result.stderr)
        self.assertEqual(json.loads(result.stdout)["text"], text)

    def test_non_ascii_analyzer_stdout_is_decoded_as_utf8(self) -> None:
        self.assert_analyzed("\u8bf7\u68c0\u67e5\u8fd9\u4e2a\u9879\u76ee")

    def test_non_ascii_analyzer_stderr_is_decoded_as_utf8(self) -> None:
        self.assert_analyzed("\u9879\u76ee/\u8bf4\u660e.txt")

    def test_emoji_survives_the_c_locale(self) -> None:
        self.assert_analyzed("Inspect \U0001f680 now")

    def test_ascii_output_is_unaffected(self) -> None:
        self.assert_analyzed("Inspect the project.")


if __name__ == "__main__":
    unittest.main()
