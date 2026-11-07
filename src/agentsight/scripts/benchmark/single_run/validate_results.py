#!/usr/bin/env python3
"""Compare load-generator request IDs with SQLite records or FFI callback receipts."""

from __future__ import annotations

import argparse
import gzip
import json
import logging
import math
import os
import re
import sqlite3
import time
from collections.abc import Iterable
from contextlib import closing
from pathlib import Path
from typing import Any

Captured = dict[str, list[tuple[str, int | None]]]


CAPACITY_REJECTED_PATTERN = re.compile(
    r"^agentsight_http_conn_removed_total\{([^}]*)\}\s+(\S+)(?:\s+\S+)?$"
)


def _capacity_rejected_series(path: Path | None) -> dict[tuple[tuple[str, str], ...], int] | None:
    """Parse capacity rejection counters keyed by their full label set."""
    if path is None or not path.is_file():
        return None
    series: dict[tuple[tuple[str, str], ...], int] = {}
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return None
    for line in lines:
        match = CAPACITY_REJECTED_PATTERN.match(line.strip())
        if not match:
            continue
        labels = dict(re.findall(r'(\w+)="((?:\\.|[^"\\])*)"', match.group(1)))
        if labels.get("reason") != "capacity_rejected":
            continue
        try:
            value = float(match.group(2))
        except ValueError:
            return None
        if not value.is_integer() or value < 0:
            return None
        key = tuple(sorted(labels.items()))
        series[key] = series.get(key, 0) + int(value)
    return series


def load_capacity_rejected_events(
    path: Path | None, baseline: Path | None = None
) -> int | None:
    """Return capacity rejections observed since the pre-load baseline snapshot.

    Prometheus counters are cumulative for the AgentSight process. Without a
    baseline they include startup and pre-load traffic, so they cannot be
    attributed to the measured run.
    """
    current = _capacity_rejected_series(path)
    if not current:
        return None
    if baseline is None:
        logging.warning(
            "No capacity-rejection baseline snapshot; capacity_rejected_events is unavailable"
        )
        return None
    previous = _capacity_rejected_series(baseline)
    if previous is None:
        logging.warning("Cannot read capacity-rejection baseline: %s", baseline)
        return None
    total = 0
    for key, value in current.items():
        delta = value - previous.get(key, 0)
        if delta < 0:
            logging.warning(
                "Capacity rejection counter reset for %s; run attribution is unavailable",
                dict(key),
            )
            return None
        total += delta
    return total


def resolve_metrics_path(load_results: Path, metrics_file: Path | None) -> Path | None:
    """Resolve metrics using CLI, environment, then the run artifact directory."""
    if metrics_file is not None:
        return metrics_file
    configured = os.environ.get("AGENTSIGHT_METRICS_FILE")
    if configured:
        return Path(configured)
    candidate = load_results.parent / "internal-metrics.prom"
    return candidate if candidate.is_file() else None


def resolve_metrics_baseline(
    load_results: Path, baseline_file: Path | None
) -> Path | None:
    """Resolve the pre-load counter snapshot from the CLI or run artifacts."""
    if baseline_file is not None:
        return baseline_file
    candidate = load_results.parent / "internal-metrics-baseline.prom"
    return candidate if candidate.is_file() else None


def incomplete_reason(value: Any) -> str | None:
    """Return the abandonment reason recorded anywhere in a semantic event."""
    if isinstance(value, dict):
        metadata = value.get("metadata")
        if isinstance(metadata, dict):
            reason = metadata.get("abandon_reason")
            if reason:
                return str(reason)
            if metadata.get("incomplete") in (True, "true", "True", 1):
                return "incomplete"
        for child in value.values():
            found = incomplete_reason(child)
            if found:
                return found
    elif isinstance(value, list):
        for child in value:
            found = incomplete_reason(child)
            if found:
                return found
    return None


def capture_status_from_event(status: str, event: Any) -> str:
    """Tag explicit abandonment from an already-parsed semantic event."""
    if status in {"incomplete", "interrupted"}:
        return f"incomplete:{status}"
    if event is None:
        return status
    reason = incomplete_reason(event)
    if reason is None:
        return status
    if reason == "capacity_rejected":
        return "rejected:capacity_rejected"
    return f"incomplete:{reason}"


