"""Keep optional downloads and their receipts away from the developer's profile."""

from pathlib import Path

import pytest


@pytest.fixture(autouse=True)
def isolated_downloads(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path / "user-data"))
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path / "user-cache"))
