"""Unit tests for observability-to-security-event correlation."""

import hashlib
from dataclasses import dataclass
from typing import Any

import pytest
from agent_sec_cli.observability.correlation import (
    ZERO_RUN_ID,
    ObservabilityRecordFields,
    SecurityCorrelationService,
)
from agent_sec_cli.security_events.schema import SecurityEvent


@dataclass(frozen=True)
class _Candidate:
    event: SecurityEvent
    timestamp_epoch: float


class _FakeReader:
    def __init__(self, candidates: list[_Candidate] | None = None) -> None:
        self.candidates = candidates or []
        self.calls: list[dict[str, object]] = []

    def query_correlation_candidates(self, **kwargs: object) -> list[_Candidate]:
        self.calls.append(kwargs)
        return self.candidates


def _record(
    *,
    hook: str = "before_tool_call",
    session_id: str | None = "session-1",
    run_id: str | None = "run-1",
    tool_call_id: str | None = "tool-1",
    observed_at_epoch: float = 100.0,
    metrics: dict[str, Any] | None = None,
) -> ObservabilityRecordFields:
    return ObservabilityRecordFields(
        hook=hook,
        session_id=session_id,
        run_id=run_id,
        tool_call_id=tool_call_id,
        observed_at_epoch=observed_at_epoch,
        metrics=metrics,
    )


def _event(
    *,
    event_id: str,
    category: str,
    timestamp: str = "2026-05-20T00:00:00+00:00",
    session_id: str | None = "session-1",
    run_id: str | None = "run-1",
    tool_call_id: str | None = "tool-1",
    details: dict[str, Any] | None = None,
) -> SecurityEvent:
    return SecurityEvent(
        event_id=event_id,
        event_type=category,
        category=category,
        result="succeeded",
        timestamp=timestamp,
        trace_id="trace-ignored",
        pid=1,
        uid=1,
        session_id=session_id,
        run_id=run_id,
        tool_call_id=tool_call_id,
        details=details or {"event": event_id},
    )


def _result_signatures(results: list[list[Any]]) -> list[list[tuple[str, str]]]:
    return [
        [(item.event.event_id, item.match_reason) for item in row] for row in results
    ]


@pytest.mark.parametrize(
    "hook",
    [
        "before_llm_call",
        "after_llm_call",
        "after_agent_run",
    ],
)
def test_unsupported_hook_returns_empty_without_reader_call(hook: str) -> None:
    reader = _FakeReader()
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(_record(hook=hook))

    assert result == []
    assert reader.calls == []


def test_missing_session_returns_empty_without_reader_call() -> None:
    reader = _FakeReader()
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(_record(session_id=None))

    assert result == []
    assert reader.calls == []


