"""The symbol table is not ASCII: decode tool output as UTF-8, not by locale.

``nm_symbols`` reads ``nm`` output with ``universal_newlines=True`` and no
``encoding``, so the pipe is decoded with ``locale.getpreferredencoding``.
C++ translation units may declare non-ASCII identifiers, which ``nm`` prints
verbatim; under the C locale one such symbol raised ``UnicodeDecodeError``
(a ``ValueError``, not the ``CalledProcessError`` the function catches) and the
extractor died with a traceback instead of using the ``nm -D`` fallback.
"""

import json
import unittest
from pathlib import Path
import tempfile

from subprocess_encoding_support import SCRIPTS, encode, fake_utility, run_helper

SYMBOL = "\u4e2d\u6587\u51fd\u6570"

# `nm --defined-only` prints the symbol name verbatim after address and type.
NM_OUTPUT = (
    "0000000000000010 T SSL_write_ex\n"
    "0000000000000020 T SSL_read_ex\n"
    "0000000000000030 T SSL_do_handshake\n"
    f"0000000000000040 T {SYMBOL}\n"
)

HELPER = (
    "import base64,importlib.util,json,sys\n"
    "spec=importlib.util.spec_from_file_location("
    f"'extract_codex_offsets', r'{SCRIPTS / 'extract-codex-offsets.py'}')\n"
    "codex=importlib.util.module_from_spec(spec)\n"
    "sys.modules['extract_codex_offsets']=codex\n"
    "spec.loader.exec_module(codex)\n"
    "binary=base64.urlsafe_b64decode(sys.argv[1]).decode('utf-8')\n"
    "sys.stdout.buffer.write(json.dumps(codex.nm_symbols(binary),"
    " ensure_ascii=False).encode('utf-8'))\n"
)


class NmOutputEncodingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.binary = self.root / "codex"
        self.binary.write_bytes(b"\x7fELF")

    def fake_nm(self, output: str) -> Path:
        """Stand in for `nm` so the fixture prints its own UTF-8 bytes."""
        return fake_utility(self.root, "nm", output.encode("utf-8"), b"", 0)

    @unittest.skipUnless(__import__("os").name == "posix", "the C locale fixture needs POSIX")
    def test_non_ascii_symbol_is_decoded_as_utf8(self) -> None:
        result = run_helper(
            self.root,
            HELPER,
            encode(str(self.binary)),
            path_prefix=self.fake_nm(NM_OUTPUT),
        )
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(result.returncode, 0, result.stderr)
        symbols = json.loads(result.stdout)
        self.assertEqual(symbols["SSL_write_ex"], 0x10)
        self.assertEqual(symbols[SYMBOL], 0x40)

    @unittest.skipUnless(__import__("os").name == "posix", "the C locale fixture needs POSIX")
    def test_ascii_symbol_table_is_unaffected(self) -> None:
        plain = "0000000000000010 T SSL_write\n0000000000000020 T SSL_read\n"
        result = run_helper(
            self.root, HELPER, encode(str(self.binary)), path_prefix=self.fake_nm(plain)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["SSL_read"], 0x20)


if __name__ == "__main__":
    unittest.main()
