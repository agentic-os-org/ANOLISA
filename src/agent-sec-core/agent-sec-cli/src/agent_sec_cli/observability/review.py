"""Textual TUI for drilling down into recorded observability events.

Stack-style drill-down: SessionList → TurnList → EventList → EventDetail.
Enter (or row click) drills in via DataTable's RowSelected event. Esc / q calls
each screen's ``action_back``: non-root screens pop, the root SessionListScreen
exits the app (so the user never lands on Textual's blank default screen). The
reader is injected by the CLI entry and closed by it (try/finally), so this
module never owns the reader's lifecycle.
"""

import json
import logging
from datetime import datetime, timezone
from typing import Any, Protocol

from agent_sec_cli.observability.correlation import (
    CorrelatedSecurityEvent,
    ObservabilityRecordFields,
    SecurityCorrelationService,
)
from agent_sec_cli.observability.models import ObservabilityEventRecord
from agent_sec_cli.observability.repositories import RunSummary, SessionSummary
from agent_sec_cli.observability.sqlite_reader import ObservabilityReader
from rich.markup import escape
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import VerticalScroll
from textual.screen import Screen
from textual.widgets import DataTable, Footer, Header, Static

logger = logging.getLogger(__name__)


class _SecurityCorrelation(Protocol):
    def find_correlated(
        self, record: ObservabilityRecordFields
    ) -> list[CorrelatedSecurityEvent]:
        pass


def _format_epoch(epoch: float) -> str:
    """Render a Unix epoch (stored as UTC) in the user's local timezone.

    Storage convention: SQLite holds UTC; UI shows local time.
    """
    return (
        datetime.fromtimestamp(epoch, tz=timezone.utc)
        .astimezone()
        .strftime("%Y-%m-%d %H:%M:%S %Z")
    )


def _truncate(value: str, width: int) -> str:
    if len(value) <= width:
        return value
    return value[: max(width - 1, 0)] + "…"


class _ListScreenBase(Screen):
    """Common shape for list screens: empty-state placeholder + DataTable.

    Drill-in is wired through Textual's ``DataTable.RowSelected`` event (DataTable
    consumes the Enter key internally and emits this message). Back navigation
    routes through ``action_back`` so the root screen can override it to quit.
    """

    BINDINGS = [
        Binding("escape", "back", "Back", show=True),
        Binding("q", "back", "Back", show=False),
    ]

    _empty_message: str = "No items."

    def compose(self) -> ComposeResult:
        yield Header()
        yield Static("", id="empty")
        yield DataTable(zebra_stripes=True, cursor_type="row")
        yield Footer()

    def on_mount(self) -> None:
        rows = list(self._load_rows())
        empty = self.query_one("#empty", Static)
        table = self.query_one(DataTable)
        if not rows:
            empty.update(self._empty_message)
            table.display = False
            return

        empty.display = False
        table.add_columns(*self._columns())
        for row in rows:
            table.add_row(*self._row_values(row), key=self._row_key(row))
        table.focus()

    def on_data_table_row_selected(self, event: DataTable.RowSelected) -> None:
        """Drill on Enter / row click. ``event.row_key.value`` is what we passed
        to ``add_row(..., key=...)``."""
        key = event.row_key.value
        if key is None:
            return
        self._drill(key)

    def action_back(self) -> None:
        """Default back behavior: pop one screen. ``SessionListScreen`` (the
        root) overrides this to exit the app, so popping the only mounted
        screen never strands the user on Textual's blank default screen."""
        self.app.pop_screen()

    # --- subclass hooks -------------------------------------------------------

    def _columns(self) -> tuple[str, ...]:
        raise NotImplementedError

    def _load_rows(self) -> list[Any]:
        raise NotImplementedError

    def _row_values(self, row: object) -> tuple[str, ...]:  # noqa: ARG002
        raise NotImplementedError

    def _row_key(self, row: object) -> str:  # noqa: ARG002
        raise NotImplementedError

    def _drill(self, key: str) -> None:  # noqa: ARG002
        raise NotImplementedError