def test_exact_mode_uses_tool_call_id_without_time_window_and_orders_categories() -> (
    None
):
    reader = _FakeReader(
        [
            _Candidate(
                _event(event_id="skill-later", category="skill_ledger"),
                timestamp_epoch=150.0,
            ),
            _Candidate(
                _event(event_id="code-far-away", category="code_scan"),
                timestamp_epoch=1000.0,
            ),
            _Candidate(
                _event(event_id="skill-nearer", category="skill_ledger"),
                timestamp_epoch=110.0,
            ),
            _Candidate(
                _event(event_id="prompt-disallowed", category="prompt_scan"),
                timestamp_epoch=100.0,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(_record(observed_at_epoch=100.0))

    assert reader.calls == [
        {
            "session_id": "session-1",
            "categories": ("code_scan", "skill_ledger", "pii_scan"),
            "run_id": "run-1",
            "tool_call_id": "tool-1",
            "since_epoch": None,
            "until_epoch": None,
        }
    ]
    assert [item.event.event_id for item in result] == [
        "code-far-away",
        "skill-nearer",
    ]
    assert [item.match_reason for item in result] == ["tool_call_id", "tool_call_id"]
    assert [item.time_delta_seconds for item in result] == [900.0, 10.0]


def test_exact_mode_does_not_fallback_when_no_exact_candidates() -> None:
    reader = _FakeReader()
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(_record(observed_at_epoch=100.0))

    assert result == []
    assert reader.calls == [
        {
            "session_id": "session-1",
            "categories": ("code_scan", "skill_ledger", "pii_scan"),
            "run_id": "run-1",
            "tool_call_id": "tool-1",
            "since_epoch": None,
            "until_epoch": None,
        }
    ]


def test_after_tool_call_exact_mode_queries_pii_scan_category() -> None:
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="pii-tool-output",
                    category="pii_scan",
                    details={"request": {"source": "tool_output"}},
                ),
                timestamp_epoch=101.0,
            ),
            _Candidate(
                _event(event_id="code-disallowed", category="code_scan"),
                timestamp_epoch=101.0,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(hook="after_tool_call", observed_at_epoch=100.0)
    )

    assert reader.calls == [
        {
            "session_id": "session-1",
            "categories": ("pii_scan",),
            "run_id": "run-1",
            "tool_call_id": "tool-1",
            "since_epoch": None,
            "until_epoch": None,
        }
    ]
    assert [item.event.event_id for item in result] == ["pii-tool-output"]
    assert [item.match_reason for item in result] == ["tool_call_id"]
    assert [item.time_delta_seconds for item in result] == [1.0]


def test_before_tool_call_exact_mode_ignores_tool_output_pii_scan() -> None:
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="pii-tool-output",
                    category="pii_scan",
                    details={"request": {"source": "tool_output"}},
                ),
                timestamp_epoch=100.1,
            ),
            _Candidate(
                _event(
                    event_id="pii-tool-input",
                    category="pii_scan",
                    details={"request": {"source": "tool_input"}},
                ),
                timestamp_epoch=100.5,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(hook="before_tool_call", observed_at_epoch=100.0)
    )

    assert [item.event.event_id for item in result] == ["pii-tool-input"]


def test_after_tool_call_exact_mode_ignores_tool_input_pii_scan() -> None:
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="pii-tool-input",
                    category="pii_scan",
                    details={"request": {"source": "tool_input"}},
                ),
                timestamp_epoch=100.1,
            ),
            _Candidate(
                _event(
                    event_id="pii-tool-output",
                    category="pii_scan",
                    details={"request": {"source": "tool_output"}},
                ),
                timestamp_epoch=100.5,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(hook="after_tool_call", observed_at_epoch=100.0)
    )

    assert [item.event.event_id for item in result] == ["pii-tool-output"]


def test_tool_call_exact_mode_falls_back_to_pii_hash_when_source_is_missing() -> None:
    tool_input = '{"content":"用户邮箱 alice@example.com"}'
    text_sha256 = hashlib.sha256(tool_input.encode("utf-8")).hexdigest()
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="pii-wrong-hash",
                    category="pii_scan",
                    details={"request": {"text_sha256": "0" * 64}},
                ),
                timestamp_epoch=100.1,
            ),
            _Candidate(
                _event(
                    event_id="pii-matching-hash",
                    category="pii_scan",
                    details={"request": {"text_sha256": text_sha256}},
                ),
                timestamp_epoch=100.5,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            hook="before_tool_call",
            observed_at_epoch=100.0,
            metrics={"pii_scan_input_sha256": text_sha256},
        )
    )

    assert [item.event.event_id for item in result] == ["pii-matching-hash"]


