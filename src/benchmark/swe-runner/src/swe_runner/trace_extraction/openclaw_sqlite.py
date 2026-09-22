# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Token accounting read from OpenClaw's own SQLite transcript store.

OpenClaw 2026.8.2 writes no session JSONL, so the JSONL reader in
:mod:`swe_runner.trace_extraction.openclaw_jsonl` cannot see these runs at all.
This module reads the same numbers the agent itself recorded, from two
independent places in one store, and reports whether they agree -- a schema
change that moved or renamed a field then shows up as a disagreement instead of
as a plausible wrong total.

The headline metric is ``prompt_tokens`` (``input + cacheRead``), and it is
meaningless without ``requests`` alongside it: a context-compression arm lowers
the tokens per request while often needing more requests, so a per-request
saving and a higher end-to-end total are not contradictory. Any report that
quotes one number without the other invites the wrong conclusion.
"""

from __future__ import annotations

import json
import logging
import sqlite3
from dataclasses import dataclass, field
from pathlib import Path

logger = logging.getLogger(__name__)

# Layout observed under a runner-managed profile:
#   <profile>/agents/<agent_id>/agent/openclaw-agent.sqlite
AGENT_STORE_GLOB = "agents/*/agent/openclaw-agent.sqlite"

_TURN_TABLE = "transcript_events"
_AGGREGATE_TABLE = "trajectory_runtime_events"
# Both events carry an identical session total; either alone is enough to
# cross-check the per-turn sum, and reading both guards against one being absent
# on an aborted run.
_AGGREGATE_EVENT_TYPES = ("model.completed", "trace.artifacts")


class SqliteUsageError(Exception):
    """Raised when a store cannot be read as an OpenClaw transcript."""


def _as_int(value: object) -> int:
    return value if isinstance(value, int) and not isinstance(value, bool) else 0


@dataclass(frozen=True)
class TurnUsage:
    """One assistant reply's token usage, in transcript order."""

    seq: int
    model: str | None
    input_tokens: int
    output_tokens: int
    cache_read_tokens: int
    cache_write_tokens: int

    @property
    def prompt_tokens(self) -> int:
        """Tokens the provider had to read for this turn, cached or not."""
        return self.input_tokens + self.cache_read_tokens


@dataclass
class SessionUsage:
    """Token totals for one OpenClaw session, plus the agent's own aggregate."""

    store_path: str
    session_id: str
    turns: list[TurnUsage] = field(default_factory=list)
    reported: dict[str, int] | None = None

    @property
    def requests(self) -> int:
        return len(self.turns)

    @property
    def input_tokens(self) -> int:
        return sum(turn.input_tokens for turn in self.turns)

    @property
    def output_tokens(self) -> int:
        return sum(turn.output_tokens for turn in self.turns)

    @property
    def cache_read_tokens(self) -> int:
        return sum(turn.cache_read_tokens for turn in self.turns)

    @property
    def cache_write_tokens(self) -> int:
        return sum(turn.cache_write_tokens for turn in self.turns)

    @property
    def prompt_tokens(self) -> int:
        """Total prompt volume: report with :attr:`requests` or not at all."""
        return self.input_tokens + self.cache_read_tokens

    @property
    def aggregate_agrees(self) -> bool | None:
        """Whether the per-turn sum matches the session aggregate event.

        ``None`` when the run recorded no aggregate (an aborted session), which
        is missing corroboration rather than a mismatch.
        """
        if self.reported is None:
            return None
        return (
            self.reported.get("input") == self.input_tokens
            and self.reported.get("output") == self.output_tokens
            and self.reported.get("cacheRead") == self.cache_read_tokens
        )

    def to_dict(self) -> dict[str, object]:
        """Return the JSON form used in run reports."""
        return {
            "store_path": self.store_path,
            "session_id": self.session_id,
            "requests": self.requests,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cache_read_tokens": self.cache_read_tokens,
            "cache_write_tokens": self.cache_write_tokens,
            "prompt_tokens": self.prompt_tokens,
            "reported_aggregate": self.reported,
            "aggregate_agrees": self.aggregate_agrees,
            "turns": [
                {
                    "seq": turn.seq,
                    "model": turn.model,
                    "input_tokens": turn.input_tokens,
                    "output_tokens": turn.output_tokens,
                    "cache_read_tokens": turn.cache_read_tokens,
                    "cache_write_tokens": turn.cache_write_tokens,
                    "prompt_tokens": turn.prompt_tokens,
                }
                for turn in self.turns
            ],
        }


def _read_turns(connection: sqlite3.Connection) -> tuple[str, list[TurnUsage]]:
    session_id = ""
    turns: list[TurnUsage] = []
    query = f"SELECT session_id, seq, event_json FROM {_TURN_TABLE} ORDER BY seq"  # noqa: S608
    for raw_session_id, seq, event_json in connection.execute(query):
        try:
            event = json.loads(event_json)
        except (TypeError, ValueError):
            continue
        message = event.get("message") if isinstance(event, dict) else None
        usage = message.get("usage") if isinstance(message, dict) else None
        if not isinstance(usage, dict):
            continue
        if isinstance(raw_session_id, str) and raw_session_id:
            session_id = raw_session_id
        model = message.get("model")
        turns.append(
            TurnUsage(
                seq=_as_int(seq),
                model=model if isinstance(model, str) else None,
                input_tokens=_as_int(usage.get("input")),
                output_tokens=_as_int(usage.get("output")),
                cache_read_tokens=_as_int(usage.get("cacheRead")),
                cache_write_tokens=_as_int(usage.get("cacheWrite")),
            )
        )
    return session_id, turns


