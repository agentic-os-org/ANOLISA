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

"""Tests for token accounting read from OpenClaw's SQLite transcript store.

The fixtures reproduce the schema observed in an OpenClaw 2026.8.2 run: per-turn
usage under ``transcript_events.event_json.message.usage``, and a session
aggregate under ``trajectory_runtime_events.event_json.data.usage`` where the sum
is spelled ``total`` rather than ``totalTokens``.
"""

import json
import logging
import sqlite3
from pathlib import Path

import pytest

from swe_runner.trace_extraction.openclaw_sqlite import (
    SqliteUsageError,
    build_usage_report,
    find_agent_stores,
    read_profiles_usage,
    read_store_usage,
)


def _turn_event(*, input_tokens: int, output_tokens: int, cache_read: int, cache_write: int = 0) -> str:
    return json.dumps(
        {
            "message": {
                "role": "assistant",
                "model": "qwen3-coder-plus",
                "usage": {
                    "input": input_tokens,
                    "output": output_tokens,
                    "cacheRead": cache_read,
                    "cacheWrite": cache_write,
                    "totalTokens": input_tokens + output_tokens + cache_read + cache_write,
                },
            }
        }
    )


def _write_store(
    store_path: Path,
    *,
    turns: list[dict[str, int]],
    aggregate: dict[str, int] | None,
    session_id: str = "session-1",
) -> Path:
    store_path.parent.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(store_path)
    with connection:
        connection.execute(
            "CREATE TABLE transcript_events "
            "(session_id TEXT, seq INTEGER, event_json TEXT, created_at TEXT)"
        )
        connection.execute("CREATE TABLE trajectory_runtime_events (event_json TEXT)")
        for seq, turn in enumerate(turns, start=1):
            connection.execute(
                "INSERT INTO transcript_events VALUES (?, ?, ?, ?)",
                (
                    session_id,
                    seq,
                    _turn_event(
                        input_tokens=turn["input"],
                        output_tokens=turn["output"],
                        cache_read=turn["cacheRead"],
                        cache_write=turn.get("cacheWrite", 0),
                    ),
                    "2026-07-28T00:00:00Z",
                ),
            )
        # A user message carries no usage record and must not count as a request.
        connection.execute(
            "INSERT INTO transcript_events VALUES (?, ?, ?, ?)",
            (session_id, 0, json.dumps({"message": {"role": "user", "content": "fix it"}}), "2026-07-28T00:00:00Z"),
        )
        if aggregate is not None:
            connection.execute(
                "INSERT INTO trajectory_runtime_events VALUES (?)",
                (json.dumps({"type": "model.completed", "data": {"usage": aggregate}}),),
            )
    connection.close()
    return store_path


def _profile_store(profiles_root: Path, instance_id: str) -> Path:
    return profiles_root / instance_id / "agents" / instance_id / "agent" / "openclaw-agent.sqlite"


def test_read_store_usage_sums_turns_and_defines_prompt_tokens_as_input_plus_cache_read(tmp_path: Path) -> None:
    store = _write_store(
        tmp_path / "openclaw-agent.sqlite",
        turns=[
            {"input": 100, "output": 10, "cacheRead": 1000, "cacheWrite": 5},
            {"input": 200, "output": 20, "cacheRead": 3000},
        ],
        aggregate={"input": 300, "output": 30, "cacheRead": 4000, "total": 4330},
    )

    usage = read_store_usage(store)

    assert usage.requests == 2
    assert usage.input_tokens == 300
    assert usage.output_tokens == 30
    assert usage.cache_read_tokens == 4000
    assert usage.cache_write_tokens == 5
    assert usage.prompt_tokens == 4300
    assert usage.session_id == "session-1"
    assert usage.aggregate_agrees is True


def test_read_store_usage_flags_a_disagreeing_aggregate_loudly(tmp_path: Path, caplog) -> None:
    """A schema drift must surface as a disagreement, not as a plausible total."""
    store = _write_store(
        tmp_path / "openclaw-agent.sqlite",
        turns=[{"input": 100, "output": 10, "cacheRead": 1000}],
        aggregate={"input": 999, "output": 10, "cacheRead": 1000, "total": 2009},
    )

    with caplog.at_level(logging.ERROR):
        usage = read_store_usage(store)

    assert usage.aggregate_agrees is False
    assert usage.input_tokens == 100, "the per-turn reading stays reportable despite the mismatch"
    assert "OPENCLAW_SQLITE_USAGE_MISMATCH" in caplog.text


def test_read_store_usage_reports_a_missing_aggregate_as_uncorroborated_not_wrong(tmp_path: Path) -> None:
    """An aborted run has no aggregate event; that is absent corroboration."""
    store = _write_store(
        tmp_path / "openclaw-agent.sqlite",
        turns=[{"input": 100, "output": 10, "cacheRead": 1000}],
        aggregate=None,
    )

    usage = read_store_usage(store)

    assert usage.reported is None
    assert usage.aggregate_agrees is None
    assert usage.prompt_tokens == 1100