def capture_status(status: str, event_json: str | None) -> str:
    """Tag explicit abandonment without changing ordinary statuses."""
    if not event_json:
        return capture_status_from_event(status, None)
    try:
        event = json.loads(event_json)
    except (json.JSONDecodeError, TypeError):
        return capture_status_from_event(status, None)
    return capture_status_from_event(status, event)


def load_ffi_captured(
    path: Path, prefix: str = "", expected: set[str] | None = None, offset: int = 0
) -> tuple[Captured, int]:
    """Read complete callback receipts; retain an unfinished line for the next poll."""
    captured: Captured = {}
    if not path.exists():
        return captured, offset
    with path.open("rb") as handle:
        handle.seek(offset)
        while line := handle.readline():
            if not line.endswith(b"\n"):
                break
            offset = handle.tell()
            kind, payload = line.decode("utf-8").rstrip("\n").split("\t", 1)
            if kind == "T":
                request_id, status, total, usage = payload.split("\t")
                identifiers = {request_id}
                tokens = int(total) if usage == "1" else None
                complete = 200 <= int(status) < 400
                receipt_status = "complete" if complete else "error"
            elif kind == "J":
                call = json.loads(payload)
                identifiers = set(re.findall(r"\[agentsight-benchmark:([A-Za-z0-9_-]+)\]", payload))
                usage = call.get("token_usage") or {}
                tokens = usage.get("total_tokens")
                if type(tokens) is not int or tokens < 0:
                    tokens = None
                metadata = call.get("metadata")
                if not isinstance(metadata, dict):
                    metadata = {}
                request_id = metadata.get("request_id")
                if isinstance(request_id, str) and request_id:
                    identifiers.add(request_id)
                status = int(metadata.get("status_code", 0))
                complete = (
                    not call.get("error")
                    and metadata.get("incomplete") not in (True, "true")
                    and 200 <= status < 400
                )
                receipt_status = "complete" if complete else "error"
                # An explicit abandonment reason is terminal, so it can be
                # attributed to a request immediately. A bare incomplete flag
                # may still be superseded by a later complete receipt.
                if metadata.get("abandon_reason"):
                    receipt_status = capture_status_from_event(receipt_status, call)
            else:
                raise ValueError(f"unknown FFI callback receipt type: {kind}")
            for request_id in identifiers:
                if (not prefix or request_id.startswith(prefix)) and (
                    expected is None or request_id in expected
                ):
                    merge_capture(captured, request_id, receipt_status, tokens)
    return captured, offset