class SessionListScreen(_ListScreenBase):
    """Top-level: one row per session_id, ordered by most recent activity."""

    _empty_message = "No observability records found."

    def _columns(self) -> tuple[str, ...]:
        return ("Last seen", "Session", "Turns", "Events")

    def _load_rows(self) -> list[SessionSummary]:
        return self.app.reader.list_sessions()  # type: ignore[attr-defined]

    def _row_values(self, row: SessionSummary) -> tuple[str, ...]:  # type: ignore[override]
        return (
            _format_epoch(row.last_seen_epoch),
            _truncate(row.session_id, 40),
            str(row.turn_count),
            str(row.event_count),
        )

    def _row_key(self, row: SessionSummary) -> str:  # type: ignore[override]
        return row.session_id

    def _drill(self, key: str) -> None:
        self.app.push_screen(TurnListScreen(session_id=key))

    def action_back(self) -> None:
        # Root screen: Esc / q quit the app (rather than popping into Textual's
        # implicit blank default screen).
        self.app.exit()


class TurnListScreen(_ListScreenBase):
    """Per-session: one row per run_id (one user turn)."""

    _empty_message = "No runs recorded for this session."

    def __init__(self, session_id: str) -> None:
        super().__init__()
        self._session_id = session_id

    def _columns(self) -> tuple[str, ...]:
        return ("Started", "Run", "Preview", "Events")

    def _load_rows(self) -> list[RunSummary]:
        return self.app.reader.list_runs(self._session_id)  # type: ignore[attr-defined]

    def _row_values(self, row: RunSummary) -> tuple[str, ...]:  # type: ignore[override]
        preview = row.user_input_preview or "(no user_input)"
        return (
            _format_epoch(row.started_at_epoch),
            _truncate(row.run_id, 36),
            _truncate(preview, 60),
            str(row.event_count),
        )

    def _row_key(self, row: RunSummary) -> str:  # type: ignore[override]
        return row.run_id

    def _drill(self, key: str) -> None:
        self.app.push_screen(EventListScreen(session_id=self._session_id, run_id=key))


class EventListScreen(_ListScreenBase):
    """Per-run: chronological timeline of hook events."""

    _empty_message = "No events for this run."

    def __init__(self, session_id: str, run_id: str) -> None:
        super().__init__()
        self._session_id = session_id
        self._run_id = run_id
        # Cache rows so action_drill can recover the full record by row key.
        self._rows_by_key: dict[str, ObservabilityEventRecord] = {}
        self._security_results_by_key: dict[str, str] = {}

    def _columns(self) -> tuple[str, ...]:
        return ("Time", "Hook", "Call / Tool", "Security Result")

    def _load_rows(self) -> list[ObservabilityEventRecord]:
        rows = self.app.reader.list_events(  # type: ignore[attr-defined]
            self._session_id, self._run_id
        )
        self._rows_by_key = {str(row.id): row for row in rows}
        security_correlation = getattr(self.app, "security_correlation", None)
        security_results = _find_correlated_security_events_many(
            rows,
            security_correlation,
        )
        self._security_results_by_key = {
            str(row.id): _format_security_result(security_results[index])
            for index, row in enumerate(rows)
        }
        return rows

    def _row_values(self, row: ObservabilityEventRecord) -> tuple[str, ...]:  # type: ignore[override]
        # Whichever id is present — call_id (model calls) or tool_call_id (tool calls).
        ident = row.tool_call_id or row.call_id or ""
        return (
            _format_epoch(row.observed_at_epoch),
            row.hook,
            _truncate(ident, 18),
            _truncate(self._security_results_by_key.get(str(row.id), "-"), 50),
        )

    def _row_key(self, row: ObservabilityEventRecord) -> str:  # type: ignore[override]
        return str(row.id)

    def _drill(self, key: str) -> None:
        record = self._rows_by_key.get(key)
        if record is None:
            return
        self.app.push_screen(
            EventDetailScreen(
                record=record,
                security_correlation=getattr(self.app, "security_correlation", None),
            )
        )


