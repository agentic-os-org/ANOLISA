"""Unit tests for caller-provided trace correlation context."""

from concurrent.futures import ThreadPoolExecutor

import pytest
from agent_sec_cli.correlation_context import (
    MAX_CORRELATION_ID_LENGTH,
    TRUNCATED_CORRELATION_ID_SUFFIX,
    TraceContext,
    clear_invocation_context_for_tests,
    clear_process_trace_context,
    get_current_trace_context,
    get_invocation_id,
    init_invocation_context,
    init_process_trace_context,
    parse_trace_context,
    parse_trace_context_payload,
    reset_current_trace_context,
    set_current_trace_context,
    trace_context_to_payload,
)


def test_parse_trace_context_accepts_snake_case_json():
    ctx = parse_trace_context(
        '{"trace_id":"trace-1","session_id":"session-1","run_id":"run-1","call_id":"call-1","tool_call_id":"tool-1","agent_name":"hermes"}'
    )

    assert ctx == TraceContext(
        trace_id="trace-1",
        session_id="session-1",
        run_id="run-1",
        call_id="call-1",
        tool_call_id="tool-1",
        agent_name="hermes",
    )


def test_parse_trace_context_accepts_camel_case_json():
    ctx = parse_trace_context(
        '{"traceId":"trace-1","sessionId":"session-1","runId":"run-1","callId":"call-1","toolCallId":"tool-1","agentName":"openclaw"}'
    )

    assert ctx == TraceContext(
        trace_id="trace-1",
        session_id="session-1",
        run_id="run-1",
        call_id="call-1",
        tool_call_id="tool-1",
        agent_name="openclaw",
    )


def test_parse_trace_context_prefers_snake_case_when_both_are_present():
    ctx = parse_trace_context(
        '{"sessionId":"camel-session","session_id":"snake-session","runId":"camel-run","run_id":"snake-run","agentName":"camel-agent","agent_name":"snake-agent"}'
    )

    assert ctx.session_id == "snake-session"
    assert ctx.run_id == "snake-run"
    assert ctx.agent_name == "snake-agent"


def test_parse_trace_context_ignores_unknown_empty_and_non_string_values():
    ctx = parse_trace_context(
        '{"session_id":"","run_id":42,"call_id":"call-1","agent_name":false,"agentName":"","unknown":"ignored"}'
    )

    assert ctx == TraceContext(call_id="call-1")


def test_parse_trace_context_ignores_whitespace_only_values_and_strips_values():
    ctx = parse_trace_context(
        '{"session_id":"   ","run_id":" run-1 ","call_id":"call-1","agent_name":" hermes "}'
    )

    assert ctx == TraceContext(run_id="run-1", call_id="call-1", agent_name="hermes")


def test_parse_trace_context_truncates_long_values_with_suffix():
    long_session_id = "s" * (MAX_CORRELATION_ID_LENGTH + 10)

    ctx = parse_trace_context(f'{{"session_id":"{long_session_id}"}}')

    assert ctx == TraceContext(
        session_id=(
            "s" * (MAX_CORRELATION_ID_LENGTH - len(TRUNCATED_CORRELATION_ID_SUFFIX))
            + TRUNCATED_CORRELATION_ID_SUFFIX
        )
    )
    assert len(ctx.session_id or "") == MAX_CORRELATION_ID_LENGTH


def test_parse_trace_context_rejects_invalid_json():
    with pytest.raises(ValueError, match="invalid trace context JSON"):
        parse_trace_context("not-json")


def test_parse_trace_context_rejects_non_object_json():
    with pytest.raises(ValueError, match="trace context must be a JSON object"):
        parse_trace_context("[]")


def test_parse_trace_context_does_not_use_env_session_as_fallback(monkeypatch):
    monkeypatch.setenv("AGENT_SEC_SESSION_ID", "env-session")

    ctx = parse_trace_context('{"run_id":"run-1"}')

    assert ctx == TraceContext(run_id="run-1")


def test_parse_trace_context_ignores_env_session_when_json_session_exists(monkeypatch):
    monkeypatch.setenv("AGENT_SEC_SESSION_ID", "env-session")

    ctx = parse_trace_context('{"session_id":"json-session"}')

    assert ctx == TraceContext(session_id="json-session")


def test_parse_trace_context_does_not_log_env_session_conflicts(
    monkeypatch,
    caplog,
):
    monkeypatch.setenv("AGENT_SEC_SESSION_ID", "env-session")

    ctx = parse_trace_context('{"session_id":"json-session"}')

    assert ctx == TraceContext(session_id="json-session")
    assert "AGENT_SEC_SESSION_ID" not in caplog.text


def test_parse_trace_context_payload_accepts_structured_mapping():
    ctx = parse_trace_context_payload(
        {
            "traceId": "trace-1",
            "session_id": "session-1",
            "run_id": " run-1 ",
            "toolCallId": "tool-1",
            "agentName": "cosh",
            "ignored": "value",
        }
    )

    assert ctx == TraceContext(
        trace_id="trace-1",
        session_id="session-1",
        run_id="run-1",
        tool_call_id="tool-1",
        agent_name="cosh",
    )


