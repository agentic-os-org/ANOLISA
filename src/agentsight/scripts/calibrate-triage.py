#!/usr/bin/env python3
"""Calibrate the session-level triage thresholds against a real trajectories.db.

The "useless" rule decides which trajectories leave the retrieval scope, so its
thresholds must be set from measured data rather than guessed. This script
reports the bucket distribution and prints the trajectories a given threshold
would exclude, so the excluded set can be eyeballed for false kills before the
rule is switched on.

Only the structural inputs are computed here (tool calls, user turns, answer
length). The grounding findings count is deliberately left out: it comes from
the Rust layer (`server::causal::grounding::evidence::build_index`) and cannot
be reproduced faithfully in Python.

Usage:
    calibrate-triage.py <path-to-trajectories.db> [--max-agent-len N] [--samples N]

Read-only: the database is opened with `mode=ro`. A WAL-mode database needs a
writable directory for its `-shm` file, so copy it somewhere writable first if
the original lives on a read-only mount.
"""

import argparse
import json
import sqlite3
import sys
from collections import Counter

#: Default answer-length ceiling below which a tool-less single-turn session is
#: considered to carry nothing reusable. Deliberately a parameter, not a
#: constant: §5.2 of the design requires it to be set from measured data.
DEFAULT_MAX_AGENT_LEN = 2000


def measure(atif_json):
    """Extract the structural triage inputs from one ATIF document."""
    doc = json.loads(atif_json)
    steps = doc.get("steps") or []
    return {
        "n_steps": len(steps),
        "n_user_turns": sum(1 for s in steps if s.get("source") == "user"),
        "n_tool_calls": sum(len(s.get("tool_calls") or []) for s in steps),
        "max_agent_len": max(
            (len(s.get("message") or "") for s in steps if s.get("source") == "agent"),
            default=0,
        ),
    }


def classify(m, max_agent_len):
    """Bucket one trajectory. Mirrors the rule in §5.2 of the design."""
    if m["n_steps"] <= 1 or m["max_agent_len"] == 0:
        return "empty"
    # More than one user turn means the topic was carried forward, which the
    # design treats as evidence of value regardless of the other inputs.
    if m["n_tool_calls"] == 0 and m["n_user_turns"] <= 1 and m["max_agent_len"] < max_agent_len:
        return "useless"
    return "substantive"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("db", help="path to trajectories.db (opened read-only)")
    ap.add_argument("--max-agent-len", type=int, default=DEFAULT_MAX_AGENT_LEN)
    ap.add_argument("--samples", type=int, default=25, help="excluded rows to print")
    args = ap.parse_args()

    try:
        conn = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
        rows = conn.execute(
            "SELECT session_id, first_user_message, atif_json FROM collected_trajectories"
        ).fetchall()
    except sqlite3.Error as e:
        print(f"cannot read {args.db}: {e}", file=sys.stderr)
        return 1

    buckets = Counter()
    excluded = []
    tool_dist = Counter()
    unparsable = 0

    for _session_id, first_msg, atif_json in rows:
        try:
            m = measure(atif_json)
        except (json.JSONDecodeError, AttributeError, TypeError):
            unparsable += 1
            continue

        bucket = classify(m, args.max_agent_len)
        buckets[bucket] += 1
        if bucket != "substantive":
            excluded.append((bucket, m, (first_msg or "")[:70]))

        n = m["n_tool_calls"]
        tool_dist["0" if n == 0 else "1-5" if n <= 5 else "6-20" if n <= 20 else "20+"] += 1

    total = len(rows)
    if total == 0:
        print("database has no collected trajectories", file=sys.stderr)
        return 1

    print(f"total={total} unparsable={unparsable} max_agent_len={args.max_agent_len}")
    for name in ("substantive", "useless", "empty"):
        n = buckets[name]
        print(f"  {name:12s} {n:5d}  ({100.0 * n / total:.1f}%)")

    excluded_pct = 100.0 * (buckets["useless"] + buckets["empty"]) / total
    print(f"\nwould be excluded from retrieval: {excluded_pct:.1f}%")
    if excluded_pct > 30.0:
        print("  WARNING: above the 30% ceiling — threshold is too wide (design §14)")

    print(f"\n=== excluded samples (up to {args.samples}) ===")
    for bucket, m, preview in excluded[: args.samples]:
        print(
            f"[{bucket:10s}] steps={m['n_steps']:3d} users={m['n_user_turns']:2d} "
            f"tools={m['n_tool_calls']:3d} answer={m['max_agent_len']:6d} | {preview!r}"
        )

    print("\n=== tool_call count distribution ===")
    for key in ("0", "1-5", "6-20", "20+"):
        print(f"  tools={key:5s} {tool_dist[key]:5d}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