def test_batch_exact_mode_queries_tool_call_ids_once_without_changing_results() -> None:
    reader = _FakeReader(
        [
            _Candidate(
                _event(event_id="code-tool-1", category="code_scan"),
                timestamp_epoch=100.5,
            ),
            _Candidate(
                _event(
                    event_id="skill-tool-2",
                    category="skill_ledger",
                    tool_call_id="tool-2",
                ),
                timestamp_epoch=101.0,
            ),
            _Candidate(
                _event(
                    event_id="wrong-tool",
                    category="code_scan",
                    tool_call_id="tool-3",
                ),
                timestamp_epoch=99.0,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated_many(
        [
            _record(tool_call_id="tool-1", observed_at_epoch=100.0),
            _record(tool_call_id="tool-2", observed_at_epoch=100.0),
        ]
    )

    assert reader.calls == [
        {
            "session_id": "session-1",
            "categories": ("code_scan", "skill_ledger", "pii_scan"),
            "run_id": "run-1",
            "tool_call_id": None,
            "tool_call_ids": ("tool-1", "tool-2"),
            "since_epoch": None,
            "until_epoch": None,
        }
    ]
    assert [[item.event.event_id for item in row] for row in result] == [
        ["code-tool-1"],
        ["skill-tool-2"],
    ]
    assert [[item.match_reason for item in row] for row in result] == [
        ["tool_call_id"],
        ["tool_call_id"],
    ]


def test_batch_mode_matches_single_record_results_for_supported_modes() -> None:
    candidates = [
        _Candidate(
            _event(event_id="exact-code", category="code_scan"),
            timestamp_epoch=100.5,
        ),
        _Candidate(
            _event(
                event_id="run-prompt",
                category="prompt_scan",
                details={"request": {"text": "review"}},
            ),
            timestamp_epoch=110.0,
        ),
        _Candidate(
            _event(
                event_id="fallback-code",
                category="code_scan",
                tool_call_id=None,
                details={"request": {"code": "rm -rf testfolder"}},
            ),
            timestamp_epoch=200.1,
        ),
    ]
    records = [
        _record(tool_call_id="tool-1", observed_at_epoch=100.0),
        _record(
            hook="before_agent_run",
            tool_call_id=None,
            observed_at_epoch=110.0,
        ),
        _record(
            tool_call_id=None,
            observed_at_epoch=200.0,
            metrics={"parameters": {"command": "rm -rf testfolder"}},
        ),
        _record(hook="after_tool_call", observed_at_epoch=210.0),
    ]

    single_results = [
        SecurityCorrelationService(_FakeReader(candidates)).find_correlated(record)
        for record in records
    ]
    batch_results = SecurityCorrelationService(
        _FakeReader(candidates)
    ).find_correlated_many(records)

    assert _result_signatures(batch_results) == _result_signatures(single_results)


def test_exact_mode_rejects_candidates_with_missing_security_correlation_fields() -> (
    None
):
    reader = _FakeReader(
        [
            _Candidate(
                _event(event_id="missing-session", category="code_scan", session_id=""),
                timestamp_epoch=100.0,
            ),
            _Candidate(
                _event(event_id="missing-run", category="code_scan", run_id=None),
                timestamp_epoch=100.0,
            ),
            _Candidate(
                _event(
                    event_id="missing-tool", category="skill_ledger", tool_call_id=""
                ),
                timestamp_epoch=100.0,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(_record(observed_at_epoch=100.0))

    assert result == []


@pytest.mark.parametrize("category", ["sandbox", "hardening", "asset_verify"])
@pytest.mark.parametrize("mode", ["exact", "fallback"])
def test_categories_outside_security_mapping_are_filtered(
    mode: str, category: str
) -> None:
    reader = _FakeReader(
        [
            _Candidate(
                _event(event_id=f"{mode}-{category}", category=category),
                timestamp_epoch=100.0,
            )
        ]
    )
    service = SecurityCorrelationService(reader)
    record = (
        _record(observed_at_epoch=100.0)
        if mode == "exact"
        else _record(tool_call_id=None, observed_at_epoch=100.0)
    )

    result = service.find_correlated(record)

    assert result == []


def test_before_agent_run_uses_run_id_match_without_time_window() -> None:
    reader = _FakeReader(
        [
            _Candidate(
                _event(event_id="prompt-slow", category="prompt_scan"),
                timestamp_epoch=106.0,
            ),
            _Candidate(
                _event(event_id="pii-near", category="pii_scan"),
                timestamp_epoch=101.0,
            ),
            _Candidate(
                _event(
                    event_id="prompt-wrong-run",
                    category="prompt_scan",
                    run_id="run-2",
                ),
                timestamp_epoch=100.5,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            hook="before_agent_run",
            tool_call_id=None,
            observed_at_epoch=100.0,
        )
    )

    assert reader.calls == [
        {
            "session_id": "session-1",
            "categories": ("prompt_scan", "pii_scan"),
            "run_id": "run-1",
            "tool_call_id": None,
            "since_epoch": None,
            "until_epoch": None,
        }
    ]
    assert [item.event.event_id for item in result] == ["prompt-slow", "pii-near"]
    assert [item.match_reason for item in result] == ["run_id", "run_id"]
    assert [item.time_delta_seconds for item in result] == [6.0, 1.0]


def test_fallback_mode_uses_session_time_and_prompt_field_match_priority() -> None:
    prompt = "Hermes added context\n删除testfolder文件夹"
    pii_text_sha256 = hashlib.sha256(prompt.encode("utf-8")).hexdigest()
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="prompt-suffix-closer",
                    category="prompt_scan",
                    session_id="session-1",
                    run_id=None,
                    tool_call_id=None,
                    details={"request": {"text": "删除testfolder文件夹"}},
                ),
                timestamp_epoch=100.1,
            ),
            _Candidate(
                _event(
                    event_id="prompt-exact-farther",
                    category="prompt_scan",
                    session_id="session-1",
                    run_id=None,
                    tool_call_id=None,
                    details={
                        "request": {
                            "text": "Hermes added context\n删除testfolder文件夹"
                        }
                    },
                ),
                timestamp_epoch=105.0,
            ),
            _Candidate(
                _event(
                    event_id="prompt-other-session",
                    category="prompt_scan",
                    session_id="session-2",
                    run_id=None,
                    tool_call_id=None,
                    details={
                        "request": {
                            "text": "Hermes added context\n删除testfolder文件夹"
                        }
                    },
                ),
                timestamp_epoch=100.0,
            ),
            _Candidate(
                _event(
                    event_id="pii-boundary",
                    category="pii_scan",
                    session_id="session-1",
                    run_id=None,
                    tool_call_id=None,
                    details={"request": {"text_sha256": pii_text_sha256}},
                ),
                timestamp_epoch=110.0,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            hook="before_agent_run",
            run_id=ZERO_RUN_ID,
            tool_call_id=None,
            observed_at_epoch=100.0,
            metrics={"prompt": prompt},
        )
    )

    assert reader.calls == [
        {
            "session_id": "session-1",
            "categories": ("prompt_scan", "pii_scan"),
            "run_id": None,
            "tool_call_id": None,
            "since_epoch": 90.0,
            "until_epoch": 110.0,
        }
    ]
    assert [item.event.event_id for item in result] == [
        "prompt-exact-farther",
        "pii-boundary",
    ]
    assert [item.match_reason for item in result] == ["field+time", "field+time"]
    assert [item.match_rank for item in result] == [0, 0]
    assert [item.time_delta_seconds for item in result] == [5.0, 10.0]


def test_fallback_mode_rejects_time_only_candidates_without_field_match() -> None:
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="prompt-time-only",
                    category="prompt_scan",
                    run_id=None,
                    tool_call_id=None,
                    details={"request": {"text": "unrelated prompt"}},
                ),
                timestamp_epoch=100.5,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            hook="before_agent_run",
            run_id=ZERO_RUN_ID,
            tool_call_id=None,
            observed_at_epoch=100.0,
            metrics={"prompt": "删除testfolder文件夹"},
        )
    )

    assert result == []


def test_fallback_mode_does_not_match_pii_scan_by_raw_text_prefix_or_suffix() -> None:
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="pii-raw-suffix",
                    category="pii_scan",
                    run_id=None,
                    tool_call_id=None,
                    details={"request": {"text": "alice@example.com"}},
                ),
                timestamp_epoch=100.1,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            hook="before_agent_run",
            run_id=ZERO_RUN_ID,
            tool_call_id=None,
            observed_at_epoch=100.0,
            metrics={"prompt": "Hermes added context\nalice@example.com"},
        )
    )

    assert result == []


