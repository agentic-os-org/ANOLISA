"""Unit tests for telemetry configuration and sentinel gates."""

import errno
from pathlib import Path

from agent_sec_cli import __version__
from agent_sec_cli.telemetry import config
from agent_sec_cli.telemetry.config import (
    DEFAULT_TELEMETRY_LOG_PATH,
    TELEMETRY_DISABLED_SENTINEL,
    TELEMETRY_LINKED_SENTINEL,
    TELEMETRY_LOG_PATH_ENV,
    get_component_fields,
    get_telemetry_log_path,
    is_l1_telemetry_allowed,
    is_l3_telemetry_linked,
    telemetry_log_path_exists,
)


def _raising_os_call(error):
    def fail(path):
        raise error

    return fail


def test_default_telemetry_log_path_is_agentic_os_component_file(monkeypatch) -> None:
    monkeypatch.delenv(TELEMETRY_LOG_PATH_ENV, raising=False)

    assert get_telemetry_log_path() == Path(DEFAULT_TELEMETRY_LOG_PATH)


def test_telemetry_log_path_env_override(monkeypatch, tmp_path: Path) -> None:
    path = tmp_path / "agent-sec-core.jsonl"
    monkeypatch.setenv(TELEMETRY_LOG_PATH_ENV, str(path))

    assert get_telemetry_log_path() == path


def test_telemetry_log_path_exists_only_for_existing_file(
    monkeypatch, tmp_path: Path
) -> None:
    path = tmp_path / "agent-sec-core.jsonl"
    monkeypatch.setenv(TELEMETRY_LOG_PATH_ENV, str(path))
    assert telemetry_log_path_exists() is False

    path.write_text("", encoding="utf-8")
    assert telemetry_log_path_exists() is True


def test_component_fields_are_fixed() -> None:
    assert get_component_fields() == {
        "component.name": "agent-sec-core",
        "component.version": __version__,
        "component.agent_name": "",
    }


def test_sentinel_paths_match_system_contract() -> None:
    assert TELEMETRY_DISABLED_SENTINEL == "/etc/anolisa/.telemetry_disabled"
    assert TELEMETRY_LINKED_SENTINEL == "/etc/anolisa/.telemetry_linked"


def test_l1_is_disabled_when_disabled_sentinel_exists(monkeypatch) -> None:
    monkeypatch.setattr(config, "_lstat", lambda path: object())

    assert is_l1_telemetry_allowed() is False


def test_l1_is_allowed_only_when_disabled_sentinel_is_absent(monkeypatch) -> None:
    monkeypatch.setattr(
        config,
        "_lstat",
        _raising_os_call(FileNotFoundError(errno.ENOENT, "absent")),
    )

    assert is_l1_telemetry_allowed() is True


def test_l1_is_disabled_by_dangling_symlink(monkeypatch, tmp_path: Path) -> None:
    sentinel = tmp_path / ".telemetry_disabled"
    sentinel.symlink_to(tmp_path / "missing-target")
    monkeypatch.setattr(config, "TELEMETRY_DISABLED_SENTINEL", str(sentinel))

    assert is_l1_telemetry_allowed() is False


def test_l1_treats_explicit_enoent_oserror_as_absent(monkeypatch) -> None:
    class ExplicitEnoent(OSError):
        pass

    monkeypatch.setattr(
        config,
        "_lstat",
        _raising_os_call(ExplicitEnoent(errno.ENOENT, "absent")),
    )

    assert is_l1_telemetry_allowed() is True


def test_l1_fails_closed_when_disabled_sentinel_is_unreadable(monkeypatch) -> None:
    for error in (
        PermissionError(errno.EACCES, "denied"),
        FileNotFoundError(errno.EIO, "unexpected file error"),
        OSError(errno.EIO, "io error"),
    ):
        monkeypatch.setattr(config, "_lstat", _raising_os_call(error))
        assert is_l1_telemetry_allowed() is False


def test_l1_gate_is_not_cached(monkeypatch) -> None:
    outcomes = [
        FileNotFoundError(errno.ENOENT, "absent"),
        object(),
        FileNotFoundError(errno.ENOENT, "absent"),
    ]

    def changing_lstat(path):
        outcome = outcomes.pop(0)
        if isinstance(outcome, Exception):
            raise outcome
        return outcome

    monkeypatch.setattr(config, "_lstat", changing_lstat)

    assert is_l1_telemetry_allowed() is True
    assert is_l1_telemetry_allowed() is False
    assert is_l1_telemetry_allowed() is True
    assert outcomes == []


def test_l3_is_linked_only_when_linked_sentinel_exists(monkeypatch) -> None:
    monkeypatch.setattr(config, "_stat", lambda path: object())

    assert is_l3_telemetry_linked() is True


def test_l3_is_not_linked_by_dangling_symlink(monkeypatch, tmp_path: Path) -> None:
    sentinel = tmp_path / ".telemetry_linked"
    sentinel.symlink_to(tmp_path / "missing-target")
    monkeypatch.setattr(config, "TELEMETRY_LINKED_SENTINEL", str(sentinel))

    assert is_l3_telemetry_linked() is False


def test_l3_fails_closed_when_linked_sentinel_is_absent_or_unreadable(
    monkeypatch,
) -> None:
    errors = [
        FileNotFoundError(errno.ENOENT, "absent"),
        PermissionError(errno.EACCES, "denied"),
        OSError(errno.EIO, "io error"),
    ]

    for error in errors:
        monkeypatch.setattr(config, "_stat", _raising_os_call(error))
        assert is_l3_telemetry_linked() is False
