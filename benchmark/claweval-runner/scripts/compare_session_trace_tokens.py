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

"""Compare token usage between openclaw session.jsonl and ce-runner trace.jsonl across all trials."""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def session_totals(path: Path) -> dict:
    n_assist = 0
    in_sum = out_sum = total_sum = cache_r = cache_w = 0
    last_total = None
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        msg = rec.get("message")
        if not isinstance(msg, dict) or msg.get("role") != "assistant":
            continue
        u = msg.get("usage") or {}
        n_assist += 1
        in_sum += int(u.get("input", 0) or 0)
        out_sum += int(u.get("output", 0) or 0)
        total_sum += int(u.get("totalTokens", 0) or 0)
        cache_r += int(u.get("cacheRead", 0) or 0)
        cache_w += int(u.get("cacheWrite", 0) or 0)
        last_total = u.get("totalTokens")
    return {
        "assistants": n_assist,
        "input": in_sum,
        "output": out_sum,
        "total": total_sum,
        "cache_read": cache_r,
        "cache_write": cache_w,
        "last_total": last_total,
    }


def trace_totals(path: Path) -> dict:
    n_assist = 0
    in_sum = out_sum = 0
    end = None
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        t = rec.get("type")
        if t == "message":
            msg = rec.get("message") or {}
            if msg.get("role") == "assistant":
                u = rec.get("usage") or {}
                n_assist += 1
                in_sum += int(u.get("input_tokens", 0) or 0)
                out_sum += int(u.get("output_tokens", 0) or 0)
        elif t == "trace_end":
            end = rec
    out = {
        "assistants": n_assist,
        "input": in_sum,
        "output": out_sum,
        "end_input": None,
        "end_output": None,
        "end_total": None,
    }
    if end:
        out["end_input"] = end.get("model_input_tokens")
        out["end_output"] = end.get("model_output_tokens")
        out["end_total"] = end.get("total_tokens")
    return out


def find_pairs(run_dir: Path):
    sess_dir = run_dir / "sessions"
    if not sess_dir.is_dir():
        return []
    pairs = []
    for sess in sorted(sess_dir.glob("*.session.jsonl")):
        stem = sess.name[: -len(".session.jsonl")]
        trace = run_dir / f"{stem}.jsonl"
        pairs.append((stem, sess, trace if trace.exists() else None))
    return pairs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default="claw-eval/traces", help="traces root dir")
    ap.add_argument("--run", help="only one run dir name; default: scan all")
    ap.add_argument("--detail", action="store_true", help="print per-trial rows even when matched")
    ap.add_argument("--csv", help="optional CSV output path")
    args = ap.parse_args()

    root = Path(args.root)
    if not root.is_dir():
        print(f"no such dir: {root}", file=sys.stderr)
        sys.exit(1)

    runs = [root / args.run] if args.run else sorted(p for p in root.iterdir() if p.is_dir())

    rows = []
    summary = {
        "total_pairs": 0,
        "matched": 0,
        "missing_trace": 0,
        "input_mismatch": 0,
        "output_mismatch": 0,
        "assistants_mismatch": 0,
        "trace_end_mismatch": 0,
        "cache_nonzero": 0,
    }

    for run_dir in runs:
        pairs = find_pairs(run_dir)
        if not pairs:
            continue
        for stem, sess, trace in pairs:
            summary["total_pairs"] += 1
            srow = session_totals(sess)
            if trace is None:
                summary["missing_trace"] += 1
                rows.append((run_dir.name, stem, srow, None, "missing_trace"))
                continue
            trow = trace_totals(trace)

            issues = []
            if srow["assistants"] != trow["assistants"]:
                issues.append("assistants")
                summary["assistants_mismatch"] += 1
            if srow["input"] != trow["input"]:
                issues.append("input")
                summary["input_mismatch"] += 1
            if srow["output"] != trow["output"]:
                issues.append("output")
                summary["output_mismatch"] += 1
            if (
                trow["end_input"] is not None
                and (
                    trow["end_input"] != trow["input"]
                    or trow["end_output"] != trow["output"]
                )
            ):
                issues.append("trace_end")
                summary["trace_end_mismatch"] += 1
            if srow["cache_read"] or srow["cache_write"]:
                summary["cache_nonzero"] += 1
                issues.append("cache_nonzero")

            if not issues:
                summary["matched"] += 1
            rows.append((run_dir.name, stem, srow, trow, ",".join(issues) if issues else "ok"))

    print("=== Summary ===")
    for k, v in summary.items():
        print(f"  {k}: {v}")

    bad = [r for r in rows if r[4] != "ok"]
    if bad:
        print(f"\n=== {len(bad)} trials with discrepancies ===")
        for run, stem, s, t, why in bad:
            si = s["input"]
            so = s["output"]
            sa = s["assistants"]
            if t is None:
                print(f"  [{why}] {run}/{stem}  session(in={si}, out={so}, n={sa})  trace=MISSING")
            else:
                print(
                    f"  [{why}] {run}/{stem}  "
                    f"session(in={si}, out={so}, n={sa})  "
                    f"trace(in={t['input']}, out={t['output']}, n={t['assistants']}, "
                    f"end_in={t['end_input']}, end_out={t['end_output']})"
                )

    if args.detail:
        print("\n=== All rows ===")
        for run, stem, s, t, why in rows:
            print(f"  [{why}] {run}/{stem}  s_in={s['input']} s_out={s['output']} "
                  f"t_in={(t or {}).get('input')} t_out={(t or {}).get('output')}")

    if args.csv:
        import csv
        with open(args.csv, "w", newline="", encoding="utf-8") as f:
            w = csv.writer(f)
            w.writerow([
                "run", "trial", "status",
                "session_assistants", "session_input", "session_output", "session_total",
                "session_cache_read", "session_cache_write",
                "trace_assistants", "trace_input", "trace_output",
                "trace_end_input", "trace_end_output", "trace_end_total",
            ])
            for run, stem, s, t, why in rows:
                w.writerow([
                    run, stem, why,
                    s["assistants"], s["input"], s["output"], s["total"],
                    s["cache_read"], s["cache_write"],
                    (t or {}).get("assistants"), (t or {}).get("input"), (t or {}).get("output"),
                    (t or {}).get("end_input"), (t or {}).get("end_output"), (t or {}).get("end_total"),
                ])
        print(f"\nCSV written: {args.csv}")


if __name__ == "__main__":
    main()