def test_fallback_mode_matches_pii_scan_by_request_text_hash() -> None:
    prompt = "联系我 alice@example.com"
    text_sha256 = hashlib.sha256(prompt.encode("utf-8")).hexdigest()
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="pii-hash",
                    category="pii_scan",
                    run_id=None,
                    tool_call_id=None,
                    details={"request": {"text_sha256": text_sha256}},
                ),
                timestamp_epoch=99.5,
            ),
            _Candidate(
                _event(
                    event_id="prompt-without-request-text",
                    category="prompt_scan",
                    run_id=None,
                    tool_call_id=None,
                    details={"request": {"source": "user_input"}},
                ),
                timestamp_epoch=99.0,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            hook="before_agent_run",
            run_id=ZERO_RUN_ID,
            tool_call_id=None,
            observed_at_epoch=100.0,
            metrics={"prompt": prompt},
        )
    )

    assert [item.event.event_id for item in result] == ["pii-hash"]
    assert result[0].match_reason == "field+time"


def test_before_agent_run_fallback_matches_pii_scan_by_input_hash() -> None:
    prompt = "联系我 alice@example.com"
    text_sha256 = hashlib.sha256(prompt.encode("utf-8")).hexdigest()
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="before-agent-pii-hash",
                    category="pii_scan",
                    run_id=None,
                    tool_call_id=None,
                    details={"request": {"text_sha256": text_sha256}},
                ),
                timestamp_epoch=99.5,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            hook="before_agent_run",
            run_id=ZERO_RUN_ID,
            tool_call_id=None,
            observed_at_epoch=100.0,
            metrics={"pii_scan_input_sha256": text_sha256},
        )
    )

    assert [item.event.event_id for item in result] == ["before-agent-pii-hash"]
    assert result[0].match_reason == "field+time"