def test_trace_context_to_payload_emits_agent_name() -> None:
    assert trace_context_to_payload(
        TraceContext(
            trace_id="trace-1",
            session_id="session-1",
            agent_name=" hermes ",
        )
    ) == {
        "trace_id": "trace-1",
        "session_id": "session-1",
        "agent_name": "hermes",
    }


def test_process_trace_context_is_visible_from_worker_threads():
    clear_process_trace_context()
    init_process_trace_context(TraceContext(session_id="session-1", run_id="run-1"))

    try:
        with ThreadPoolExecutor(max_workers=1) as executor:
            ctx = executor.submit(get_current_trace_context).result()
    finally:
        clear_process_trace_context()

    assert ctx == TraceContext(session_id="session-1", run_id="run-1")


def test_contextvar_override_takes_precedence_over_process_context():
    clear_process_trace_context()
    init_process_trace_context(TraceContext(session_id="process-session"))
    token = set_current_trace_context(TraceContext(session_id="request-session"))

    try:
        assert get_current_trace_context() == TraceContext(session_id="request-session")
    finally:
        reset_current_trace_context(token)
        clear_process_trace_context()


def test_contextvar_none_override_can_clear_process_context_temporarily():
    clear_process_trace_context()
    init_process_trace_context(TraceContext(session_id="process-session"))
    token = set_current_trace_context(None)

    try:
        assert get_current_trace_context() is None
    finally:
        reset_current_trace_context(token)
        clear_process_trace_context()


def test_invocation_context_uses_env_value(monkeypatch):
    clear_invocation_context_for_tests()
    monkeypatch.setenv("AGENT_SEC_INVOCATION_ID", "caller-invocation")

    try:
        init_invocation_context()

        assert get_invocation_id() == "caller-invocation"
    finally:
        clear_invocation_context_for_tests()


def test_invocation_context_strips_env_whitespace(monkeypatch):
    clear_invocation_context_for_tests()
    monkeypatch.setenv("AGENT_SEC_INVOCATION_ID", "  caller-invocation  ")

    try:
        init_invocation_context()

        assert get_invocation_id() == "caller-invocation"
    finally:
        clear_invocation_context_for_tests()


def test_invocation_context_falls_back_when_env_is_whitespace_only(monkeypatch):
    clear_invocation_context_for_tests()
    monkeypatch.setenv("AGENT_SEC_INVOCATION_ID", "   ")

    try:
        init_invocation_context()

        invocation_id = get_invocation_id()
        # Whitespace-only env is treated as missing — UUID fallback kicks in.
        assert invocation_id and invocation_id.strip() == invocation_id
        assert len(invocation_id) == 36  # uuid4 canonical length
    finally:
        clear_invocation_context_for_tests()


def test_invocation_context_truncates_oversized_env_value(monkeypatch):
    clear_invocation_context_for_tests()
    oversized = "x" * (MAX_CORRELATION_ID_LENGTH + 10)
    monkeypatch.setenv("AGENT_SEC_INVOCATION_ID", oversized)

    try:
        init_invocation_context()

        invocation_id = get_invocation_id()
        assert len(invocation_id) == MAX_CORRELATION_ID_LENGTH
        assert invocation_id.endswith(TRUNCATED_CORRELATION_ID_SUFFIX)
    finally:
        clear_invocation_context_for_tests()


def test_generated_invocation_id_is_stable_and_visible_from_worker_threads():
    clear_invocation_context_for_tests()

    try:
        init_invocation_context()
        invocation_id = get_invocation_id()

        with ThreadPoolExecutor(max_workers=1) as executor:
            worker_invocation_id = executor.submit(get_invocation_id).result()
    finally:
        clear_invocation_context_for_tests()

    assert invocation_id
    assert worker_invocation_id == invocation_id


def test_concurrent_init_invocation_context_is_atomic(monkeypatch):
    """Without locking, two threads racing into ``init_invocation_context``
    would each generate a UUID and one would silently overwrite the other,
    so records emitted by the loser before the overwrite would carry an id
    that no longer matches the process-level value. See PR #651 review #8.
    """
    import threading

    clear_invocation_context_for_tests()
    monkeypatch.delenv("AGENT_SEC_INVOCATION_ID", raising=False)

    n_threads = 32
    barrier = threading.Barrier(n_threads)
    seen: list[str] = []
    seen_lock = threading.Lock()

    def _race() -> None:
        barrier.wait()
        init_invocation_context()
        observed = get_invocation_id()
        with seen_lock:
            seen.append(observed)

    threads = [threading.Thread(target=_race) for _ in range(n_threads)]
    try:
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()

        assert len(seen) == n_threads
        assert (
            len(set(seen)) == 1
        ), f"all racing threads must observe the same invocation id; got {set(seen)}"
    finally:
        clear_invocation_context_for_tests()