def json_lines(path: Path) -> Iterable[dict[str, Any]]:
    """Yield UTF-8 JSON objects, isolating malformed records in plain/gzip logs."""
    opener = gzip.open if path.suffix == ".gz" else Path.open
    skipped = 0
    # Decode each record separately: TextIOWrapper can reject an entire read
    # buffer when a later line contains a torn multibyte character.
    with opener(path, mode="rb") as handle:
        for line in handle:
            try:
                item = json.loads(line.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                skipped += 1
                continue
            if isinstance(item, dict):
                yield item
    if skipped:
        logging.warning("Skipped %d malformed JSONL records in %s", skipped, path)


def _coerce_optional_status(status: Any) -> int | None:
    """Coerce an optional k6 ``data.status`` value to an integer code.

    The status field is optional load-generator metadata: a list, object,
    non-numeric string or non-finite number contributes no success evidence
    instead of aborting the read of the remaining records.
    """
    try:
        return int(status)
    except (TypeError, ValueError, OverflowError):
        return None


def load_expected(path: Path) -> tuple[set[str], set[str]]:
    """Return request IDs from legacy k6 output containing per-request tags."""
    expected: set[str] = set()
    successful: set[str] = set()
    for item in json_lines(path):
        # `data` and its `tags` are optional load-generator metadata whose shape
        # is not guaranteed. A list, string or null used to raise
        # AttributeError from `.get` and abort the read, losing every later
        # record (and the whole comparison); a non-dict value contributes no
        # evidence instead.
        data = item.get("data")
        data = data if isinstance(data, dict) else {}
        tags = data.get("tags")
        tags = tags if isinstance(tags, dict) else {}
        request_id = item.get("request_id") or tags.get("request_id")
        if not request_id:
            continue
        request_id = str(request_id)
        expected.add(request_id)
        metric = item.get("metric")
        value = data.get("value")
        status = data.get("status")
        if metric == "benchmark_http_success" and value:
            successful.add(request_id)
        status_code = _coerce_optional_status(status)
        if status_code is not None and 200 <= status_code < 300:
            successful.add(request_id)
    return expected, successful


def load_request_log(path: Path, prefix: str = "") -> tuple[set[str], set[str]]:
    """Return accepted and successful IDs from the mock server request ledger."""
    expected: set[str] = set()
    successful: set[str] = set()
    if not path.exists():
        return expected, successful
    for item in json_lines(path):
        request_id = item.get("request_id")
        if not request_id:
            continue
        request_id = str(request_id)
        if prefix and not request_id.startswith(prefix):
            continue
        expected.add(request_id)
        status = item.get("http_status")
        if isinstance(status, int) and 200 <= status < 300:
            successful.add(request_id)
    return expected, successful


def table_columns(connection: sqlite3.Connection, table: str) -> set[str]:
    """Return columns for a table, allowing old AgentSight databases."""
    return {row[1] for row in connection.execute(f"PRAGMA table_info({table})")}


def load_captured(
    db_path: Path,
    prefix: str = "",
    expected: set[str] | None = None,
    since_ns: int | None = None,
) -> Captured:
    """Find captured IDs, narrowing indexed queries to the current run."""
    captured: dict[str, list[tuple[str, int | None]]] = {}
    uri = db_path.resolve().as_uri() + "?mode=ro"
    with closing(sqlite3.connect(uri, uri=True, timeout=0.5)) as connection:
        tables = {
            row[0]
            for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")
        }
        if "token_records" in tables:
            columns = table_columns(connection, "token_records")
            if "request_id" in columns:
                query = (
                    "SELECT request_id, input_tokens + output_tokens "
                    "FROM token_records WHERE request_id IS NOT NULL"
                )
                parameters: list[int] = []
                if since_ns is not None and "timestamp_ns" in columns:
                    query += " AND timestamp_ns >= ?"
                    parameters.append(since_ns)
                rows = connection.execute(query, parameters)
                for request_id, total in rows:
                    request_id = str(request_id)
                    if (not prefix or request_id.startswith(prefix)) and (
                        expected is None or request_id in expected
                    ):
                        captured.setdefault(request_id, []).append(("complete", int(total)))
        if "genai_events" in tables:
            columns = table_columns(connection, "genai_events")
            selected = [
                name
                for name in (
                    "call_id",
                    "trace_id",
                    "status",
                    "total_tokens",
                    "event_json",
                )
                if name in columns
            ]
            if selected:
                query = f"SELECT {', '.join(selected)} FROM genai_events"
                parameters = []
                if since_ns is not None and "start_timestamp_ns" in columns:
                    query += " WHERE start_timestamp_ns >= ?"
                    parameters.append(since_ns)
                rows = connection.execute(query, parameters)
                for row in rows:
                    record = dict(zip(selected, row))
                    identifiers = {
                        str(record[name]) for name in ("call_id", "trace_id") if record.get(name)
                    }
                    event: Any = None
                    if record.get("event_json"):
                        try:
                            event = json.loads(record["event_json"])
                            identifiers.update(find_request_ids(event))
                        except (json.JSONDecodeError, TypeError):
                            pass
                    status = capture_status_from_event(
                        str(record.get("status") or "complete"), event
                    )
                    for request_id in identifiers:
                        if (not prefix or request_id.startswith(prefix)) and (
                            expected is None or request_id in expected
                        ):
                            captured.setdefault(request_id, []).append(
                                (status, record.get("total_tokens"))
                            )
    return captured


def merge_capture(captured: Captured, request_id: str, status: str, total: int | None) -> None:
    """Retain each observed state once across incremental database polls."""
    observation = (status, total)
    observations = captured.setdefault(request_id, [])
    if observation not in observations:
        observations.append(observation)


def load_captured_incremental(
    db_path: Path,
    prefix: str,
    since_ns: int | None,
    genai_after_id: int,
    token_after_rowid: int,
    pending_genai_ids: set[int],
    expected_total_tokens: int | None = None,
) -> tuple[Captured, int, int, set[int]]:
    """Read newly inserted records and refresh GenAI rows still pending."""
    captured: Captured = {}
    next_genai_id = genai_after_id
    next_token_rowid = token_after_rowid
    next_pending_ids = set(pending_genai_ids)
    uri = db_path.resolve().as_uri() + "?mode=ro"
    with closing(sqlite3.connect(uri, uri=True, timeout=0.5)) as connection:
        tables = {
            row[0]
            for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")
        }
        if "token_records" in tables:
            columns = table_columns(connection, "token_records")
            if "request_id" in columns:
                query = (
                    "SELECT rowid, request_id, input_tokens + output_tokens "
                    "FROM token_records WHERE rowid > ? "
                    "AND request_id IS NOT NULL"
                )
                parameters: list[int] = [token_after_rowid]
                if since_ns is not None and "timestamp_ns" in columns:
                    query += " AND timestamp_ns >= ?"
                    parameters.append(since_ns)
                for rowid, request_id, total in connection.execute(query, parameters):
                    request_id = str(request_id)
                    if not prefix or request_id.startswith(prefix):
                        merge_capture(captured, request_id, "complete", int(total))
                    next_token_rowid = max(next_token_rowid, int(rowid))
        if "genai_events" in tables:
            columns = table_columns(connection, "genai_events")
            selected = [
                name
                for name in (
                    "call_id",
                    "trace_id",
                    "status",
                    "total_tokens",
                    "event_json",
                )
                if name in columns
            ]
            if selected:
                has_id = "id" in columns
                id_column = "id, " if has_id else ""
                query = f"SELECT {id_column}{', '.join(selected)} FROM genai_events"
                parameters = []
                if has_id:
                    query += " WHERE id > ?"
                    parameters.append(genai_after_id)
                if since_ns is not None and "start_timestamp_ns" in columns:
                    query += (
                        " AND start_timestamp_ns >= ?"
                        if has_id
                        else " WHERE start_timestamp_ns >= ?"
                    )
                    parameters.append(since_ns)

                def rows() -> Iterable[tuple[Any, ...]]:
                    # Keep payload memory bounded even when the capture database
                    # contains a large backlog outside this benchmark's prefix.
                    for row in connection.execute(query, parameters):
                        yield row if has_id else (None, *row)
                    if not has_id:
                        return
                    pending_list = sorted(pending_genai_ids)
                    for start in range(0, len(pending_list), 500):
                        chunk = pending_list[start : start + 500]
                        missing = set(chunk)
                        placeholders = ",".join("?" for _ in chunk)
                        for row in connection.execute(
                            f"SELECT id, {', '.join(selected)} "
                            f"FROM genai_events WHERE id IN ({placeholders})",
                            chunk,
                        ):
                            missing.discard(row[0])
                            yield row
                        # Retention-deleted rows cannot receive a later completion.
                        next_pending_ids.difference_update(missing)

                for row in rows():
                    row_id = int(row[0]) if row[0] is not None else None
                    record = dict(zip(selected, row[1:]))
                    identifiers = {
                        str(record[name]) for name in ("call_id", "trace_id") if record.get(name)
                    }
                    event: Any = None
                    if record.get("event_json"):
                        try:
                            event = json.loads(record["event_json"])
                            identifiers.update(find_request_ids(event))
                        except (json.JSONDecodeError, TypeError):
                            pass
                    db_status = str(record.get("status") or "complete")
                    status = capture_status_from_event(db_status, event)
                    for request_id in identifiers:
                        if not prefix or request_id.startswith(prefix):
                            merge_capture(
                                captured,
                                request_id,
                                status,
                                record.get("total_tokens"),
                            )
                    if row_id is not None:
                        # GenAI rows may receive their final token totals after
                        # becoming complete; keep refreshing requested evidence.
                        if db_status == "pending" or (
                            db_status == "complete"
                            and expected_total_tokens is not None
                            and record.get("total_tokens") != expected_total_tokens
                            and any(
                                not prefix or identifier.startswith(prefix)
                                for identifier in identifiers
                            )
                        ):
                            next_pending_ids.add(row_id)
                        else:
                            next_pending_ids.discard(row_id)
                        next_genai_id = max(next_genai_id, row_id)
    return (
        captured,
        next_genai_id,
        next_token_rowid,
        next_pending_ids,
    )


def merge_captured(target: Captured, source: Captured) -> None:
    """Merge observations without inflating duplicate rows across polls."""
    for request_id, observations in source.items():
        for status, total in observations:
            merge_capture(target, request_id, status, total)


def find_request_ids(value: Any) -> set[str]:
    """Recursively extract request_id values from serialized semantic events."""
    if isinstance(value, dict):
        found = {str(value["request_id"])} if value.get("request_id") else set()
        for child in value.values():
            found.update(find_request_ids(child))
        return found
    if isinstance(value, list):
        found: set[str] = set()
        for child in value:
            found.update(find_request_ids(child))
        return found
    if isinstance(value, str) and value.lstrip().startswith(("{", "[")):
        # AgentSight preserves the original HTTP request as a JSON-encoded
        # string in ``request.raw_body``. Decode one nested JSON layer so the
        # benchmark can correlate records even when the response parser did
        # not copy the request_id into the semantic event metadata.
        try:
            decoded = json.loads(value)
        except json.JSONDecodeError:
            return set()
        return find_request_ids(decoded)
    return set()


def make_report(
    expected: set[str],
    successful: set[str],
    captured: dict[str, list[tuple[str, int | None]]],
    expected_total_tokens: int | None,
) -> dict[str, Any]:
    """Build the JSON report used by CI and benchmark comparisons."""
    matched = expected & captured.keys()
    complete = {
        request_id
        for request_id in matched
        if any(status == "complete" for status, _ in captured[request_id])
    }
    rejected = {
        request_id
        for request_id in matched
        if request_id not in complete
        and any(
            status == "rejected:capacity_rejected" or status == "capacity_rejected"
            for status, _ in captured[request_id]
        )
    }
    conflicts = {
        request_id
        for request_id in matched
        if any(status == "complete" for status, _ in captured[request_id])
        and any(
            status.startswith(("rejected:", "incomplete:")) or status == "interrupted"
            for status, _ in captured[request_id]
        )
    }
    admitted_sent = len(expected) - len(rejected)
    token_correct = set()
    captured_token_totals: list[int] = []
    if expected_total_tokens is not None:
        token_correct = {
            request_id
            for request_id in matched
            if any(
                status == "complete" and total == expected_total_tokens
                for status, total in captured[request_id]
            )
        }
    for request_id in complete:
        total = next(
            (
                total
                for status, total in captured[request_id]
                if status == "complete" and total == expected_total_tokens
            ),
            None,
        )
        if total is None:
            total = next(
                (
                    total
                    for status, total in captured[request_id]
                    if status == "complete" and total is not None
                ),
                None,
            )
        if total is not None:
            captured_token_totals.append(total)
    report = {
        "sent": len(expected),
        "http_success": len(successful),
        "captured_calls": len(captured),
        "matched": len(matched),
        "complete": len(complete),
        "token_correct": (len(token_correct) if expected_total_tokens is not None else None),
        "expected_total_tokens": (
            expected_total_tokens * len(successful) if expected_total_tokens is not None else None
        ),
        "captured_total_tokens": (sum(captured_token_totals) if captured_token_totals else None),
        "rejected": len(rejected),
        "rejected_request_ids": len(rejected),
        "capacity_rejected_events": None,
        "conflicts": len(conflicts),
        "admitted_sent": admitted_sent,
        "admitted_completeness": (
            len(complete) / admitted_sent if admitted_sent > 0 else None
        ),
        "unexplained_loss": len(expected) - len(complete) - len(rejected),
        "missing_ids": sorted(expected - captured.keys()),
        "extra_ids": sorted(captured.keys() - expected),
    }
    denominator = len(expected) or 1
    report["match_ratio"] = len(matched) / denominator
    report["completeness_ratio"] = len(complete) / denominator
    report["token_accuracy"] = (
        len(token_correct) / denominator if expected_total_tokens is not None else None
    )
    return report


def apply_capacity_rejected_metrics(report: dict[str, Any], count: int | None) -> None:
    """Apply aggregate capacity rejections while preserving request-level attribution."""
    report["capacity_rejected_events"] = count
    if report.get("rejected_request_ids") is None:
        report["rejected_request_ids"] = report.get("rejected", 0)
    sent = report["sent"]
    complete = report["complete"]
    missing = max(0, sent - complete)
    rejected_ids = report["rejected_request_ids"]
    if count is not None and count > missing:
        logging.warning(
            "capacity_rejected_events=%d exceeds missing requests=%d; "
            "the excess may be retries or pre-load traffic and is not attributed to request loss",
            count,
            missing,
        )
    # Rejections cannot outnumber requests that never completed; clamping here
    # keeps admitted_sent >= complete instead of hiding an impossible ratio
    # behind min(1.0, ...).
    rejected = min(missing, max(rejected_ids, count or 0))
    admitted_sent = sent - rejected
    report["rejected"] = rejected
    report["admitted_sent"] = admitted_sent
    report["admitted_completeness"] = complete / admitted_sent if admitted_sent else None
    report["unexplained_loss"] = missing - rejected


def _validation_passed(report: dict[str, Any], minimum: float) -> bool:
    return (
        report["completeness_ratio"] >= minimum
        and (report["token_accuracy"] is None or report["token_accuracy"] >= minimum)
        and report.get("conflicts", 0) == 0
    )


def wait_for_report(
    db_path: Path | None,
    prefix: str,
    expected: set[str],
    successful: set[str],
    expected_total_tokens: int | None,
    min_completeness: float,
    wait_seconds: float,
    poll_interval: float,
    since_ns: int | None = None,
    ffi_events: Path | None = None,
) -> dict[str, Any]:
    """Poll SQLite or callback receipts until capture settles or the deadline expires."""
    deadline = time.monotonic() + wait_seconds
    captured: Captured = {}
    while True:
        try:
            captured = (
                load_ffi_captured(ffi_events, prefix, expected)[0]
                if ffi_events is not None
                else load_captured(db_path, prefix, expected, since_ns)
            )
        except sqlite3.OperationalError as error:
            if (getattr(error, "sqlite_errorcode", 0) & 0xFF) not in (
                sqlite3.SQLITE_BUSY,
                sqlite3.SQLITE_LOCKED,
            ):
                raise
            logging.warning("Capture database busy; retrying within validation deadline: %s", error)
        report = make_report(expected, successful, captured, expected_total_tokens)
        if _validation_passed(report, min_completeness):
            return report
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return report
        time.sleep(min(poll_interval, remaining))


def wait_for_streaming_report(
    db_path: Path | None,
    load_results: Path,
    completion_marker: Path,
    prefix: str,
    expected_total_tokens: int | None,
    min_completeness: float,
    wait_seconds: float,
    poll_interval: float,
    since_ns: int | None = None,
    request_log: Path | None = None,
    ffi_events: Path | None = None,
) -> dict[str, Any]:
    """Accumulate captures while load runs so retention pruning cannot erase evidence."""
    captured: Captured = {}
    genai_after_id = 0
    token_after_rowid = 0
    pending_genai_ids: set[int] = set()
    completion_deadline: float | None = None
    ffi_offset = 0
    while True:
        try:
            if ffi_events is not None:
                incremental, ffi_offset = load_ffi_captured(ffi_events, prefix, offset=ffi_offset)
            else:
                incremental, genai_after_id, token_after_rowid, pending_genai_ids = (
                    load_captured_incremental(
                        db_path,
                        prefix,
                        # Run IDs are authoritative across capture clock domains.
                        None,
                        genai_after_id,
                        token_after_rowid,
                        pending_genai_ids,
                        expected_total_tokens=expected_total_tokens,
                    )
                )
        except sqlite3.OperationalError as error:
            if (getattr(error, "sqlite_errorcode", 0) & 0xFF) not in (
                sqlite3.SQLITE_BUSY,
                sqlite3.SQLITE_LOCKED,
            ):
                raise
            logging.warning("Capture database busy; retrying within validation deadline: %s", error)
            incremental = {}
        merge_captured(captured, incremental)
        if completion_marker.exists():
            expected, successful = (
                load_request_log(request_log, prefix)
                if request_log is not None
                else load_expected(load_results)
            )
            report = make_report(expected, successful, captured, expected_total_tokens)
            report["validation_mode"] = "streaming_incremental"
            report["retention_safe"] = True
            report["capture_source"] = "ffi_callbacks" if ffi_events is not None else "sqlite"
            if _validation_passed(report, min_completeness):
                return report
            now = time.monotonic()
            if completion_deadline is None:
                completion_deadline = now + wait_seconds
            if now >= completion_deadline:
                return report
        time.sleep(
            min(poll_interval, completion_deadline - now)
            if completion_deadline is not None
            else poll_interval
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--load-results", type=Path, required=True)
    parser.add_argument(
        "--request-log",
        type=Path,
        help="read per-request IDs from the mock server instead of k6 metric tags",
    )
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--db", type=Path)
    source.add_argument(
        "--ffi-events", type=Path, help="callback receipts from the minimal FFI host"
    )
    parser.add_argument("--prefix", default="")
    parser.add_argument(
        "--since-ns",
        type=int,
        help="ignore database records with an earlier Unix timestamp",
    )
    parser.add_argument("--expected-total-tokens", type=int)
    parser.add_argument("--metrics-file", type=Path)
    parser.add_argument(
        "--metrics-baseline",
        type=Path,
        help="pre-load Prometheus snapshot used to scope cumulative counters to this run",
    )
    parser.add_argument("--min-completeness", type=float, default=0.999)
    parser.add_argument(
        "--wait-seconds",
        type=float,
        default=0,
        help="wait for asynchronous AgentSight writes before final validation",
    )
    parser.add_argument("--poll-interval", type=float, default=0.5)
    parser.add_argument(
        "--completion-marker",
        type=Path,
        help="poll throughout the load and finish after this file appears",
    )
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.db is not None and not args.db.exists():
        raise SystemExit(f"AgentSight database does not exist: {args.db}")
    if not 0 <= args.min_completeness <= 1:
        raise SystemExit("min-completeness must be between 0 and 1")
    if not math.isfinite(args.wait_seconds) or args.wait_seconds < 0:
        raise SystemExit("wait-seconds must be finite and non-negative")
    if not math.isfinite(args.poll_interval) or args.poll_interval <= 0:
        raise SystemExit("poll-interval must be finite and positive")
    if args.since_ns is not None and args.since_ns < 0:
        raise SystemExit("since-ns must be non-negative")
    if args.completion_marker:
        report = wait_for_streaming_report(
            args.db,
            args.load_results,
            args.completion_marker,
            args.prefix,
            args.expected_total_tokens,
            args.min_completeness,
            args.wait_seconds,
            args.poll_interval,
            args.since_ns,
            args.request_log,
            args.ffi_events,
        )
    else:
        expected, successful = (
            load_request_log(args.request_log, args.prefix)
            if args.request_log is not None
            else load_expected(args.load_results)
        )
        report = wait_for_report(
            args.db,
            args.prefix,
            expected,
            successful,
            args.expected_total_tokens,
            args.min_completeness,
            args.wait_seconds,
            args.poll_interval,
            args.since_ns,
            args.ffi_events,
        )
    report["capture_source"] = "ffi_callbacks" if args.ffi_events is not None else "sqlite"
    metrics_path = resolve_metrics_path(args.load_results, args.metrics_file)
    baseline_path = resolve_metrics_baseline(args.load_results, args.metrics_baseline)
    apply_capacity_rejected_metrics(
        report, load_capacity_rejected_events(metrics_path, baseline_path)
    )
    text = json.dumps(report, indent=2, sort_keys=True)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text + "\n", encoding="utf-8")
    print(
        json.dumps(
            {
                "sent": report["sent"],
                "matched": report["matched"],
                "complete": report["complete"],
                "rejected": report["rejected"],
                "capacity_rejected_events": report["capacity_rejected_events"],
                "unexplained_loss": report["unexplained_loss"],
                "completeness_ratio": report["completeness_ratio"],
                "token_accuracy": report["token_accuracy"],
                "missing_count": len(report["missing_ids"]),
                "extra_count": len(report["extra_ids"]),
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0 if _validation_passed(report, args.min_completeness) else 1


if __name__ == "__main__":
    raise SystemExit(main())