def test_after_tool_call_fallback_matches_pii_scan_by_input_hash() -> None:
    tool_output = "用户邮箱 alice@example.com"
    text_sha256 = hashlib.sha256(tool_output.encode("utf-8")).hexdigest()
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="tool-output-pii",
                    category="pii_scan",
                    run_id=None,
                    tool_call_id=None,
                    details={"request": {"text_sha256": text_sha256}},
                ),
                timestamp_epoch=100.5,
            )
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            hook="after_tool_call",
            run_id=ZERO_RUN_ID,
            tool_call_id=None,
            observed_at_epoch=100.0,
            metrics={"pii_scan_input_sha256": text_sha256},
        )
    )

    assert [item.event.event_id for item in result] == ["tool-output-pii"]
    assert result[0].match_reason == "field+time"


def test_before_tool_call_fallback_matches_pii_scan_by_input_hash() -> None:
    tool_input = '{"content":"用户邮箱 alice@example.com"}'
    text_sha256 = hashlib.sha256(tool_input.encode("utf-8")).hexdigest()
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="tool-input-pii",
                    category="pii_scan",
                    run_id=None,
                    tool_call_id="tool-1",
                    details={"request": {"text_sha256": text_sha256}},
                ),
                timestamp_epoch=100.5,
            )
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            hook="before_tool_call",
            run_id=ZERO_RUN_ID,
            tool_call_id="tool-1",
            observed_at_epoch=100.0,
            metrics={"pii_scan_input_sha256": text_sha256},
        )
    )

    assert [item.event.event_id for item in result] == ["tool-input-pii"]
    assert result[0].match_reason == "field+time"


@pytest.mark.parametrize("run_id", [None, ""])
def test_fallback_mode_uses_session_only_when_run_id_is_missing(
    run_id: str | None,
) -> None:
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="cross-run-match",
                    category="code_scan",
                    run_id="security-run",
                    tool_call_id=None,
                    details={"request": {"code": "rm -rf testfolder"}},
                ),
                timestamp_epoch=100.5,
            )
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            run_id=run_id,
            tool_call_id=None,
            observed_at_epoch=100.0,
            metrics={"parameters": {"command": "rm -rf testfolder"}},
        )
    )

    assert reader.calls == [
        {
            "session_id": "session-1",
            "categories": ("code_scan", "skill_ledger", "pii_scan"),
            "run_id": None,
            "tool_call_id": None,
            "since_epoch": 90.0,
            "until_epoch": 110.0,
        }
    ]
    assert [item.event.event_id for item in result] == ["cross-run-match"]
    assert result[0].event.run_id == "security-run"
    assert result[0].match_reason == "field+time"


