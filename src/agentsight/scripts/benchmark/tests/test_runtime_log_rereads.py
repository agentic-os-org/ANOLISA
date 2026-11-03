"""Preserve available diagnostics when log continuity changes during capture."""

import sys
from pathlib import Path
from typing import Any, BinaryIO

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "campaign"))
from campaign_runtime import capture_runtime_log, log_position


@pytest.mark.parametrize("legacy", [False, True])
@pytest.mark.parametrize("fatal", [False, True])
@pytest.mark.parametrize("already_rotated", [False, True])
def test_rotation_during_open_preserves_replacement(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    legacy: bool,
    fatal: bool,
    already_rotated: bool,
) -> None:
    source, destination = tmp_path / "runtime.log", tmp_path / "captured.log"
    source.write_bytes(b"old unrelated worker panicked at startup\n")
    cursor = log_position(source)
    assert cursor is not None
    if already_rotated:
        source.rename(tmp_path / "before-capture.log")
    with source.open("ab") as handle:
        handle.write(b"healthy measured interval\n")
    replacement = b"worker panicked at storage\n" if fatal else b"replacement\n"
    original_open = Path.open
    opens = 0

    def open_and_rotate(path: Path, *args: Any, **kwargs: Any) -> Any:
        nonlocal opens
        handle = original_open(path, *args, **kwargs)
        if path == source:
            opens += 1
            if opens == 1:
                path.rename(tmp_path / "old.log")
                with original_open(path, "wb") as output:
                    output.write(replacement)
        return handle

    monkeypatch.setattr(Path, "open", open_and_rotate)
    assert capture_runtime_log(source, cursor.size if legacy else cursor, destination) == (
        False if fatal else None,
        ["panic"] if fatal else [],
    )
    assert destination.read_bytes() == b"healthy measured interval\n\n" + replacement
    assert opens == 2


@pytest.mark.parametrize("fatal", [False, True])
def test_copytruncate_during_read_preserves_replacement(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, fatal: bool
) -> None:
    source, destination = tmp_path / "runtime.log", tmp_path / "captured.log"
    source.write_bytes(b"prior startup\n" * 20)
    cursor = log_position(source)
    with source.open("ab") as handle:
        handle.write(b"healthy measured interval\n")
    replacement = b"worker panicked at storage\n" if fatal else b"replacement\n"
    replacement += b"new output\n" * 100
    original_open = Path.open
    payload_reads = 0

    class TruncatingReader:
        def __init__(self, handle: BinaryIO) -> None:
            self.handle = handle

        def __getattr__(self, name: str) -> Any:
            return getattr(self.handle, name)

        def __enter__(self) -> "TruncatingReader":
            return self

        def __exit__(self, *args: Any) -> None:
            self.handle.close()

        def read(self, size: int = -1) -> bytes:
            nonlocal payload_reads
            payload = self.handle.read(size)
            if size == -1:
                payload_reads += 1
                if payload_reads == 1:
                    with original_open(source, "wb") as output:
                        output.write(replacement)
            return payload

    def opened(path: Path, *args: Any, **kwargs: Any) -> Any:
        handle = original_open(path, *args, **kwargs)
        return TruncatingReader(handle) if path == source else handle

    monkeypatch.setattr(Path, "open", opened)
    assert capture_runtime_log(source, cursor, destination) == (
        False if fatal else None,
        ["panic"] if fatal else [],
    )
    assert destination.read_bytes() == b"healthy measured interval\n\n" + replacement
    assert payload_reads == 2


@pytest.mark.parametrize("change", ["rotation", "copytruncate", "unlink"])
@pytest.mark.parametrize("fatal", [False, True])
def test_failed_recovery_preserves_already_observed_evidence(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, change: str, fatal: bool
) -> None:
    source, destination = tmp_path / "runtime.log", tmp_path / "captured.log"
    source.write_bytes(b"prior startup\n" * 20)
    cursor = log_position(source)
    observed = b"worker panicked at storage\n" if fatal else b"healthy interval\n"
    with source.open("ab") as handle:
        handle.write(observed)
    original_open = Path.open
    changed = False

    class ChangingReader:
        def __init__(self, handle: BinaryIO) -> None:
            self.handle = handle

        def __getattr__(self, name: str) -> Any:
            return getattr(self.handle, name)

        def __enter__(self) -> "ChangingReader":
            return self

        def __exit__(self, *args: Any) -> None:
            self.handle.close()

        def read(self, size: int = -1) -> bytes:
            nonlocal changed
            if size == -1 and changed:
                raise OSError("reread unavailable")
            payload = self.handle.read(size)
            if size == -1:
                changed = True
                if change == "unlink":
                    source.unlink()
                else:
                    if change == "rotation":
                        source.rename(tmp_path / "old.log")
                    with original_open(source, "wb") as output:
                        output.write(b"replacement\n" * 100)
            return payload

    def opened(path: Path, *args: Any, **kwargs: Any) -> Any:
        if path == source and changed:
            raise OSError("replacement unavailable")
        handle = original_open(path, *args, **kwargs)
        return ChangingReader(handle) if path == source else handle

    monkeypatch.setattr(Path, "open", opened)
    assert capture_runtime_log(source, cursor, destination) == (
        False if fatal else None,
        ["panic"] if fatal else [],
    )
    assert destination.read_bytes() == observed


def test_rotation_reread_does_not_chase_repeated_replacements(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source, destination = tmp_path / "runtime.log", tmp_path / "captured.log"
    source.write_bytes(b"prior startup\n")
    cursor = log_position(source)
    with source.open("ab") as handle:
        handle.write(b"healthy measured interval\n")
    original_open = Path.open
    opens = 0

    def open_and_rotate(path: Path, *args: Any, **kwargs: Any) -> Any:
        nonlocal opens
        handle = original_open(path, *args, **kwargs)
        if path == source:
            opens += 1
            assert opens <= 2, "capture must not chase repeated replacements"
            path.rename(tmp_path / f"old-{opens}.log")
            with original_open(path, "wb") as output:
                output.write(b"worker panicked at storage\n" if opens == 1 else b"out of memory\n")
        return handle

    monkeypatch.setattr(Path, "open", open_and_rotate)
    assert capture_runtime_log(source, cursor, destination) == (False, ["panic"])
    assert destination.read_bytes() == (
        b"healthy measured interval\n\nworker panicked at storage\n"
    )
    assert opens == 2
