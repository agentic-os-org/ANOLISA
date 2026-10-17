"""Discover and explicitly extract embedded PDF payloads without clobbering files."""

import contextlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import pymupdf

SCRIPT = Path(__file__).parents[2] / "src/os-skills/others/pdf-reader/scripts/read_pdf.py"


class PdfAttachmentTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("attachment_reader", SCRIPT)
        cls.reader = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.reader)

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.path = self.root / "portfolio.pdf"
        self.payloads = [
            'label,amount\n"收入,quoted",1\n'.encode("utf-8"),
            bytes(range(256)) * 200,
            b"",
        ]
        with pymupdf.open() as doc:
            for _ in range(2):
                page = doc.new_page()
                page.insert_text((30, 30), "Document page")
            for index, (name, data) in enumerate(
                zip(("Archive", "Binary", "Empty"), self.payloads)
            ):
                doc.embfile_add(
                    name, data, filename=f"nested/file-{index}.bin", desc=f"Attached item {index}"
                )
            doc.save(self.path)

    def run_cli(self, *arguments):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "-f", str(self.path), *arguments],
            env={**os.environ, "PYTHONUTF8": "1"},
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=30,
        )

    def discover(self, *arguments):
        result = self.run_cli("--format", "json", "--attachments", *arguments)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_real_document_metadata_and_physical_index_order(self):
        report = self.discover()
        with pymupdf.open(self.path) as doc:
            expected = [{"index": i, **doc.embfile_info(i)} for i in range(doc.embfile_count())]
        self.assertEqual(report["attachments"], expected)
        self.assertEqual([item["index"] for item in report["attachments"]], [0, 1, 2])
        self.assertEqual(
            [item["size"] for item in report["attachments"]], list(map(len, self.payloads))
        )
        self.assertLess(report["attachments"][1]["length"], report["attachments"][1]["size"])

    def test_discovery_does_not_load_payload_or_write_files(self):
        entries = set(self.root.iterdir())
        original = self.path.read_bytes()
        with mock.patch.object(
            pymupdf.Document, "embfile_get", side_effect=AssertionError("loaded payload")
        ), mock.patch.object(
            sys, "argv", [str(SCRIPT), "-f", str(self.path), "--format", "json", "--attachments"]
        ), contextlib.redirect_stdout(
            io.StringIO()
        ) as output:
            self.reader.main()
        self.assertEqual(len(json.loads(output.getvalue())["attachments"]), 3)
        self.assertEqual(set(self.root.iterdir()), entries)
        self.assertEqual(self.path.read_bytes(), original)

    def test_empty_document_attachment_list(self):
        with pymupdf.open() as doc:
            doc.new_page()
            doc.save(self.path)
        self.assertEqual(self.discover()["attachments"], [])

    def test_selected_binary_utf8_and_empty_payloads_extract_exactly(self):
        original = self.path.read_bytes()
        for index, payload in enumerate(self.payloads):
            with self.subTest(index=index):
                output = self.root / f"提取-{index}.dat"
                result = self.run_cli(
                    "--format", "json", "--extract-attachment", str(index), "--output", str(output)
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(output.read_bytes(), payload)
                record = json.loads(result.stdout)["extracted_attachment"]
                self.assertEqual(record["index"], index)
                self.assertEqual(record["size"], len(payload))
                self.assertEqual(record["output"], str(output))
        self.assertEqual(self.path.read_bytes(), original)

    def test_document_attachments_are_independent_of_page_selection(self):
        report = self.discover("-p", "2", "--metadata")
        self.assertEqual([p["page"] for p in report["pages"]], [2])
        self.assertEqual(len(report["attachments"]), 3)

    def test_out_of_range_selection_has_no_output_or_traceback(self):
        output = self.root / "absent.dat"
        result = self.run_cli(
            "--format", "json", "--extract-attachment", "3", "--output", str(output)
        )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("index", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertFalse(output.exists())

    def test_incompatible_or_incomplete_options_fail_before_open(self):
        variants = [
            ["--attachments"],
            ["--extract-attachment", "0", "--output", "x.dat"],
            ["--format", "json", "--output", "x.dat"],
            ["--format", "json", "--extract-attachment", "0"],
            ["--format", "json", "--extract-attachment", "-1", "--output", "x.dat"],
            ["--format", "json", "--extract-attachment", "bad", "--output", "x.dat"],
        ]
        for args in variants:
            with self.subTest(args=args), mock.patch.object(
                sys, "argv", [str(SCRIPT), "-f", "absent.pdf", *args]
            ), mock.patch.object(
                self.reader, "_install", side_effect=AssertionError("installed")
            ), contextlib.redirect_stderr(
                io.StringIO()
            ):
                with self.assertRaises(SystemExit) as raised:
                    self.reader.main()
                self.assertEqual(raised.exception.code, 2)

    def test_existing_destination_and_source_cannot_be_overwritten(self):
        other = self.root / "old.dat"
        other.write_bytes(b"old complete output")
        for output in (self.path, other):
            with self.subTest(output=output):
                original = output.read_bytes()
                result = self.run_cli(
                    "--format", "json", "--extract-attachment", "0", "--output", str(output)
                )
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertNotIn("Traceback", result.stderr)
                self.assertEqual(output.read_bytes(), original)

    def test_missing_output_parent_is_an_ordinary_error(self):
        output = self.root / "missing" / "output.dat"
        result = self.run_cli(
            "--format", "json", "--extract-attachment", "0", "--output", str(output)
        )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertFalse(output.parent.exists())
        self.assertNotIn("Traceback", result.stderr)

    def test_link_failure_leaves_no_partial_output_or_staging(self):
        output = self.root / "new.dat"
        before = set(self.root.iterdir())
        with mock.patch.object(self.reader.os, "link", side_effect=OSError("publication failed")):
            with self.assertRaises(OSError):
                self.reader._publish_attachment(self.payloads[1], output)
        self.assertFalse(output.exists())
        self.assertEqual(set(self.root.iterdir()), before)

    def test_flush_failure_leaves_no_partial_output_or_staging(self):
        output = self.root / "new.dat"
        before = set(self.root.iterdir())
        with mock.patch.object(self.reader.os, "fsync", side_effect=OSError("flush failed")):
            with self.assertRaises(OSError):
                self.reader._publish_attachment(self.payloads[1], output)
        self.assertFalse(output.exists())
        self.assertEqual(set(self.root.iterdir()), before)

    def test_partial_staging_write_never_publishes_prefix(self):
        output = self.root / "new.dat"
        before = set(self.root.iterdir())
        actual_open = Path.open

        class PartialWriter:
            def __init__(self, stream):
                self.stream = stream

            def __enter__(self):
                return self

            def __exit__(self, *arguments):
                self.stream.close()

            def write(self, data):
                self.stream.write(data[:7])
                self.stream.flush()
                raise OSError("partial disk-full write")

        def faulty_open(path, *arguments, **keywords):
            stream = actual_open(path, *arguments, **keywords)
            if path.name == "payload.bin" and arguments == ("xb",):
                return PartialWriter(stream)
            return stream

        with mock.patch.object(Path, "open", faulty_open):
            with self.assertRaises(OSError):
                self.reader._publish_attachment(self.payloads[1], output)
        self.assertFalse(output.exists())
        self.assertEqual(set(self.root.iterdir()), before)

    def test_concurrent_destination_creation_is_never_replaced(self):
        output = self.root / "new.dat"
        link = os.link
        before = set(self.root.iterdir())

        def race(source, target):
            target.write_bytes(b"concurrent owner")
            return link(source, target)

        with mock.patch.object(self.reader.os, "link", side_effect=race):
            with self.assertRaises(FileExistsError):
                self.reader._publish_attachment(self.payloads[1], output)
        self.assertEqual(output.read_bytes(), b"concurrent owner")
        self.assertEqual(set(self.root.iterdir()), before | {output})

    def test_duplicate_embedded_keys_remain_unambiguous_by_index(self):
        duplicate = self.root / "duplicate.pdf"
        with pymupdf.open(self.path) as doc:
            kind, names = doc.xref_get_key(doc.pdf_catalog(), "Names/EmbeddedFiles/Names")
            self.assertEqual(kind, "array")
            doc.xref_set_key(
                doc.pdf_catalog(),
                "Names/EmbeddedFiles/Names",
                names.replace("(Binary)", "(Archive)"),
            )
            doc.save(duplicate)
        self.path = duplicate
        report = self.discover()
        self.assertEqual(
            [item["name"] for item in report["attachments"]][:2], ["Archive", "Archive"]
        )
        output = self.root / "selected.dat"
        result = self.run_cli(
            "--format", "json", "--extract-attachment", "1", "--output", str(output)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output.read_bytes(), self.payloads[1])

    def test_default_mode_retains_schema_and_does_not_discover(self):
        with mock.patch.object(
            self.reader, "_document_attachments", side_effect=AssertionError("discovered")
        ), mock.patch.object(
            sys, "argv", [str(SCRIPT), "-f", str(self.path), "--format", "json"]
        ), contextlib.redirect_stdout(
            io.StringIO()
        ) as output:
            self.reader.main()
        report = json.loads(output.getvalue())
        self.assertEqual(set(report), {"total_pages", "pages"})
        self.assertEqual(set(report["pages"][0]), {"page", "text"})


if __name__ == "__main__":
    unittest.main()
