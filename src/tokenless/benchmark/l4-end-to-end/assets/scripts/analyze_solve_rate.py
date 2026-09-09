#!/usr/bin/env python3
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""Aggregate SWE-bench evaluation reports into the solve rate the L4 report cites.

Solve rate is the only end-to-end correctness metric here: an instance is resolved
only when its FAIL_TO_PASS tests go from failing to passing and its PASS_TO_PASS
tests stay passing. Everything else in the report -- tokens, requests, wall-clock --
is economics, and economics without correctness cannot say whether an arm is better.

Emits per-arm rates pooled over hosts for each repeat, plus the instances that
failed. A failure list matters more than the rate on this corpus: the prompts hand
over the answer (see audit_prompt_disclosure.py), so an unresolved instance is
usually a mechanical defect rather than a hard task, and one that recurs across
repeats and arms is a reproducible defect rather than run-to-run noise.
"""
from __future__ import annotations

import argparse
import collections
import glob
import json
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
L4 = os.path.normpath(os.path.join(HERE, "..", ".."))


def load_reports(evals: str):
    """Read every per-arm SWE-bench report under a collected eval tree.

    Layout: ``<evals>/<host>/repeat-<n>-arm-<x>/evaluate/<model>.<run-id>.json``.
    The nested ``logs/`` copies are skipped: they are per-instance, not per-arm.
    """
    out = []
    for path in sorted(glob.glob(f"{evals}/**/repeat-*-arm-*/evaluate/*.json",
                                 recursive=True)):
        if f"{os.sep}logs{os.sep}" in path:
            continue
        # Match any single-letter arm, not just a-d: hardcoding the executed set
        # would silently drop an e/f eval report from the pooled solve rate.
        m = re.search(r"repeat-(\d+)-arm-([a-z])", path)
        if not m:
            continue
        host = "unknown"
        hm = re.search(r"(host\d)", path)
        if hm:
            host = hm.group(1)
        try:
            doc = json.load(open(path))
        except (OSError, json.JSONDecodeError):
            continue
        out.append({
            "host": host,
            "repeat": int(m.group(1)),
            "arm": m.group(2),
            "submitted": doc.get("submitted_instances"),
            "completed": doc.get("completed_instances"),
            "resolved": sorted(doc.get("resolved_ids") or []),
            "unresolved": sorted(doc.get("unresolved_ids") or []),
            "empty_patch": sorted(doc.get("empty_patch_ids") or []),
            "error": sorted(doc.get("error_ids") or []),
        })
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--evals", default=f"{L4}/runs",
                    help="root holding collected per-host eval trees")
    ap.add_argument("-o", "--out", default=f"{L4}/reports/solve_rate.json")
    args = ap.parse_args()

    reports = load_reports(args.evals)
    if not reports:
        raise SystemExit(f"no evaluation reports found under {args.evals}")

    by_repeat_arm = collections.defaultdict(
        lambda: {"submitted": 0, "resolved": 0, "unresolved": [], "empty": [], "error": []})
    fail_counter = collections.Counter()
    seen_cells = collections.Counter()
    for r in reports:
        k = (r["repeat"], r["arm"])
        acc = by_repeat_arm[k]
        acc["submitted"] += r["submitted"] or 0
        acc["resolved"] += len(r["resolved"])
        acc["unresolved"] += r["unresolved"]
        acc["empty"] += r["empty_patch"]
        acc["error"] += r["error"]
        seen_cells[k] += 1
        for iid in r["unresolved"]:
            fail_counter[iid] += 1

    per_arm = {}
    for (rep, arm), acc in sorted(by_repeat_arm.items()):
        sub = acc["submitted"]
        per_arm[f"repeat{rep}-arm{arm}"] = {
            "hosts_scored": seen_cells[(rep, arm)],
            "submitted": sub,
            "resolved": acc["resolved"],
            "solve_rate_pct": round(acc["resolved"] / sub * 100, 1) if sub else None,
            "unresolved_ids": sorted(acc["unresolved"]),
            "empty_patch_ids": sorted(acc["empty"]),
            "error_ids": sorted(acc["error"]),
        }

    arm_totals = {}
    for arm in sorted({a for _, a in by_repeat_arm}):
        sub = sum(v["submitted"] for k, v in per_arm.items() if k.endswith(f"arm{arm}"))
        res = sum(v["resolved"] for k, v in per_arm.items() if k.endswith(f"arm{arm}"))
        if sub:
            # A pooled resolved count above submitted means some host's report was
            # missing submitted_instances (contributing resolved ids while adding
            # 0 to the denominator), which would print a solve rate over 100%.
            # Refuse rather than publish an impossible figure.
            if res > sub:
                raise SystemExit(
                    f"arm {arm}: resolved {res} > submitted {sub}; an eval report is "
                    "missing submitted_instances -- fix the inputs, do not pool"
                )
            arm_totals[arm] = {"submitted": sub, "resolved": res,
                               "solve_rate_pct": round(res / sub * 100, 1)}

    doc = {
        "metric": "SWE-bench resolved: FAIL_TO_PASS must flip to passing and "
                  "PASS_TO_PASS must stay passing; anything less is unresolved",
        "caveat": "the corpus is guided -- prompts name the file to edit and often "
                  "reproduce gold-patch lines -- so this rate characterises the "
                  "corpus as much as the agent and is not comparable to published "
                  "SWE-bench numbers",
        "per_repeat_arm": per_arm,
        "pooled_by_arm_over_repeats": arm_totals,
        "instances_failing_most_often": [
            {"instance_id": iid, "failed_cells": n}
            for iid, n in fail_counter.most_common(20)
        ],
    }
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as fh:
        fh.write(json.dumps(doc, indent=2) + "\n")
    print(f"wrote {args.out}")
    for arm, v in sorted(arm_totals.items()):
        print(f"  arm {arm}: {v['resolved']}/{v['submitted']} = {v['solve_rate_pct']}%")
    print("  most frequent failures:",
          ", ".join(f"{i['instance_id']}({i['failed_cells']})"
                    for i in doc["instances_failing_most_often"][:6]) or "none")


if __name__ == "__main__":
    main()
