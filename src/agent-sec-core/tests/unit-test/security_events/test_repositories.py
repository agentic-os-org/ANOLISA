"""Focused unit tests for security_events.repositories logging hooks.

Broader repository behavior (insert/query/prune) is covered by the SQLite
store and writer tests; this module exists to lock in the diagnostic warning
that fires when correlation-candidate queries fail.
"""

import logging
from unittest.mock import MagicMock

from agent_sec_cli.security_events.repositories import SecurityEventRepository
from sqlalchemy.exc import OperationalError


class _RaisingSession:
    """Context-managed session whose query raises SQLAlchemyError."""

    def __enter__(self) -> "_RaisingSession":
        return self

    def __exit__(self, *_: object) -> None:
        return None

    def scalars(self, _stmt: object) -> object:
        raise OperationalError("SELECT ...", {}, Exception("simulated"))


def test_query_correlation_candidates_logs_warning_on_sqlalchemy_error(caplog) -> None:
    store = MagicMock()
    store.session_factory.return_value = lambda: _RaisingSession()
    repo = SecurityEventRepository(store)

    with caplog.at_level(
        logging.WARNING, logger="agent_sec_cli.security_events.repositories"
    ):
        result = repo.query_correlation_candidates(
            session_id="sess-1",
            categories=["scan_code"],
            run_id="run-1",
        )

    assert result == []
    store.dispose.assert_called_once()

    matching = [
        r for r in caplog.records if r.message == "correlation candidate query failed"
    ]
    assert len(matching) == 1
    record = matching[0]
    assert record.session_id == "sess-1"
    assert record.run_id == "run-1"
    assert record.data == {"error_type": "OperationalError"}