def _read_aggregate(connection: sqlite3.Connection) -> dict[str, int] | None:
    query = f"SELECT event_json FROM {_AGGREGATE_TABLE} ORDER BY rowid"  # noqa: S608
    for (event_json,) in connection.execute(query):
        try:
            event = json.loads(event_json)
        except (TypeError, ValueError):
            continue
        if not isinstance(event, dict) or event.get("type") not in _AGGREGATE_EVENT_TYPES:
            continue
        data = event.get("data")
        usage = data.get("usage") if isinstance(data, dict) else None
        if not isinstance(usage, dict):
            continue
        # The aggregate event names the sum "total" while per-turn rows call it
        # "totalTokens"; keep the source spelling so a future rename is visible.
        return {
            "input": _as_int(usage.get("input")),
            "output": _as_int(usage.get("output")),
            "cacheRead": _as_int(usage.get("cacheRead")),
            "total": _as_int(usage.get("total")),
        }
    return None


def read_store_usage(store_path: Path) -> SessionUsage:
    """Read token usage from one ``openclaw-agent.sqlite``.

    Raises:
        SqliteUsageError: The file is not readable as an OpenClaw agent store,
            for instance because the transcript table is absent.
    """
    uri = f"file:{store_path}?mode=ro"
    try:
        with sqlite3.connect(uri, uri=True) as connection:
            session_id, turns = _read_turns(connection)
            reported = _read_aggregate(connection)
    except sqlite3.Error as exc:
        raise SqliteUsageError(f"cannot read OpenClaw agent store {store_path}: {exc}") from exc

    usage = SessionUsage(store_path=str(store_path), session_id=session_id, turns=turns, reported=reported)
    if usage.aggregate_agrees is False:
        # Loud rather than fatal: the numbers are still reportable, but they can
        # no longer be presented as corroborated by two independent readings.
        logger.error(
            "OPENCLAW_SQLITE_USAGE_MISMATCH store=%s per_turn=input:%s,output:%s,cacheRead:%s reported=%s",
            store_path,
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            reported,
        )
    return usage


def find_agent_stores(profile_dir: Path) -> list[Path]:
    """Return the agent SQLite stores inside one OpenClaw profile directory."""
    return sorted(profile_dir.glob(AGENT_STORE_GLOB))


def read_profiles_usage(profiles_root: Path) -> dict[str, SessionUsage]:
    """Map instance id to session usage for every profile under a run's profiles root.

    The profile directory name is the instance id, as written by the runner.

    Raises:
        SqliteUsageError: ``profiles_root`` does not exist, or a profile that
            exists holds no agent store. Both cases would otherwise produce an
            empty report that reads like a run where no tokens were spent.
    """
    if not profiles_root.is_dir():
        raise SqliteUsageError(f"profiles root not found: {profiles_root}")

    usages: dict[str, SessionUsage] = {}
    for profile_dir in sorted(path for path in profiles_root.iterdir() if path.is_dir()):
        stores = find_agent_stores(profile_dir)
        if not stores:
            raise SqliteUsageError(f"no agent SQLite store under profile {profile_dir}")
        if len(stores) > 1:
            raise SqliteUsageError(
                f"profile {profile_dir} holds {len(stores)} agent stores; per-instance "
                "attribution needs exactly one"
            )
        usages[profile_dir.name] = read_store_usage(stores[0])
    return usages


def build_usage_report(*, arm: str, profiles_root: Path) -> dict[str, object]:
    """Build the per-arm token report that downstream comparisons are computed from."""
    usages = read_profiles_usage(profiles_root)
    instances = {instance_id: usage.to_dict() for instance_id, usage in sorted(usages.items())}
    return {
        "schema_version": 1,
        "arm": arm,
        "profiles_root": str(profiles_root),
        # metric_definition travels with the numbers on purpose: the same totals
        # support opposite readings depending on whether requests are shown.
        "metric_definition": {
            "prompt_tokens": "sum over assistant turns of (usage.input + usage.cacheRead)",
            "requests": "assistant turns carrying a usage record",
            "note": "prompt_tokens is only interpretable together with requests",
        },
        "totals": {
            "instances": len(usages),
            "requests": sum(usage.requests for usage in usages.values()),
            "input_tokens": sum(usage.input_tokens for usage in usages.values()),
            "output_tokens": sum(usage.output_tokens for usage in usages.values()),
            "cache_read_tokens": sum(usage.cache_read_tokens for usage in usages.values()),
            "cache_write_tokens": sum(usage.cache_write_tokens for usage in usages.values()),
            "prompt_tokens": sum(usage.prompt_tokens for usage in usages.values()),
            "instances_with_disagreeing_aggregate": sorted(
                instance_id for instance_id, usage in usages.items() if usage.aggregate_agrees is False
            ),
            "instances_without_aggregate": sorted(
                instance_id for instance_id, usage in usages.items() if usage.aggregate_agrees is None
            ),
        },
        "instances": instances,
    }
