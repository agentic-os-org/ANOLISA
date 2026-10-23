"""Exercise password inputs using real, locally generated encrypted PDFs."""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import pymupdf as fitz

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/others/pdf-reader/scripts/read_pdf.py"
USER_PASSWORD = "fixture-user-secret"
OWNER_PASSWORD = "fixture-owner-secret"
ENVIRONMENT_KEY = "ANOLISA_TEST_PDF_PASSWORD"


class PDFPasswordTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.encrypted = Path(cls.temporary.name) / "protected.pdf"
        cls.plain = Path(cls.temporary.name) / "plain.pdf"
        with fitz.open() as document:
            for text in ("First page", "Second page"):
                document.new_page().insert_text((72, 72), text)
            document.set_metadata({"title": "Protected fixture", "author": "Test author"})
            document.save(cls.plain)
            document.save(
                cls.encrypted,
                encryption=fitz.PDF_ENCRYPT_AES_256,
                user_pw=USER_PASSWORD,
                owner_pw=OWNER_PASSWORD,
                permissions=fitz.PDF_PERM_COPY | fitz.PDF_PERM_ACCESSIBILITY,
            )
        cls.encrypted_bytes = cls.encrypted.read_bytes()

    def run_reader(self, *options, path=None, password=None):
        environment = os.environ.copy()
        environment.pop(ENVIRONMENT_KEY, None)
        if password is not None:
            environment[ENVIRONMENT_KEY] = password
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "-f", str(path or self.encrypted), *options],
            env=environment,
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=30,
        )
        self.assertEqual(self.encrypted.read_bytes(), self.encrypted_bytes)
        return result

    def assert_authentication_error(self, result):
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("ERROR:", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertNotIn(USER_PASSWORD, result.stderr)
        self.assertNotIn(OWNER_PASSWORD, result.stderr)

    def test_user_and_owner_passwords_extract_all_pages(self):
        for password in (USER_PASSWORD, OWNER_PASSWORD):
            with self.subTest(password_type="user" if password == USER_PASSWORD else "owner"):
                result = self.run_reader("--password", password, "--format", "json")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(
                    json.loads(result.stdout),
                    {
                        "total_pages": 2,
                        "pages": [
                            {"page": 1, "text": "First page"},
                            {"page": 2, "text": "Second page"},
                        ],
                    },
                )

    def test_environment_password_supports_selected_pages_and_metadata(self):
        result = self.run_reader(
            "--password-env",
            ENVIRONMENT_KEY,
            "-p",
            "2",
            "--metadata",
            "--format",
            "json",
            password=USER_PASSWORD,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        self.assertEqual(data["pages"], [{"page": 2, "text": "Second page"}])
        self.assertEqual(data["metadata"]["title"], "Protected fixture")
        self.assertNotIn(USER_PASSWORD, result.stdout + result.stderr)

    def test_text_output_after_authentication_uses_existing_page_format(self):
        result = self.run_reader("--password", USER_PASSWORD, "-p", "1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--- Page 1 ---\nFirst page", result.stdout)
        self.assertNotIn("Second page", result.stdout)

    def test_missing_password_has_a_controlled_error(self):
        self.assert_authentication_error(self.run_reader("--format", "json"))

    def test_wrong_password_has_a_controlled_error(self):
        self.assert_authentication_error(self.run_reader("--password", "wrong-fixture-password"))

    def test_unset_password_environment_variable_has_a_controlled_error(self):
        self.assert_authentication_error(self.run_reader("--password-env", ENVIRONMENT_KEY))

    def test_password_sources_are_mutually_exclusive(self):
        result = self.run_reader("--password", USER_PASSWORD, "--password-env", ENVIRONMENT_KEY)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertNotIn(USER_PASSWORD, result.stderr)

    def test_unencrypted_pdf_keeps_existing_behavior(self):
        result = self.run_reader("-p", "1", "--format", "json", path=self.plain)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["pages"], [{"page": 1, "text": "First page"}])


if __name__ == "__main__":
    unittest.main()
