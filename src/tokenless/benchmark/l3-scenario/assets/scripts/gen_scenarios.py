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

"""One-time generator for the L3 scenario assets.

Mirrors the reference's own two benchmark suites and keeps their structure, so
L3 results can be cross-referenced against the reference's published numbers
instead of being reorganised into a taxonomy only this repo uses:

  assets/pipeline/   <- benchmarks/bench_transforms.py::TestPipelinePerformance
                       (simple / agentic / rag, each with the reference's target)
  assets/scenario/   <- benchmarks/bench_latency.py::generate_scenarios()
                       (grouped by the reference's own content_type: json, code,
                        text, logs, agentic, rag)

The two suites sit side by side under assets/ so the division stays visible,
while code stays in src/ — the same split the L1 and L2 crates use.

One asset is NOT the reference-native and is labelled as such in its `source` block:
`assets/scenario/schema/tools_20.json`. The four-layer plan's L3 table defines
the single-turn case as "system prompt + 20 tool schemas + one exchange"
targeting SchemaCompressor, but the reference's fixtures carry no tools array
(it compacts tool schemas in its proxy layer, not in the message
transforms its benchmarks exercise). Without it, one of tokenless' four
compressors would never appear anywhere in L3.

Run this deliberately and review the diff; do NOT wire it into the benchmark
run. the reference's `generate_agentic_conversation` mints tool-call ids with
`uuid.uuid4()`, which `random.seed(42)` does not constrain, so generating at run
time would produce a different payload every run: different token counts,
nothing comparable across runs or between the two compressor sides. Committing
the output is what makes the comparison reproducible.

Usage:

    python3 assets/scripts/gen_scenarios.py --headroom ~/git_repo/the reference
"""

from __future__ import annotations

import argparse
import json
import pathlib
import random
import subprocess
import sys
from typing import Any