def test_read_store_usage_rejects_a_file_without_a_transcript_table(tmp_path: Path) -> None:
    store = tmp_path / "not-openclaw.sqlite"
    connection = sqlite3.connect(store)
    with connection:
        connection.execute("CREATE TABLE unrelated (x INTEGER)")
    connection.close()

    with pytest.raises(SqliteUsageError, match="cannot read OpenClaw agent store"):
        read_store_usage(store)


def test_find_agent_stores_matches_the_observed_profile_layout(tmp_path: Path) -> None:
    store = _write_store(
        _profile_store(tmp_path, "astropy__astropy-12907"),
        turns=[{"input": 1, "output": 1, "cacheRead": 1}],
        aggregate=None,
    )

    assert find_agent_stores(tmp_path / "astropy__astropy-12907") == [store]


def test_read_profiles_usage_keys_instances_by_profile_directory_name(tmp_path: Path) -> None:
    _write_store(
        _profile_store(tmp_path, "instance-a"),
        turns=[{"input": 10, "output": 1, "cacheRead": 100}],
        aggregate={"input": 10, "output": 1, "cacheRead": 100, "total": 111},
    )
    _write_store(
        _profile_store(tmp_path, "instance-b"),
        turns=[{"input": 20, "output": 2, "cacheRead": 200}],
        aggregate={"input": 20, "output": 2, "cacheRead": 200, "total": 222},
    )

    usages = read_profiles_usage(tmp_path)

    assert sorted(usages) == ["instance-a", "instance-b"]
    assert usages["instance-b"].prompt_tokens == 220


def test_read_profiles_usage_fails_when_the_profiles_root_is_absent(tmp_path: Path) -> None:
    with pytest.raises(SqliteUsageError, match="profiles root not found"):
        read_profiles_usage(tmp_path / "never-ran")


def test_read_profiles_usage_fails_on_a_profile_holding_no_store(tmp_path: Path) -> None:
    """An empty profile means unreadable, not zero tokens spent."""
    (tmp_path / "instance-a" / "state").mkdir(parents=True)

    with pytest.raises(SqliteUsageError, match="no agent SQLite store"):
        read_profiles_usage(tmp_path)


def test_read_profiles_usage_fails_when_a_profile_holds_several_stores(tmp_path: Path) -> None:
    """Two agents in one profile make per-instance attribution ambiguous."""
    profile = tmp_path / "instance-a"
    for agent_id in ("agent-1", "agent-2"):
        _write_store(
            profile / "agents" / agent_id / "agent" / "openclaw-agent.sqlite",
            turns=[{"input": 1, "output": 1, "cacheRead": 1}],
            aggregate=None,
        )

    with pytest.raises(SqliteUsageError, match="holds 2 agent stores"):
        read_profiles_usage(tmp_path)


def test_build_usage_report_ships_the_metric_definition_with_the_totals(tmp_path: Path) -> None:
    """Prompt volume alone supports opposite readings; requests must ride along."""
    _write_store(
        _profile_store(tmp_path, "instance-a"),
        turns=[
            {"input": 100, "output": 10, "cacheRead": 1000},
            {"input": 100, "output": 10, "cacheRead": 2000},
        ],
        aggregate={"input": 200, "output": 20, "cacheRead": 3000, "total": 3220},
    )

    report = build_usage_report(arm="c-headroom", profiles_root=tmp_path)

    assert report["arm"] == "c-headroom"
    assert report["totals"]["requests"] == 2
    assert report["totals"]["prompt_tokens"] == 3200
    assert report["totals"]["instances"] == 1
    assert report["totals"]["instances_with_disagreeing_aggregate"] == []
    assert report["totals"]["instances_without_aggregate"] == []
    assert "requests" in report["metric_definition"]["note"]
    assert report["instances"]["instance-a"]["turns"][1]["prompt_tokens"] == 2100


def test_build_usage_report_names_the_instances_whose_readings_did_not_corroborate(tmp_path: Path) -> None:
    _write_store(
        _profile_store(tmp_path, "instance-mismatch"),
        turns=[{"input": 100, "output": 10, "cacheRead": 1000}],
        aggregate={"input": 7, "output": 10, "cacheRead": 1000, "total": 1117},
    )
    _write_store(
        _profile_store(tmp_path, "instance-no-aggregate"),
        turns=[{"input": 100, "output": 10, "cacheRead": 1000}],
        aggregate=None,
    )

    report = build_usage_report(arm="c-headroom", profiles_root=tmp_path)

    assert report["totals"]["instances_with_disagreeing_aggregate"] == ["instance-mismatch"]
    assert report["totals"]["instances_without_aggregate"] == ["instance-no-aggregate"]
