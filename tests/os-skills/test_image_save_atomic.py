"""Check image publication failures without contacting an image service."""

import base64
import importlib.util
import io
import os
import stat
import tempfile
import unittest
from contextlib import contextmanager, redirect_stdout
from pathlib import Path
from typing import Iterator
from unittest.mock import patch

SCRIPT = (
    Path(__file__).resolve().parents[2] / "src/os-skills/others/image-gen/scripts/generate_image.py"
)
SPEC = importlib.util.spec_from_file_location("image_generator", SCRIPT)
GENERATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GENERATOR)
PAYLOAD = b"replacement image bytes"
SOURCE = "b64:" + base64.b64encode(PAYLOAD).decode("ascii")


class ImageSaveTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.target = self.directory / "image.png"
        self.target.write_bytes(b"previous image")

    def test_partial_write_preserves_destination_and_cleans_staging(self) -> None:
        real_open, real_fdopen = open, os.fdopen

        class FailingWriter:
            def __init__(self, stream: io.BufferedWriter) -> None:
                self.stream = stream

            def write(self, data: bytes) -> int:
                self.stream.write(data[:3])
                self.stream.flush()
                raise OSError("simulated full disk")

        @contextmanager
        def failing_open(*args: object, **kwargs: object) -> Iterator[FailingWriter]:
            with real_open(*args, **kwargs) as stream:
                yield FailingWriter(stream)

        @contextmanager
        def failing_fdopen(*args: object, **kwargs: object) -> Iterator[FailingWriter]:
            with real_fdopen(*args, **kwargs) as stream:
                yield FailingWriter(stream)

        output = io.StringIO()
        with patch.object(GENERATOR, "open", failing_open, create=True), patch.object(
            GENERATOR.os, "fdopen", failing_fdopen
        ), redirect_stdout(output):
            with self.assertRaises(OSError):
                GENERATOR._save(SOURCE, str(self.target))
        self.assertEqual(self.target.read_bytes(), b"previous image")
        self.assertEqual(list(self.directory.iterdir()), [self.target])
        self.assertNotIn("Saved", output.getvalue())

    def test_replace_failure_preserves_destination(self) -> None:
        with patch.object(GENERATOR.os, "replace", side_effect=PermissionError("locked")):
            with self.assertRaises(PermissionError):
                GENERATOR._save(SOURCE, str(self.target))
        self.assertEqual(self.target.read_bytes(), b"previous image")
        self.assertEqual(list(self.directory.iterdir()), [self.target])

    def test_replace_failure_cleans_readonly_staging(self) -> None:
        self.target.chmod(stat.S_IREAD)
        self.addCleanup(self.target.chmod, stat.S_IREAD | stat.S_IWRITE)
        with patch.object(GENERATOR.os, "replace", side_effect=PermissionError("locked")):
            with self.assertRaisesRegex(PermissionError, "locked"):
                GENERATOR._save(SOURCE, str(self.target))
        self.assertEqual(self.target.read_bytes(), b"previous image")
        self.assertEqual(list(self.directory.iterdir()), [self.target])

    def test_success_and_new_parent(self) -> None:
        for target in (self.target, self.directory / "nested" / "new.png"):
            with self.subTest(target=target), redirect_stdout(io.StringIO()):
                GENERATOR._save(SOURCE, str(target))
                self.assertEqual(target.read_bytes(), PAYLOAD)
                self.assertEqual(list(target.parent.iterdir()), [target])

    def test_existing_symlink_still_updates_its_target(self) -> None:
        link = self.directory / "link.png"
        try:
            link.symlink_to(self.target)
        except (OSError, NotImplementedError) as exc:
            self.skipTest(f"Symlinks unavailable: {exc}")
        with redirect_stdout(io.StringIO()):
            GENERATOR._save(SOURCE, str(link))
        self.assertTrue(link.is_symlink())
        self.assertEqual(self.target.read_bytes(), PAYLOAD)

    def test_download_preserves_existing_mode(self) -> None:
        self.target.chmod(0o640)
        mode = stat.S_IMODE(self.target.stat().st_mode)
        with patch.object(
            GENERATOR.urllib.request, "urlopen", return_value=io.BytesIO(PAYLOAD)
        ), redirect_stdout(io.StringIO()):
            GENERATOR._save("https://example.invalid/image.png", str(self.target))
        self.assertEqual(self.target.read_bytes(), PAYLOAD)
        self.assertEqual(stat.S_IMODE(self.target.stat().st_mode), mode)

    def test_close_failure_does_not_publish_new_file(self) -> None:
        target = self.directory / "new.png"
        real_fdopen = os.fdopen

        @contextmanager
        def failing_close(*args: object, **kwargs: object) -> Iterator[io.BufferedWriter]:
            with real_fdopen(*args, **kwargs) as stream:
                yield stream
            raise OSError("simulated close failure")

        with patch.object(GENERATOR.os, "fdopen", failing_close):
            with self.assertRaises(OSError):
                GENERATOR._save(SOURCE, str(target))
        self.assertFalse(target.exists())
        self.assertEqual(list(self.directory.iterdir()), [self.target])


if __name__ == "__main__":
    unittest.main(verbosity=2)