def headroom_revision(root: pathlib.Path) -> tuple[str | None, bool | None]:
    """Revision and tracked-dirty state of the reference checkout, best effort.

    A wheel install or a missing git binary yields (None, None), recorded as
    unknown rather than silently claiming the generator was pinned.
    """
    base = ["git", "-c", f"safe.directory={root}", "-C", str(root)]
    try:
        rev = subprocess.run(
            [*base, "rev-parse", "HEAD"], capture_output=True, text=True, timeout=10
        )
        if rev.returncode != 0:
            return None, None
        status = subprocess.run(
            [*base, "status", "--porcelain", "--untracked-files=no"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        dirty = bool(status.stdout.strip()) if status.returncode == 0 else None
        return rev.stdout.strip(), dirty
    except (OSError, subprocess.SubprocessError):
        return None, None


# --- pipeline suite ---------------------------------------------------------

# The reference's own latency targets and context limits for the pipeline tests,
# carried into the asset so the report cites the reference system's expectation
# rather than inventing one.
PIPELINE_META = {
    "simple": {
        "target_ms": 5.0,
        "model_limit": 100000,
        "fixture": "messages_with_system_date",
        "description": (
            "Minimal conversation: system prompt carrying a date, one question, "
            "one answer. the reference uses it to exercise CacheAligner's date "
            "detection; it carries no tools array and no tool messages."
        ),
    },
    "agentic": {
        "target_ms": 30.0,
        "model_limit": 50000,
        "fixture": "conversation_50_turns",
        "description": (
            "50 turns with ~2 tool calls each and 50 items per tool response."
        ),
    },
    "rag": {
        "target_ms": 50.0,
        "model_limit": 30000,
        "fixture": "rag_conversation_20k",
        "description": (
            "~20K tokens of injected context documents followed by 5 Q&A turns. "
            "The payload sits in a user message, not a tool message."
        ),
    },
}


def system_prompt_with_date() -> str:
    """Verbatim copy of the reference's ``system_prompt_with_date`` fixture.

    Copied rather than imported because it is a pytest fixture, not part of any
    importable module; the date is what CacheAligner is meant to detect.
    """
    return """You are a helpful AI assistant.

Current date: 2025-01-06
Today is Monday, January 6th, 2025.

You have access to various tools for searching and querying data.
Always provide accurate and helpful responses."""


def build_pipeline(mod: Any) -> dict[str, list[dict]]:
    """The three conversations behind the reference's TestPipelinePerformance."""
    out: dict[str, list[dict]] = {}

    out["simple"] = [
        {"role": "system", "content": system_prompt_with_date()},
        {"role": "user", "content": "What's the current date?"},
        {"role": "assistant", "content": "Today is January 6th, 2025."},
    ]

    random.seed(42)
    out["agentic"] = mod.generate_agentic_conversation(
        turns=50, tool_calls_per_turn=2, items_per_tool_response=50
    )

    random.seed(42)
    out["rag"] = mod.generate_rag_conversation(context_tokens=20000, num_queries=5)

    return out


# --- scenario suite ---------------------------------------------------------

# The reference's generate_scenarios() reuses one 4-message skeleton for every
# single-content-type case, putting the payload in a tool message. Replicated
# here (rather than imported) because it is a module-private helper.
def wrap_as_tool_message(content: str) -> list[dict[str, Any]]:
    """the reference's ``_wrap_as_tool_message``: payload in a tool message."""
    return [
        {
            "role": "system",
            "content": "You are a helpful assistant.\n\nCurrent date: 2025-01-06",
        },
        {"role": "user", "content": "Analyze the following data."},
        {
            "role": "assistant",
            "content": None,
            "tool_calls": [
                {
                    "id": "call_bench_1",
                    "type": "function",
                    "function": {"name": "get_data", "arguments": "{}"},
                }
            ],
        },
        {"role": "tool", "tool_call_id": "call_bench_1", "content": content},
    ]


def build_scenario(mod: Any, sc: Any) -> list[dict[str, Any]]:
    """Every scenario point of the reference's generate_scenarios(), grouped by its
    own content_type. Sizes and payload construction follow bench_latency.py.
    """
    out: list[dict[str, Any]] = []
    random.seed(42)

    # json -- SmartCrusher path
    for n, label in [(100, "100 items"), (500, "500 items"), (1_000, "1K items"), (5_000, "5K items")]:
        out.append(("json", f"search_results_{n}", f"JSON: Search Results ({label})", label,
                    wrap_as_tool_message(json.dumps(sc.generate_search_results(n))), None))
    out.append(("json", "api_responses_500", "JSON: API Responses (500 items)", "500 items",
                wrap_as_tool_message(json.dumps(sc.generate_api_responses(500))), None))
    out.append(("json", "database_rows_1k", "JSON: Database Rows (1K rows)", "1K rows",
                wrap_as_tool_message(json.dumps(sc.generate_database_rows(1_000, table_type="metrics"))), None))
    for n, label in [(100, "100 strings"), (500, "500 strings"), (1_000, "1K strings")]:
        strings = [f"GET /api/endpoint_{i % 20} 200 OK" for i in range(n)]
        for j in range(0, n, max(1, n // 5)):
            strings[j] = f"GET /api/endpoint_{j} 500 error: internal server error"
        out.append(("json", f"string_array_{n}", f"JSON: String Array ({label})", label,
                    wrap_as_tool_message(json.dumps(strings)), None))
    for n, label in [(200, "200 numbers"), (1_000, "1K numbers")]:
        numbers = [42.0 + random.gauss(0, 5) for _ in range(n)]
        numbers[n // 4] = 999.9
        numbers[3 * n // 4] = -500.0
        out.append(("json", f"number_array_{n}", f"JSON: Number Array ({label})", label,
                    wrap_as_tool_message(json.dumps(numbers)), None))
    mixed = ([{"id": i, "status": "active"} for i in range(100)]
             + [f"log: request {i} completed" for i in range(100)]
             + [random.gauss(50, 10) for _ in range(50)])
    out.append(("json", "mixed_array_250", "JSON: Mixed Array (250 items)", "250 items",
                wrap_as_tool_message(json.dumps(mixed)), None))
    flat_obj = {f"config_{i}": f"value_{i} " * 20 for i in range(100)}
    out.append(("json", "flat_object_100", "JSON: Flat Object (100 keys)", "100 keys",
                wrap_as_tool_message(json.dumps(flat_obj)), None))
    nested = {
        "search_results": sc.generate_search_results(200),
        "log_entries": [f"INFO: processed request {i}" for i in range(100)],
        "metrics": [random.gauss(50, 5) for _ in range(300)],
        "metadata": {"total": 600, "query": "benchmark test"},
    }
    out.append(("json", "nested_object_600", "JSON: Nested Object (3 arrays)", "600 items nested",
                wrap_as_tool_message(json.dumps(nested)), None))

    # code -- CodeCompressor path. Payload is raw Python, not JSON.
    for lines, label in [(50, "~50 lines"), (200, "~200 lines"), (500, "~500 lines"), (1_000, "~1K lines")]:
        out.append(("code", f"python_{lines}", f"Code: Python ({label})", label,
                    wrap_as_tool_message(mod.generate_python_code(lines)), None))

    # text -- TextCrusher/Kompress path. Payload is raw prose, not JSON.
    for tokens, label in [(1_000, "1K tokens"), (5_000, "5K tokens"), (20_000, "20K tokens"), (50_000, "50K tokens")]:
        out.append(("text", f"documentation_{tokens}", f"Text: Documentation ({label})", label,
                    wrap_as_tool_message(mod.generate_plain_text(tokens)), None))

    # logs -- LogCompressor path. JSON array, so tokenless can act on it too.
    for n, label in [(100, "100 entries"), (500, "500 entries"), (1_000, "1K entries"), (5_000, "5K entries")]:
        out.append(("logs", f"structured_{n}", f"Logs: Structured ({label})", label,
                    wrap_as_tool_message(json.dumps(sc.generate_log_entries(n))), None))

    # agentic -- real multi-turn conversations, model_limit forces crushing.
    for turns, items, label in [(10, 50, "10 turns"), (25, 50, "25 turns"), (50, 50, "50 turns"), (100, 30, "100 turns")]:
        random.seed(42)
        msgs = mod.generate_agentic_conversation(
            turns=turns, tool_calls_per_turn=2, items_per_tool_response=items
        )
        out.append(("agentic", f"multi_tool_{turns}t", f"Agentic: Multi-tool ({label})", label,
                    msgs, max(50_000, turns * 2_000)))

    # rag -- payload sits in a user message.
    for tokens, queries, label in [(5_000, 3, "5K context"), (20_000, 5, "20K context"), (50_000, 5, "50K context")]:
        random.seed(42)
        out.append(("rag", f"document_qa_{tokens}", f"RAG: Document QA ({label})", label,
                    mod.generate_rag_conversation(context_tokens=tokens, num_queries=queries), None))

    return out


# --- added scenario: tool schemas (NOT the reference-native) ---------------------

# Twenty OpenAI Function Calling schemas shaped like a real coding agent's tool
# set: verbose descriptions, nested parameter objects, and the annotation keys
# (title/examples/markdown) both products' schema compaction targets.
_TOOL_SPECS = [
    ("search_code", "Search code repositories for a pattern", ["pattern", "path", "glob"]),
    ("read_file", "Read the contents of a file from disk", ["path", "start_line", "end_line"]),
    ("write_file", "Write content to a file, creating it if absent", ["path", "content"]),
    ("run_tests", "Execute a test suite and return the results", ["suite", "filter", "verbose"]),
    ("query_database", "Run a read-only SQL query against the app database", ["sql", "limit"]),
    ("get_logs", "Retrieve service logs for a time window", ["service", "since", "level"]),
    ("get_metrics", "Fetch time-series metrics for a service", ["service", "metric", "window"]),
    ("list_files", "List files under a directory", ["path", "recursive"]),
    ("git_diff", "Show the diff between two revisions", ["base", "head", "paths"]),
    ("git_log", "List commits reachable from a revision", ["revision", "limit"]),
    ("create_issue", "Open a tracker issue", ["title", "body", "labels"]),
    ("update_issue", "Update an existing tracker issue", ["number", "state", "body"]),
    ("send_email", "Send an email to a recipient list", ["to", "subject", "body"]),
    ("schedule_task", "Schedule a task for later execution", ["cron", "command"]),
    ("http_request", "Perform an outbound HTTP request", ["method", "url", "headers", "body"]),
    ("parse_json", "Parse a JSON document and extract a path", ["document", "json_path"]),
    ("format_code", "Format source code with the project formatter", ["path", "language"]),
    ("lint_code", "Run the linter over a path", ["path", "rules"]),
    ("build_project", "Build the project and report failures", ["target", "release"]),
    ("deploy_service", "Deploy a service to an environment", ["service", "environment", "version"]),
]


def build_tool_schemas() -> list[dict[str, Any]]:
    """Twenty tool schemas carrying the annotations schema compaction removes."""
    tools: list[dict[str, Any]] = []
    for name, summary, params in _TOOL_SPECS:
        properties: dict[str, Any] = {}
        for p in params:
            properties[p] = {
                "type": "string",
                "title": p.replace("_", " ").title(),
                "description": (
                    f"The {p.replace('_', ' ')} argument for {name}. "
                    f"Provide a well-formed value; **markdown** is supported in "
                    f"free-text fields. See the guide for details on how {p} "
                    f"interacts with the other parameters of this tool."
                ),
                "examples": [f"example_{p}_1", f"example_{p}_2"],
            }
        tools.append(
            {
                "type": "function",
                "function": {
                    "name": name,
                    "description": (
                        f"{summary}. This tool is part of the standard agent "
                        f"toolset. It validates its arguments, reports actionable "
                        f"errors, and never mutates state unless documented. "
                        f"Prefer it over shelling out when applicable."
                    ),
                    "parameters": {
                        "type": "object",
                        "title": f"{name} parameters",
                        "properties": properties,
                        "required": params[:1],
                    },
                },
            }
        )
    return tools


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--headroom", required=True, type=pathlib.Path,
                        help="local reference checkout providing the generators")
    parser.add_argument("--out", type=pathlib.Path,
                        default=pathlib.Path(__file__).resolve().parents[1],
                        help="directory holding pipeline/ and scenario/ (default: assets/)")
    args = parser.parse_args()

    root = args.headroom.expanduser().resolve()
    if not (root / "benchmarks" / "scenarios" / "conversations.py").exists():
        print(f"error: {root} is not a reference checkout", file=sys.stderr)
        return 1

    sys.path.insert(0, str(root))
    from benchmarks import bench_latency as mod  # noqa: PLC0415
    from benchmarks.scenarios import tool_outputs as sc  # noqa: PLC0415
    from benchmarks.scenarios import conversations as conv  # noqa: PLC0415

    mod.generate_agentic_conversation = conv.generate_agentic_conversation
    mod.generate_rag_conversation = conv.generate_rag_conversation

    revision, dirty = headroom_revision(root)

    def provenance(reference: str, native: bool = True) -> dict[str, Any]:
        return {
            "reference": reference,
            "headroom_native": native,
            "headroom_revision": revision,
            "headroom_dirty": dirty,
        }

    def write(path: pathlib.Path, payload: dict[str, Any]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(payload, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")

    count = 0

    # -- pipeline suite --
    for name, messages in build_pipeline(mod).items():
        meta = PIPELINE_META[name]
        write(args.out / "pipeline" / f"{name}.json", {
            "suite": "pipeline",
            "scenario": name,
            "content_type": name,
            "description": meta["description"],
            "source": provenance(
                "the reference benchmarks/bench_transforms.py::TestPipelinePerformance"
                f"::test_pipeline_{name} (fixture {meta['fixture']})"
            ),
            "headroom_target_ms": meta["target_ms"],
            "model_limit": meta["model_limit"],
            "message_count": len(messages),
            "tool_message_count": sum(1 for m in messages if m.get("role") == "tool"),
            "messages": messages,
        })
        count += 1

    # -- scenario suite --
    for ctype, slug, name, size_label, messages, limit in build_scenario(mod, sc):
        write(args.out / "scenario" / ctype / f"{slug}.json", {
            "suite": "scenario",
            "scenario": slug,
            "display_name": name,
            "content_type": ctype,
            "size_label": size_label,
            "source": provenance(
                "the reference benchmarks/bench_latency.py::generate_scenarios()"
            ),
            "model_limit": limit if limit is not None else 200_000,
            "message_count": len(messages),
            "tool_message_count": sum(1 for m in messages if m.get("role") == "tool"),
            "messages": messages,
        })
        count += 1

    # -- added: tool schemas --
    tools = build_tool_schemas()
    write(args.out / "scenario" / "schema" / "tools_20.json", {
        "suite": "scenario",
        "scenario": "tools_20",
        "display_name": "Schema: 20 Tool Definitions",
        "content_type": "schema",
        "size_label": "20 tools",
        "source": provenance(
            "four-layer plan L3 table, single-turn row: 'system prompt + 20 tool "
            "schemas + one exchange' targeting SchemaCompressor. Added here "
            "because the reference's benchmark fixtures carry no tools array, so "
            "without it neither side's schema compaction appears in L3.",
            native=False,
        ),
        "model_limit": 200_000,
        "message_count": 2,
        "tool_message_count": 0,
        "tool_count": len(tools),
        "tools": tools,
        "messages": [
            {"role": "system", "content": system_prompt_with_date()},
            {"role": "user", "content": "Find where the retry policy is configured."},
        ],
    })
    count += 1

    print(f"wrote {count} scenario assets from the reference {revision or 'unknown'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
