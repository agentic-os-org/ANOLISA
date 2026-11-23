#!/usr/bin/env python3
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""Pre/post-compression tool results of one L4 arm, for the LLM semantic sample.

Two sources, one output shape:

- arm B（tokenless）：stats 中的 `compress-response` 和 `compress-toon` 均携带
  压缩前后文本；没有实际恢复载荷来源时，`retrieval` 为 null，不伪造空恢复集。
- arms C-F (headroom): the proxy's JSONL log, which with `--log-messages`
  records every request's messages before (`request_messages`) and after
  (`compressed_messages`) compression; tool messages are paired by
  `tool_call_id`, or by position when a request carries no ids.

Only results the side actually changed are kept, deduplicated (a conversation
resends earlier tool results on every turn) and thinned evenly to `--max-rows`.
The output feeds `llm-eval`'s L4 loader: rows of {instance, arm, call_id,
tool_name, original, compressed, retrieval}.

Instance attribution: a stats row names its OpenClaw session, whose id embeds
the instance id; a proxy row is matched through its tool call id against the
per-instance transcript stores under `--profiles-dir`, because the proxy log
itself records no session. Known ids come from `--instance-list`.
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import re
import sqlite3
import sys
from pathlib import Path

INSTANCE = re.compile(
    r"([A-Za-z0-9_.-]+__[A-Za-z0-9_.-]+?-\d+)(?:-[0-9a-f]{8})?(?![0-9])"
)
CALL_ID = re.compile(r'"(?:toolCallId|tool_call_id|toolUseId|id)"\s*:\s*"([^"]{6,})"')


def text_of(content) -> str | None:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = [
            part.get("text", "")
            for part in content
            if isinstance(part, dict)
            and part.get("type") in ("text", "input_text", "output_text")
        ]
        return "".join(parts) if parts else None
    return None


class Attribution:
    """Maps session ids and tool call ids to instance ids."""

    def __init__(self, known: list[str], profiles: Path | None) -> None:
        # Longest first, so django__django-1315 can never shadow django__django-13158.
        self.known = sorted(set(known), key=len, reverse=True)
        self.by_call: dict[str, str] = {}
        if profiles and profiles.is_dir():
            for profile in sorted(path for path in profiles.iterdir() if path.is_dir()):
                instance = self.instance_of(profile.name)
                for store in sorted(profile.rglob("*")):
                    for text in strings_in(store):
                        for call in CALL_ID.findall(text):
                            self.by_call.setdefault(call, instance)

    def instance_of(self, *values) -> str:
        for value in values:
            if not isinstance(value, str):
                continue
            for known in self.known:
                if known in value:
                    return known
            if not self.known and (match := INSTANCE.search(value)):
                return match.group(1)
        return "unknown"

    def instance_of_call(self, call_id: str | None) -> str:
        return self.by_call.get(call_id or "", "unknown")


def strings_in(path: Path):
    """Text held by one transcript file: every string cell of a SQLite store, or the file itself."""
    if not path.is_file():
        return
    if path.suffix == ".sqlite":
        # Through SQLite rather than the raw file, so rows still in the WAL count.
        try:
            connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
            tables = [
                name
                for (name,) in connection.execute(
                    "SELECT name FROM sqlite_master WHERE type = 'table'"
                )
            ]
            for table in tables:
                for row in connection.execute(f'SELECT * FROM "{table}"'):  # noqa: S608 - names from sqlite_master
                    yield from (value for value in row if isinstance(value, str))
        except sqlite3.Error:
            return
    elif path.suffix in (".jsonl", ".json"):
        yield path.read_text(errors="replace")


def recovery_events(
    connection: sqlite3.Connection, since_ns: int, session: str | None
) -> list[dict] | None:
    if session is None:
        return None
    since = datetime.datetime.fromtimestamp(
        since_ns / 1_000_000_000, datetime.timezone.utc
    ).isoformat()
    try:
        records = connection.execute(
            "SELECT hash, outcome, tool_use_id FROM retrieve_events WHERE session_id = ? AND timestamp >= ?",
            (session, since),
        ).fetchall()
    except sqlite3.Error:
        return None
    return [
        {"key": key, "outcome": outcome, "call_id": call_id}
        for key, outcome, call_id in records
    ]


def from_stats(
    db: Path, since_ns: int, arm: str, attribution: Attribution
) -> list[dict]:
    connection = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    rows = connection.execute(
        "SELECT id, session_id, tool_use_id, before_text, after_text, content_type, recoverability, "
        "applied_operations, before_tokens, after_tokens FROM stats "
        "WHERE operation IN ('compress-response', 'compress-toon') AND before_text IS NOT NULL AND after_text IS NOT NULL "
        "AND COALESCE(timestamp_ns, 0) >= ? ORDER BY id",
        (since_ns,),
    ).fetchall()
    out = []
    for (
        rid,
        session,
        tool_use,
        before,
        after,
        content_type,
        recoverability,
        operations,
        bt,
        at,
    ) in rows:
        instance = attribution.instance_of(session)
        if instance == "unknown":
            instance = attribution.instance_of_call(tool_use)
        events = recovery_events(connection, since_ns, session)
        out.append(
            {
                "instance": instance,
                "arm": arm,
                "call_id": tool_use or f"stats-{rid}",
                "tool_name": None,
                "original": before,
                "compressed": after,
                "content_type": content_type,
                "recoverability": recoverability,
                "operations": operations,
                "estimated_tokens_before": bt,
                "estimated_tokens_after": at,
                "retrieval": None,
                "recovery_observation": "unobservable"
                if events is None
                else "events_only",
                "recovery_events": events,
            }
        )
    return out


