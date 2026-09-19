#!/usr/bin/env python3
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""Quantify run-to-run reproducibility across repeats of the same greedy sweep.

The repeats were executed at the runner's default temperature, which is
``temperature=0``. Nothing here is a sampling error bar: at greedy decoding the
seed is inert, so what varies between repeats is server-side nondeterminism plus
the agent's own reaction to it (tool ordering, retries, wall-clock). That makes
this the right tool to answer "how stable is a single L4 number", and the wrong
tool to answer "what is the variance of the model".

For every arm and every instance measured in two or more repeats it reports the
spread of prompt tokens and requests as a coefficient of variation, plus how often
the agent's success/patch verdict itself flipped between repeats -- a verdict flip
matters more than a token wobble, because it changes which instances survive the
pairing filter and therefore moves every downstream ratio.
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import statistics as st

HERE = os.path.dirname(os.path.abspath(__file__))
L4 = os.path.normpath(os.path.join(HERE, "..", ".."))


def hosts(runs):
    out = {}
    for d in sorted(glob.glob(f"{runs}/host*")):
        out[os.path.basename(d).split("-", 1)[0]] = os.path.basename(d)
    if not out:
        raise SystemExit(f"no host* run trees under {runs}")
    return out


def read_cell(runs, hdir, repeat, arm):
    """Per-instance token/verdict facts for one host x arm x repeat, or None.

    Instances whose per-turn sum disagrees with OpenClaw's own session aggregate
    are dropped here rather than downstream: their token pair is known-corrupt, so
    including them would report harness drift as model drift.
    """
    base = f"{runs}/{hdir}/out/repeat-{repeat}/arm-{arm}"
    rep_path = f"{base}/report.json"
    if not os.path.exists(rep_path):
        return None
    inst = json.load(open(rep_path)).get("instances", {})
    out = {}
    for f in glob.glob(f"{base}/run/results/*.json"):
        res = json.load(open(f))
        iid = res["instance_id"]
        tok = inst.get(iid)
        if not tok or tok.get("aggregate_agrees") is False:
            continue
        out[iid] = {
            "prompt_tokens": tok.get("prompt_tokens"),
            "requests": tok.get("requests"),
            "ok": bool(res.get("success")) and bool(res.get("patch_produced")),
            "duration": res.get("duration_seconds"),
        }
    return out


def cv(values):
    """Coefficient of variation in percent; 0.0 for identical values."""
    # Keep a genuine 0 (e.g. a zero-request instance); only None means "no datum".
    # The old `and v` truthiness test dropped 0 together with None.
    usable = [v for v in values if isinstance(v, (int, float)) and not isinstance(v, bool)]
    if len(usable) < 2:
        return None
    mean = st.mean(usable)
    return round(st.pstdev(usable) / mean * 100, 2) if mean else None


def summarise(series):
    """Aggregate per-instance spreads into one arm-level stability figure."""
    tok = [s for s in (cv(v["prompt_tokens"] for v in i) for i in series) if s is not None]
    req = [s for s in (cv(v["requests"] for v in i) for i in series) if s is not None]
    wall = [s for s in (cv(v["duration"] for v in i) for i in series) if s is not None]
    flips = sum(1 for i in series if len({v["ok"] for v in i}) > 1)
    # A set of all-None prompt tokens has size 1 but is "no data", not "bit
    # identical": require a single concrete (non-None) value.
    identical = sum(
        1 for i in series
        if {v["prompt_tokens"] for v in i} != {None}
        and len({v["prompt_tokens"] for v in i}) == 1
    )
    return {
        "instances_compared": len(series),
        "instances_bit_identical_prompt_tokens": identical,
        "verdict_flips": flips,
        "prompt_tokens_cv_pct": {
            "median": round(st.median(tok), 2) if tok else None,
            "max": max(tok) if tok else None,
        },
        "requests_cv_pct": {
            "median": round(st.median(req), 2) if req else None,
            "max": max(req) if req else None,
        },
        "wall_clock_cv_pct": {
            "median": round(st.median(wall), 2) if wall else None,
            "max": max(wall) if wall else None,
        },
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--runs", default=f"{L4}/runs")
    ap.add_argument("--repeats", default="1,2,3,4,5",
                    help="comma-separated repeat indices to compare")
    ap.add_argument("-o", "--out", default=f"{L4}/reports/reproducibility.json")
    args = ap.parse_args()

    repeats = [int(x) for x in args.repeats.split(",") if x.strip()]
    hosts_map = hosts(args.runs)
    # Arms actually on disk across the requested repeats, not a hardcoded a-d, so
    # a wider sweep is compared rather than silently trimmed.
    arms = sorted({
        os.path.basename(d).split("-", 1)[1]
        for hdir in hosts_map.values()
        for r in repeats
        for d in glob.glob(f"{args.runs}/{hdir}/out/repeat-{r}/arm-*")
        if os.path.basename(d).split("-", 1)[1]
    })

    per_arm, coverage = {}, {}
    for arm in arms:
        series, present = [], {}
        for host, hdir in hosts_map.items():
            cells = {}
            for r in repeats:
                cell = read_cell(args.runs, hdir, r, arm)
                if cell is not None:
                    cells[r] = cell
            present[host] = sorted(cells)
            common = set.intersection(*(set(c) for c in cells.values())) if cells else set()
            for iid in sorted(common):
                obs = [cells[r][iid] for r in sorted(cells)]
                if len(obs) >= 2:
                    series.append(obs)
        coverage[arm] = present
        per_arm[arm] = summarise(series) if series else {"instances_compared": 0}

    doc = {
        "method": "per-instance spread across repeats of the same greedy (temp=0) "
                  "sweep; measures reproducibility and server-side nondeterminism, "
                  "NOT sampling variance -- the seed is inert at temperature 0",
        "repeats_requested": repeats,
        "repeats_present_by_arm_host": coverage,
        "excluded": "instances whose per-turn token sum disagrees with OpenClaw's "
                    "session aggregate (known upstream turn-row loss)",
        "by_arm": per_arm,
    }
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as fh:
        fh.write(json.dumps(doc, indent=2) + "\n")
    print(f"wrote {args.out}")
    for arm, s in per_arm.items():
        print(f"  arm {arm}: n={s.get('instances_compared')} "
              f"flips={s.get('verdict_flips')} "
              f"tok_cv_med={s.get('prompt_tokens_cv_pct', {}).get('median')}")


if __name__ == "__main__":
    main()
