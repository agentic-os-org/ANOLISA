"""Unit tests for security_events.config — log path selection."""

from pathlib import Path

import pytest
from agent_sec_cli.security_events.config import (
    FALLBACK_LOG_PATH,
    PRIMARY_LOG_PATH,
    get_data_dir,
    get_db_path,
    get_log_path,
    get_stream_db_path,
    get_stream_log_path,
)


@pytest.fixture
def no_data_dir_override(monkeypatch: pytest.MonkeyPatch) -> None:
    """Remove AGENT_SEC_DATA_DIR so tests exercise the real path fallback logic."""
    monkeypatch.delenv("AGENT_SEC_DATA_DIR", raising=False)


class TestGetLogPath:
    def test_primary_path_when_writable(
        self,
        no_data_dir_override: None,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.os.access",
            lambda path, mode: True,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.is_dir",
            lambda path: True,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.mkdir",
            lambda path, *args, **kwargs: None,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.chmod",
            lambda path, mode: None,
        )

        path = get_log_path()
        assert path == PRIMARY_LOG_PATH

    def test_fallback_when_primary_not_writable(
        self,
        no_data_dir_override: None,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.os.access",
            lambda path, mode: False,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.is_dir",
            lambda path: True,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.mkdir",
            lambda path, *args, **kwargs: None,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.chmod",
            lambda path, mode: None,
        )

        path = get_log_path()
        assert path == FALLBACK_LOG_PATH

    def test_fallback_when_makedirs_fails(
        self,
        no_data_dir_override: None,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        mkdir_results: list[Exception | None] = [OSError("permission denied"), None]

        def mkdir_with_primary_failure(
            path: Path, *args: object, **kwargs: object
        ) -> None:
            result = mkdir_results.pop(0)
            if result is not None:
                raise result

        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.mkdir",
            mkdir_with_primary_failure,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.chmod",
            lambda path, mode: None,
        )

        path = get_log_path()
        assert path == FALLBACK_LOG_PATH


class TestGetDbPath:
    def test_db_path_uses_primary_dir(
        self,
        no_data_dir_override: None,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.os.access",
            lambda path, mode: True,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.is_dir",
            lambda path: True,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.mkdir",
            lambda path, *args, **kwargs: None,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.chmod",
            lambda path, mode: None,
        )

        path = get_db_path()
        assert path == "/var/log/agent-sec/security-events.db"

    def test_db_path_uses_fallback_dir(
        self,
        no_data_dir_override: None,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.os.access",
            lambda path, mode: False,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.is_dir",
            lambda path: True,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.mkdir",
            lambda path, *args, **kwargs: None,
        )
        monkeypatch.setattr(
            "agent_sec_cli.security_events.config.Path.chmod",
            lambda path, mode: None,
        )

        path = get_db_path()
        assert path.endswith(".agent-sec-core/security-events.db")


class TestStreamPaths:
    def test_env_override_resolves_stream_specific_paths(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))

        assert get_data_dir() == tmp_path
        assert get_stream_log_path("observability") == str(
            tmp_path / "observability.jsonl"
        )
        assert get_stream_db_path("observability") == str(tmp_path / "observability.db")

    def test_security_event_paths_remain_default_stream(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(tmp_path))

        assert get_log_path() == str(tmp_path / "security-events.jsonl")
        assert get_db_path() == str(tmp_path / "security-events.db")

    def test_stream_names_reject_path_traversal(self) -> None:
        with pytest.raises(ValueError):
            get_stream_log_path("../observability")
