#!/usr/bin/env python3
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

"""Single source of truth for benchmark input data.

Both the Rust latency benches and the Python compression-rate scripts must run
on byte-identical inputs, otherwise "latency" and "compression rate" describe
different workloads and cannot be attributed to the same payload. This script
generates the canonical fixtures once; everything else loads them.

Outputs (under ``fixtures/``, pretty-printed, deterministic — no RNG):
    records.json        1000 uniform records; backs response_items(n) latency.
    record_reduction.json
                        64 records with a critical anomaly in the middle.
    tool_response.json  a realistic tool result (envelope + first 60 records +
                        trace/logs); the canonical response payload measured
                        for BOTH latency (Rust) and compression rate (Python).
    schema_search.json  the canonical function-calling schema; measured for
                        BOTH latency (Rust) and compression rate (Python).

Regenerate with:  python3 gen_fixtures.py
Fixtures are committed so CI and the Rust `include_str!` build do not depend on
Python being run first.
"""

from __future__ import annotations

import json
from pathlib import Path

FIXTURES_DIR = Path(__file__).resolve().parent.parent / "fixtures"

# Number of canonical records. Large enough that response_items(1000) — the
# heaviest latency point — is served entirely from the fixture.
RECORD_COUNT = 1000

# How many records the canonical tool response embeds. The surrounding and
# per-record diagnostic fields give lossless cleanup measurable work to do.
TOOL_RESPONSE_ITEMS = 60

# A focused Record Reduction fixture kept separate from the canonical response
# so existing benchmark inputs remain byte-identical.
RECORD_REDUCTION_ITEMS = 64

# Rounds in a simulated agent session (cost analysis reuses the canonical
# response, varying only the tool index).
SESSION_ROUNDS = 50


def _record(i: int) -> dict:
    """One canonical record.

    Fields combine removable diagnostics with scalar columns that TOON can
    encode into a compact table.
    """
    return {
        "id": i,
        "name": f"item-{i}",
        "path": f"src/module_{i % 20}/file_{i}.rs",
        "status": "ok" if i % 2 == 0 else "pending",
        "score": round((i % 97) / 13.0, 4),
        "snippet": "matched line of source code with some surrounding context " * 2,
        "debug": "verbose internal trace that should be dropped " * 2,
    }


def build_records() -> list[dict]:
    return [_record(i) for i in range(RECORD_COUNT)]


def build_tool_response(records: list[dict]) -> dict:
    """Envelope the first ``TOOL_RESPONSE_ITEMS`` records with trace/log noise."""
    return {
        "tool": "search_code",
        "status": "ok",
        "results": records[:TOOL_RESPONSE_ITEMS],
        "trace": "step-by-step debug trace of the tool invocation " * 10,
        "logs": [f"log entry number {i}" for i in range(40)],
    }


def build_record_reduction() -> list[dict]:
    """Records with an error and numeric anomaly away from either boundary."""
    records = [
        {
            "id": i,
            "status": "ok",
            "latency_ms": 10,
            "message": f"request-{i} " + "completed normally " * 6,
        }
        for i in range(RECORD_REDUCTION_ITEMS)
    ]
    records[31]["status"] = "failed"
    records[31]["latency_ms"] = 10_000
    records[31]["message"] = "critical anomaly in the middle of the collection"
    return records


def build_schema() -> dict:
    """Canonical OpenAI function-calling schema with verbose descriptions.

    Descriptions are padded past the compressor's truncation limits so schema
    compression does measurable work.
    """
    return {
        "function": {
            "name": "search_code",
            "description": "Search the codebase for a query string. " * 20,
            "parameters": {
                "type": "object",
                "title": "SearchParams",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The query. " * 15,
                        "examples": ["foo"],
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results. " * 15,
                    },
                },
            },
        }
    }


def _write(name: str, value: object) -> None:
    path = FIXTURES_DIR / name
    # Stable formatting so a regenerated fixture only diffs on real changes.
    text = json.dumps(value, indent=2, ensure_ascii=False, sort_keys=True)
    path.write_text(text + "\n", encoding="utf-8")
    print(f"wrote {path.relative_to(FIXTURES_DIR.parent)} ({len(text)} bytes)")


def main() -> None:
    FIXTURES_DIR.mkdir(parents=True, exist_ok=True)
    records = build_records()
    _write("records.json", records)
    _write("record_reduction.json", build_record_reduction())
    _write("tool_response.json", build_tool_response(records))
    _write("schema_search.json", build_schema())


if __name__ == "__main__":
    main()
