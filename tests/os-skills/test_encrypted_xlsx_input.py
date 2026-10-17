"""Read real password-protected OOXML files without publishing plaintext."""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import olefile
import pandas as pd
from msoffcrypto.format.ooxml import OOXMLFile
from openpyxl import Workbook

SCRIPT = Path(__file__).parents[2] / "src/os-skills/others/xlsx/scripts/xlsx_reader.py"
PASSWORD = '知情, "password"'
PASSWORD_VARIABLE = "ANOLISA_TEST_WORKBOOK_PASSWORD"


class EncryptedXlsxInputTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("encrypted_table_reader", SCRIPT)
        cls.reader = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.reader)
        cls.directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.directory.cleanup)
        cls.root = Path(cls.directory.name)
        cls.plain = cls.root / "plain.xlsx"
        workbook = Workbook()
        for index, title in enumerate(("Sales", "预算 2024")):
            sheet = workbook.active if index == 0 else workbook.create_sheet()
            sheet.title = title
            sheet.append(["label", "amount"])
            for row in range(1, 5):
                sheet.append([f'收入, "{row}"', row])
        workbook.save(cls.plain)
        workbook.close()
        cls.encrypted = cls.root / "protected.xlsx"
        with cls.plain.open("rb") as source, cls.encrypted.open("wb") as destination:
            OOXMLFile(source).encrypt(PASSWORD, destination)

    def run_cli(self, path, *arguments, password=PASSWORD):
        environment = {**os.environ, "PYTHONUTF8": "1"}
        environment.pop(PASSWORD_VARIABLE, None)
        if password is not None:
            environment[PASSWORD_VARIABLE] = password
        return subprocess.run(
            [sys.executable, str(SCRIPT), str(path), *arguments],
            env=environment,
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=30,
        )

    def test_actual_encrypted_workbook_all_sheets_matches_plain_data(self):
        expected = self.reader.detect_and_load(str(self.plain))
        actual = self.reader.detect_and_load(str(self.encrypted), password=PASSWORD)
        self.assertEqual(list(actual), ["Sales", "预算 2024"])
        for title in actual:
            pd.testing.assert_frame_equal(actual[title], expected[title])

    def test_named_unicode_sheet_uses_the_existing_selection_contract(self):
        actual = self.reader.detect_and_load(
            str(self.encrypted), sheet_name_filter="预算 2024", password=PASSWORD
        )
        self.assertEqual(list(actual), ["预算 2024"])
        self.assertEqual(actual["预算 2024"]["amount"].tolist(), [1, 2, 3, 4])

    def test_uppercase_macro_enabled_extension_uses_ooxml_reader(self):
        path = self.root / "protected.XLSM"
        path.write_bytes(self.encrypted.read_bytes())
        actual = self.reader.detect_and_load(str(path), password=PASSWORD)
        self.assertEqual(list(actual), ["Sales", "预算 2024"])

    def test_password_is_required_with_an_actionable_cli_hint(self):
        with self.assertRaises((ValueError, RuntimeError)) as raised:
            self.reader.detect_and_load(str(self.encrypted))
        self.assertIn("--password-env", str(raised.exception))

    def test_wrong_password_is_an_ordinary_secret_free_read_error(self):
        with self.assertRaises(ValueError) as raised:
            self.reader.detect_and_load(str(self.encrypted), password="wrong-secret")
        self.assertIn("password", str(raised.exception).lower())
        self.assertNotIn("wrong-secret", str(raised.exception))
        self.assertNotIn(PASSWORD, str(raised.exception))

    def test_missing_optional_backend_has_an_install_hint(self):
        with mock.patch.dict(sys.modules, {"msoffcrypto": None}):
            with self.assertRaises(RuntimeError) as raised:
                self.reader.detect_and_load(str(self.encrypted), password=PASSWORD)
        self.assertIn("pip install msoffcrypto-tool", str(raised.exception))

    def test_plain_workbook_keeps_working_without_optional_backend(self):
        with mock.patch.dict(sys.modules, {"msoffcrypto": None}):
            actual = self.reader.detect_and_load(str(self.plain), password=PASSWORD)
        self.assertEqual(list(actual), ["Sales", "预算 2024"])

    def test_decryption_is_in_memory_and_preserves_source_bytes(self):
        original = self.encrypted.read_bytes()
        entries = set(self.root.iterdir())
        with mock.patch(
            "tempfile.mkstemp", side_effect=AssertionError("plaintext temp file")
        ), mock.patch("tempfile.TemporaryFile", side_effect=AssertionError("plaintext temp file")):
            actual = self.reader.detect_and_load(str(self.encrypted), password=PASSWORD)
        self.assertEqual(actual["Sales"]["amount"].tolist(), [1, 2, 3, 4])
        self.assertEqual(self.encrypted.read_bytes(), original)
        self.assertEqual(set(self.root.iterdir()), entries)

    def test_tampered_encrypted_payload_fails_integrity_check(self):
        path = self.root / "tampered.xlsx"
        path.write_bytes(self.encrypted.read_bytes())
        with olefile.OleFileIO(path, write_mode=True) as compound:
            payload = bytearray(compound.openstream("EncryptedPackage").read())
            payload[-16] ^= 1
            compound.write_stream("EncryptedPackage", bytes(payload))
        with self.assertRaises(ValueError) as raised:
            self.reader.detect_and_load(str(path), password=PASSWORD)
        self.assertIn("integrity", str(raised.exception).lower())

    def test_actual_cli_reads_password_from_named_environment_variable(self):
        original = self.encrypted.read_bytes()
        result = self.run_cli(
            self.encrypted, "--json", "--sheet", "Sales", "--password-env", PASSWORD_VARIABLE
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(list(report["structure"]), ["Sales"])
        self.assertEqual(report["structure"]["Sales"]["preview"][0]["label"], '收入, "1"')
        self.assertEqual(self.encrypted.read_bytes(), original)
        self.assertNotIn(PASSWORD, result.stdout + result.stderr)

    def test_missing_environment_variable_is_a_usage_error(self):
        result = self.run_cli(self.encrypted, "--password-env", PASSWORD_VARIABLE, password=None)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("not set", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_wrong_cli_password_is_secret_free_and_nonzero(self):
        result = self.run_cli(
            self.encrypted, "--password-env", PASSWORD_VARIABLE, password="bad-secret-value"
        )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("password", result.stderr.lower())
        self.assertNotIn("bad-secret-value", result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_password_option_rejects_non_ooxml_inputs(self):
        path = self.root / "data.csv"
        path.write_text("amount\n1\n", encoding="utf-8")
        result = self.run_cli(path, "--password-env", PASSWORD_VARIABLE)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(".xlsx", result.stderr)
        self.assertIn(".xlsm", result.stderr)

    def test_empty_password_environment_value_is_an_actionable_usage_error(self):
        result = self.run_cli(
            self.encrypted, "--json", "--password-env", PASSWORD_VARIABLE, password=""
        )
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("non-empty", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_default_plain_input_keeps_working_without_optional_backend(self):
        with mock.patch.dict(sys.modules, {"msoffcrypto": None}):
            self.assertEqual(
                list(self.reader.detect_and_load(str(self.plain))), ["Sales", "预算 2024"]
            )

    def test_corrupt_non_ooxml_input_has_a_safe_read_error(self):
        path = self.root / "corrupt.xlsx"
        path.write_bytes(b"not an OOXML or encrypted Office file")
        with self.assertRaises(ValueError) as raised:
            self.reader.detect_and_load(str(path), password=PASSWORD)
        self.assertNotIn(PASSWORD, str(raised.exception))


if __name__ == "__main__":
    unittest.main()
