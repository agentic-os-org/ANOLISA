"""Discriminating tests for benchmark runtime evidence continuity."""

import sys
from pathlib import Path
from typing import Any, BinaryIO

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "campaign"))
from campaign_runtime import capture_runtime_log, log_position


@pytest.mark.parametrize(
    "change", ["replace_short", "replace_long", "truncate_short", "truncate_regrow"]
)
@pytest.mark.parametrize("fatal", [False, True])
def test_changed_log_cannot_certify_a_clean_run(
    tmp_path: Path, change: str, fatal: bool
) -> None:
    source = tmp_path / "runtime.log"
    destination = tmp_path / "captured.log"
    source.write_text("prior startup output\n" * 20, encoding="utf-8")
    start = log_position(source)
    if change.startswith("replace"):
        source.rename(tmp_path / "rotated.log")
    payload = "worker panicked at storage\n" if fatal else "new output\n"
    if change.endswith("long") or change.endswith("regrow"):
        payload += "normal output\n" * 100
    source.write_text(payload, encoding="utf-8")

    clean, errors = capture_runtime_log(source, start, destination)

    assert clean is (False if fatal else None)
    assert errors == (["panic"] if fatal else [])
    assert destination.read_text(encoding="utf-8") == payload


def test_unchanged_log_uses_only_measured_append(tmp_path: Path) -> None:
    source = tmp_path / "runtime.log"
    destination = tmp_path / "captured.log"
    source.write_text("old worker panicked at startup\n" * 200, encoding="utf-8")
    start = log_position(source)
    with source.open("a", encoding="utf-8") as handle:
        handle.write("measured healthy operation\n")
    assert capture_runtime_log(source, start, destination) == (True, [])
    assert destination.read_text(encoding="utf-8") == "measured healthy operation\n"


def test_empty_initial_log_can_grow(tmp_path: Path) -> None:
    source = tmp_path / "runtime.log"
    source.touch()
    start = log_position(source)
    source.write_text("healthy operation\n", encoding="utf-8")
    assert capture_runtime_log(source, start, tmp_path / "captured.log") == (True, [])


@pytest.mark.parametrize("fatal", [False, True])
def test_rotation_during_open_cannot_hide_replacement(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, fatal: bool
) -> None:
    source = tmp_path / "runtime.log"
    destination = tmp_path / "captured.log"
    source.write_text("prior\n", encoding="utf-8")
    start = log_position(source)
    with source.open("a", encoding="utf-8") as handle:
        handle.write("healthy\n")
    original_open = Path.open
    rotated = False

    def open_and_rotate(path: Path, *args: Any, **kwargs: Any) -> Any:
        nonlocal rotated
        handle = original_open(path, *args, **kwargs)
        if path == source and not rotated:
            rotated = True
            path.rename(tmp_path / "old.log")
            with original_open(path, "w", encoding="utf-8") as replacement:
                replacement.write(
                    "worker panicked at storage\n" if fatal else "replacement\n"
                )
        return handle

    monkeypatch.setattr(Path, "open", open_and_rotate)
    clean, errors = capture_runtime_log(source, start, destination)
    assert clean is (False if fatal else None)
    assert errors == (["panic"] if fatal else [])
    captured = destination.read_text(encoding="utf-8")
    assert "healthy" in captured
    assert ("panicked" if fatal else "replacement") in captured


def test_copytruncate_during_read_preserves_available_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "runtime.log"
    source.write_text("prior\n" * 20, encoding="utf-8")
    start = log_position(source)
    with source.open("a", encoding="utf-8") as handle:
        handle.write("healthy\n")
    original_open = Path.open

    class TruncatingReader:
        def __init__(self, handle: BinaryIO) -> None:
            self.handle = handle
            self.changed = False

        def __enter__(self) -> "TruncatingReader":
            return self

        def __exit__(self, *args: Any) -> None:
            self.handle.close()

        def fileno(self) -> int:
            return self.handle.fileno()

        def seek(self, *args: Any) -> int:
            return self.handle.seek(*args)

        def read(self, size: int = -1) -> bytes:
            payload = self.handle.read(size)
            if size == -1 and not self.changed:
                self.changed = True
                with original_open(source, "w", encoding="utf-8") as replacement:
                    replacement.write(
                        "worker panicked at storage\n" + "replacement\n" * 100
                    )
            return payload

    def open_with_truncation(path: Path, *args: Any, **kwargs: Any) -> Any:
        handle = original_open(path, *args, **kwargs)
        return TruncatingReader(handle) if path == source else handle

    monkeypatch.setattr(Path, "open", open_with_truncation)
    destination = tmp_path / "captured.log"
    assert capture_runtime_log(source, start, destination) == (False, ["panic"])
    assert "panicked" in destination.read_text(encoding="utf-8")