class EventDetailScreen(Screen):
    """Leaf screen: full pretty-printed metadata + metrics for one event."""

    BINDINGS = [
        Binding("escape", "app.pop_screen", "Back", show=True),
        Binding("q", "app.pop_screen", "Back", show=False),
    ]

    def __init__(
        self,
        record: ObservabilityEventRecord,
        security_correlation: _SecurityCorrelation | None = None,
    ) -> None:
        super().__init__()
        self._record = record
        self._security_correlation = security_correlation

    def compose(self) -> ComposeResult:
        security_events = self._correlated_security_events()
        yield Header()
        with VerticalScroll():
            yield Static(self._render_header(), markup=True)
            yield Static("\n[b]Metadata[/b]:", markup=True)
            yield Static(_safe_pretty_json(self._record.metadata_json), markup=False)
            yield Static("\n[b]Metrics[/b]:", markup=True)
            yield Static(_safe_pretty_json(self._record.metrics_json), markup=False)
            if security_events:
                yield Static("\n[b]Security Events[/b]:", markup=True)
                yield Static(_render_security_events(security_events), markup=False)
        yield Footer()

    def _render_header(self) -> str:
        # Renamed from _render() — Textual's Widget._render() is an internal
        # rendering hook that must return a Visual; overriding it with a str
        # breaks the renderer (AttributeError: 'str' has no 'render_strips').
        r = self._record
        # Display local time for scanning and a normalized UTC ISO timestamp for
        # traceability. The stored raw string may carry a non-UTC offset.
        observed_local = _format_epoch(r.observed_at_epoch)
        observed_utc = datetime.fromtimestamp(
            r.observed_at_epoch, tz=timezone.utc
        ).isoformat()
        header_lines = [
            f"[b]Hook[/b]:        {escape(r.hook)}",
            (
                f"[b]Observed at[/b]: {escape(observed_local)}  "
                f"([dim]{escape(observed_utc)}[/dim])"
            ),
            f"[b]Session[/b]:     {escape(r.session_id)}",
            f"[b]Run[/b]:         {escape(r.run_id)}",
        ]
        if r.call_id:
            header_lines.append(f"[b]Call ID[/b]:     {escape(r.call_id)}")
        if r.tool_call_id:
            header_lines.append(f"[b]Tool call[/b]:   {escape(r.tool_call_id)}")

        return "\n".join(header_lines)

    def _correlated_security_events(self) -> list[CorrelatedSecurityEvent]:
        return _find_correlated_security_events(
            self._record,
            self._security_correlation,
        )


class ObservabilityReviewApp(App):
    """Drill-down TUI over recorded observability events."""

    BINDINGS = [Binding("q", "quit", "Quit", show=True)]
    TITLE = "agent-sec-cli observability review"

    def __init__(
        self,
        reader: ObservabilityReader,
        security_correlation: SecurityCorrelationService | None = None,
    ) -> None:
        super().__init__()
        # Reader is owned by the CLI entry — App must not close it.
        self.reader = reader
        self.security_correlation = security_correlation

    def on_mount(self) -> None:
        self.push_screen(SessionListScreen())


def _summarize_metrics(hook: str, metrics_json: str) -> str:
    """One-line gist of an event for the timeline view."""
    try:
        metrics = json.loads(metrics_json)
    except (ValueError, TypeError):
        return "(unparseable metrics)"
    if not isinstance(metrics, dict):
        return "(non-object metrics)"

    if hook == "before_agent_run":
        return str(metrics.get("user_input") or metrics.get("prompt") or "")
    if hook == "before_llm_call":
        model = metrics.get("model_id") or metrics.get("model_provider") or ""
        return f"model={model}"
    if hook == "after_llm_call":
        latency = metrics.get("latency_ms")
        outcome = metrics.get("outcome") or metrics.get("stop_reason") or ""
        return f"latency={latency}ms {outcome}".strip()
    if hook == "before_tool_call":
        return f"tool={metrics.get('tool_name', '')}"
    if hook == "after_tool_call":
        status = metrics.get("status") or (
            "ok" if metrics.get("error") is None else "err"
        )
        duration = metrics.get("duration_ms")
        return f"status={status} duration={duration}ms"
    if hook == "after_agent_run":
        success = metrics.get("success")
        duration = metrics.get("duration_ms")
        return f"success={success} duration={duration}ms"
    return ""