def tool_messages(messages: list) -> list[tuple[str | None, str | None]]:
    return [
        (message.get("tool_call_id"), text_of(message.get("content")))
        for message in messages
        if isinstance(message, dict) and message.get("role") == "tool"
    ]


def pair_tool_results(before: list, after: list) -> list[tuple[str | None, str, str]]:
    """(call id, original, compressed) for every tool message the proxy changed."""
    original, compressed = tool_messages(before), tool_messages(after)
    by_id = {call: text for call, text in compressed if call}
    pairs = []
    if by_id:
        for call, text in original:
            if call in by_id:
                pairs.append((call, text, by_id[call]))
    elif len(original) == len(compressed):
        pairs = [(call, o, c) for (call, o), (_, c) in zip(original, compressed)]
    return [
        (call, o, c)
        for call, o, c in pairs
        if o is not None and c is not None and o != c
    ]


def from_proxy_log(
    path: Path, since: datetime.datetime, arm: str, attribution: Attribution
) -> list[dict]:
    out = []
    for line in path.read_text(errors="replace").splitlines():
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if not isinstance(entry, dict):
            continue
        stamp = entry.get("timestamp")
        if isinstance(stamp, str):
            try:
                when = datetime.datetime.fromisoformat(stamp.replace("Z", "+00:00"))
                if when.tzinfo is None:
                    when = when.replace(tzinfo=datetime.timezone.utc)
                if when < since:
                    continue
            except ValueError:
                pass
        before = entry.get("request_messages") or []
        after = entry.get("compressed_messages") or []
        if not isinstance(before, list) or not isinstance(after, list):
            continue
        names = {}
        for message in before:
            for call in (
                (message.get("tool_calls") or []) if isinstance(message, dict) else []
            ):
                if isinstance(call, dict) and call.get("id"):
                    names[call["id"]] = (call.get("function") or {}).get("name")
        for call_id, original, compressed in pair_tool_results(before, after):
            out.append(
                {
                    "instance": attribution.instance_of_call(call_id),
                    "arm": arm,
                    "call_id": call_id
                    or hashlib.sha256(original.encode()).hexdigest()[:16],
                    "tool_name": names.get(call_id),
                    "original": original,
                    "compressed": compressed,
                    "transforms_applied": entry.get("transforms_applied"),
                    "retrieval": None,
                    "recovery_observation": "unobservable",
                }
            )
    return out


def thin(rows: list[dict], maximum: int, min_chars: int) -> list[dict]:
    unique, seen = [], set()
    for row in rows:
        if row["original"] == row["compressed"] or len(row["original"]) < min_chars:
            continue
        key = hashlib.sha256(
            (row["original"] + "\0" + row["compressed"]).encode()
        ).hexdigest()
        if key not in seen:
            seen.add(key)
            unique.append(row)
    if len(unique) <= maximum:
        return unique
    if maximum == 1:
        return unique[:1]
    step = (len(unique) - 1) / (maximum - 1)
    return [unique[round(index * step)] for index in range(maximum)]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--arm", required=True)
    parser.add_argument("--since", required=True, help="UTC ISO-8601 start of the arm")
    parser.add_argument("--output", type=Path, required=True)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--stats-db", type=Path)
    source.add_argument("--proxy-log", type=Path)
    parser.add_argument(
        "--instance-list", type=Path, help="known instance ids, one per line"
    )
    parser.add_argument(
        "--profiles-dir", type=Path, help="the arm's openclaw-profiles directory"
    )
    parser.add_argument("--max-rows", type=int, default=400)
    parser.add_argument("--min-chars", type=int, default=200)
    args = parser.parse_args()
    if args.max_rows < 1 or args.min_chars < 0:
        parser.error("max-rows must be positive and min-chars must be non-negative")
    since = datetime.datetime.fromisoformat(args.since.replace("Z", "+00:00"))
    if since.tzinfo is None:
        since = since.replace(tzinfo=datetime.timezone.utc)
    known = []
    if args.instance_list and args.instance_list.exists():
        known = [
            line.strip()
            for line in args.instance_list.read_text().splitlines()
            if line.strip()
        ]
    attribution = Attribution(known, args.profiles_dir)
    if args.stats_db:
        if not args.stats_db.exists():
            print(f"[capture] arm {args.arm}: no stats store at {args.stats_db}")
            return 1
        # Integer nanoseconds: a float product would round away the boundary.
        since_ns = int(since.timestamp()) * 1_000_000_000 + since.microsecond * 1000
        rows = from_stats(args.stats_db, since_ns, args.arm, attribution)
        origin = "tokenless stats store (compress-response/compress-toon before_text/after_text)"
    else:
        if not args.proxy_log.exists():
            print(f"[capture] arm {args.arm}: no proxy log at {args.proxy_log}")
            return 1
        rows = from_proxy_log(args.proxy_log, since, args.arm, attribution)
        origin = "headroom proxy --log-messages (request_messages/compressed_messages)"
    kept = thin(rows, args.max_rows, args.min_chars)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "arm": args.arm,
                "source": origin,
                "since": args.since,
                "changed_results": len(rows),
                "rows": kept,
            },
            ensure_ascii=False,
            indent=1,
        )
        + "\n"
    )
    unattributed = sum(1 for row in kept if row["instance"] == "unknown")
    print(
        f"[capture] arm {args.arm}: {len(rows)} changed tool results, {len(kept)} kept "
        f"({unattributed} without an instance) -> {args.output}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