def test_fallback_mode_matches_tool_call_fields_by_suffix_then_prefix() -> None:
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="code-prefix-closer",
                    category="code_scan",
                    run_id=None,
                    tool_call_id=None,
                    details={"request": {"code": "cd /repo"}},
                ),
                timestamp_epoch=100.1,
            ),
            _Candidate(
                _event(
                    event_id="code-suffix-farther",
                    category="code_scan",
                    run_id=None,
                    tool_call_id=None,
                    details={"request": {"code": "rm -rf testfolder"}},
                ),
                timestamp_epoch=101.0,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            run_id=None,
            tool_call_id=None,
            observed_at_epoch=100.0,
            metrics={"parameters": {"command": "cd /repo && rm -rf testfolder"}},
        )
    )

    assert [item.event.event_id for item in result] == ["code-suffix-farther"]


def test_fallback_mode_filters_candidates_outside_time_window_from_defensive_reader() -> (
    None
):
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="inside",
                    category="code_scan",
                    details={"request": {"code": "inside"}},
                ),
                timestamp_epoch=101.9,
            ),
            _Candidate(
                _event(
                    event_id="outside",
                    category="code_scan",
                    details={"request": {"code": "inside"}},
                ),
                timestamp_epoch=110.1,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            tool_call_id=None,
            observed_at_epoch=100.0,
            metrics={"parameters": {"command": "inside"}},
        )
    )

    assert reader.calls[0]["run_id"] == "run-1"
    assert [item.event.event_id for item in result] == ["inside"]


def test_batch_fallback_keeps_long_run_time_windows_bounded() -> None:
    reader = _FakeReader()
    service = SecurityCorrelationService(reader)

    result = service.find_correlated_many(
        [
            _record(
                tool_call_id=None,
                observed_at_epoch=10.0,
                metrics={"parameters": {"command": "first"}},
            ),
            _record(
                tool_call_id=None,
                observed_at_epoch=3500.0,
                metrics={"parameters": {"command": "last"}},
            ),
        ]
    )

    assert result == [[], []]
    assert reader.calls == [
        {
            "session_id": "session-1",
            "categories": ("code_scan", "skill_ledger", "pii_scan"),
            "run_id": "run-1",
            "tool_call_id": None,
            "since_epoch": 0.0,
            "until_epoch": 20.0,
        },
        {
            "session_id": "session-1",
            "categories": ("code_scan", "skill_ledger", "pii_scan"),
            "run_id": "run-1",
            "tool_call_id": None,
            "since_epoch": 3490.0,
            "until_epoch": 3510.0,
        },
    ]


def test_fallback_mode_skips_skill_ledger_even_when_basename_would_match() -> None:
    """skill_ledger has no reliable field-level correlation outside exact mode.

    obs records carry the unresolved skill name; events carry the resolved
    absolute skill_dir. The two are at different abstraction layers, so the
    fallback path must not guess via basename/suffix matching.
    """
    reader = _FakeReader(
        [
            _Candidate(
                _event(
                    event_id="skill-basename-match",
                    category="skill_ledger",
                    run_id=None,
                    tool_call_id=None,
                    details={"request": {"skill_dir": "/abs/skills/devops/pass-skill"}},
                ),
                timestamp_epoch=100.5,
            ),
        ]
    )
    service = SecurityCorrelationService(reader)

    result = service.find_correlated(
        _record(
            run_id=None,
            tool_call_id=None,
            observed_at_epoch=100.0,
            metrics={
                "tool_name": "skill_view",
                "parameters": {"name": "pass-skill"},
            },
        )
    )

    assert result == []