def _safe_pretty_json(raw: str) -> str:
    """Pretty-print a JSON blob; fall back to a tagged escape if it's broken."""
    try:
        parsed = json.loads(raw)
    except (ValueError, TypeError):
        snippet = raw[:500]
        return f"Failed to parse JSON:\n{snippet}"
    return json.dumps(parsed, indent=2, ensure_ascii=False)


def _find_correlated_security_events(
    record: ObservabilityEventRecord,
    security_correlation: _SecurityCorrelation | None,
) -> list[CorrelatedSecurityEvent]:
    if security_correlation is None:
        return []
    try:
        return security_correlation.find_correlated(_record_fields(record))
    except Exception as exc:  # noqa: BLE001 - reviewer must not crash on backend faults
        logger.warning(
            "security correlation lookup failed",
            extra={
                "session_id": record.session_id,
                "run_id": record.run_id,
                "data": {"error_type": type(exc).__name__},
            },
        )
        return []


def _find_correlated_security_events_many(
    records: list[ObservabilityEventRecord],
    security_correlation: _SecurityCorrelation | None,
) -> list[list[CorrelatedSecurityEvent]]:
    if security_correlation is None:
        return [[] for _ in records]

    find_many = getattr(security_correlation, "find_correlated_many", None)
    if callable(find_many):
        try:
            results = find_many([_record_fields(record) for record in records])
        except Exception:
            return [[] for _ in records]
        if len(results) == len(records):
            return [list(result) for result in results]

    return [
        _find_correlated_security_events(record, security_correlation)
        for record in records
    ]


def _record_fields(record: ObservabilityEventRecord) -> ObservabilityRecordFields:
    return ObservabilityRecordFields(
        hook=record.hook,
        session_id=record.session_id,
        run_id=record.run_id,
        tool_call_id=record.tool_call_id,
        observed_at_epoch=record.observed_at_epoch,
        metrics=_safe_metrics_dict(record.metrics_json),
    )


def _safe_metrics_dict(raw: str) -> dict[str, Any] | None:
    try:
        parsed = json.loads(raw)
    except (ValueError, TypeError):
        return None
    if not isinstance(parsed, dict):
        return None
    return parsed


def _format_security_result(events: list[CorrelatedSecurityEvent]) -> str:
    if not events:
        return "-"
    return ", ".join(
        f"{correlated.event.category}:{_security_result_value(correlated)}"
        for correlated in events
    )


def _security_result_value(correlated: CorrelatedSecurityEvent) -> str:
    event = correlated.event
    result = _value_from_result_object(event.details.get("result"))
    if result is not None:
        return result

    result = _value_from_result_object(event.details)
    if result is not None:
        return result

    return event.result


def _value_from_result_object(value: Any) -> str | None:
    if not isinstance(value, dict):
        return None

    for key in ("verdict", "status"):
        result_value = value.get(key)
        if result_value is not None and result_value != "":
            return str(result_value)

    valid = value.get("valid")
    if valid is True:
        return "pass"
    if valid is False:
        return "fail"
    if valid is not None and valid != "":
        return str(valid)
    return None


def _render_security_events(events: list[CorrelatedSecurityEvent]) -> str:
    lines: list[str] = []
    for index, correlated in enumerate(events, start=1):
        event = correlated.event
        if index > 1:
            lines.append("")
        lines.append(
            f"{index}. {event.category} / {event.event_type} result={event.result}"
        )
        lines.append(
            "   "
            f"match={correlated.match_reason} "
            f"delta={correlated.time_delta_seconds:+.3f}s "
            f"security_at={_format_epoch(correlated.security_timestamp_epoch)}"
        )
        lines.append("   details:")
        detail_lines = json.dumps(
            event.details,
            indent=2,
            ensure_ascii=False,
        ).splitlines()
        lines.extend(f"     {line}" for line in detail_lines)
    return "\n".join(lines)


__all__ = [
    "EventDetailScreen",
    "EventListScreen",
    "ObservabilityReviewApp",
    "SessionListScreen",
    "TurnListScreen",
]
