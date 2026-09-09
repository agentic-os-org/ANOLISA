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
import hashlib
import json
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
L4 = os.path.normpath(os.path.join(HERE, "..", ".."))


def selected_task_ids(run_dir):
    for path in (
        os.path.join(os.path.dirname(run_dir), "selection.json"),
        os.path.join(run_dir, "run_metadata.json"),
    ):
        if os.path.isfile(path):
            with open(path, encoding="utf-8") as selection_file:
                ids = json.load(selection_file).get("instance_ids")
            if (
                isinstance(ids, list)
                and ids
                and all(isinstance(iid, str) and iid for iid in ids)
            ):
                return set(ids)
    return None


def unscored_cells(evals, runs, seen_runs):
    """保留没有评分报告的选定任务；尚未完成的尝试不能发布解决率。"""
    roots = {runs or evals}
    if runs is None and os.path.basename(os.path.normpath(evals)) == "eval":
        roots.add(os.path.dirname(os.path.normpath(evals)))
    manifests = set()
    for root in roots:
        manifests.update(
            glob.glob(f"{root}/**/out/repeat-*/arm-*/selection.json", recursive=True)
        )
    for path in sorted(manifests):
        arm_dir = os.path.dirname(path)
        run_dir = os.path.join(arm_dir, "run")
        if os.path.abspath(run_dir) in seen_runs:
            continue
        match = re.fullmatch(
            r"repeat-(\d+)", os.path.basename(os.path.dirname(arm_dir))
        )
        arm_match = re.fullmatch(r"arm-([a-z])", os.path.basename(arm_dir))
        if not match or not arm_match:
            continue
        selected = selected_task_ids(run_dir)
        if selected is None:
            raise SystemExit(f"selected task denominator is missing: {path}")
        with open(path, encoding="utf-8") as manifest:
            finished = json.load(manifest).get("attempt_complete") is True
        preds_path = os.path.join(run_dir, "preds.json")
        submitted = 0
        if os.path.isfile(preds_path):
            with open(preds_path, encoding="utf-8") as predictions:
                preds = json.load(predictions)
            if not isinstance(preds, (dict, list)):
                raise SystemExit(f"invalid predictions document: {preds_path}")
            submitted = len(preds)
        no_predictions = finished and submitted == 0
        host_dir = os.path.dirname(os.path.dirname(os.path.dirname(arm_dir)))
        yield {
            "host": os.path.basename(host_dir),
            "repeat": int(match.group(1)),
            "arm": arm_match.group(1),
            "selected": len(selected),
            "submitted": submitted,
            "completed": 0,
            "resolved": [],
            "unresolved": sorted(selected) if no_predictions else [],
            "unscored": [] if no_predictions else sorted(selected),
            "empty_patch": [],
            "error": [],
            "evaluation_status": "no_predictions" if no_predictions else "unscored",
            "prediction_verification": "not_applicable"
            if no_predictions
            else "unverified",
        }


def load_reports(evals: str, runs: str | None = None, allow_legacy: bool = False):
    """Read every per-arm SWE-bench report under a collected eval tree.

    Layout: ``<evals>/<host>/repeat-<n>-arm-<x>/evaluate/<model>.<run-id>.json``.
    The nested ``logs/`` copies are skipped: they are per-instance, not per-arm.
    """
    out = []
    seen_runs = set()
    for path in sorted(
        glob.glob(f"{evals}/**/repeat-*-arm-*/evaluate/*.json", recursive=True)
    ):
        if f"{os.sep}logs{os.sep}" in path:
            continue
        cell_root = os.path.dirname(os.path.dirname(path))
        marker_path = os.path.join(cell_root, ".eval-complete")
        m = re.search(r"repeat-(\d+)-arm-([a-z])", os.path.basename(cell_root))
        if not m:
            continue
        marker = ""
        if os.path.exists(marker_path):
            with open(marker_path, encoding="utf-8") as marker_file:
                marker = marker_file.read().strip()
        if marker == "pending":
            continue
        run_root = os.path.dirname(os.path.dirname(cell_root))
        if runs is not None:
            host_dir = os.path.dirname(os.path.relpath(cell_root, evals))
            if os.path.basename(host_dir) == "eval":
                host_dir = os.path.dirname(host_dir)
            run_root = os.path.join(runs, host_dir)
        run_dir = os.path.join(
            run_root, "out", f"repeat-{m.group(1)}", f"arm-{m.group(2)}", "run"
        )
        verified = False
        if marker:
            completed = marker.split()
            if len(completed) != 2 or not re.fullmatch(r"[0-9a-f]{64}", completed[0]):
                raise SystemExit(f"invalid evaluation completion marker: {marker_path}")
            if not os.path.basename(path).endswith(f".{completed[1]}.json"):
                continue
            preds_path = os.path.join(run_dir, "preds.json")
            if os.path.isfile(preds_path):
                with open(preds_path, "rb") as predictions:
                    digest = hashlib.sha256(predictions.read()).hexdigest()
                if digest != completed[0]:
                    continue
                verified = True
        if not verified and not allow_legacy:
            raise SystemExit(
                f"cannot bind evaluation to current predictions: {path}; supply --runs, "
                "or --allow-legacy for explicitly unverified historical reports"
            )
        host = "unknown"
        hm = re.search(r"(host\d)", path)
        if hm:
            host = hm.group(1)
        try:
            doc = json.load(open(path))
        except (OSError, json.JSONDecodeError):
            continue
        selected = selected_task_ids(run_dir)
        if selected is None and not allow_legacy:
            raise SystemExit(f"selected task denominator is missing: {run_dir}")
        resolved = set(doc.get("resolved_ids") or [])
        if selected is not None and not resolved <= selected:
            raise SystemExit(
                f"evaluation resolved IDs are outside the selected task set: {path}"
            )
        unresolved = (
            selected - resolved
            if selected is not None
            else set(doc.get("unresolved_ids") or [])
        )
        run_key = os.path.abspath(run_dir)
        if run_key in seen_runs:
            raise SystemExit(
                f"duplicate current evaluation report for one cell: {path}"
            )
        seen_runs.add(run_key)
        out.append(
            {
                "host": host,
                "evaluation_status": "scored",
                "unscored": [],
                "selected": len(selected) if selected is not None else None,
                "prediction_verification": "current"
                if verified
                else "legacy_unverified",
                "repeat": int(m.group(1)),
                "arm": m.group(2),
                "submitted": doc.get("submitted_instances"),
                "completed": doc.get("completed_instances"),
                "resolved": sorted(resolved),
                "unresolved": sorted(unresolved),
                "empty_patch": sorted(doc.get("empty_patch_ids") or []),
                "error": sorted(doc.get("error_ids") or []),
            }
        )
    out.extend(unscored_cells(evals, runs, seen_runs))
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--evals",
        default=f"{L4}/runs",
        help="root holding collected per-host eval trees",
    )
    ap.add_argument(
        "--runs",
        help="current collected run tree with the matching per-host out/ directories",
    )
    ap.add_argument(
        "--allow-legacy",
        action="store_true",
        help="explicitly read unverified historical evaluations",
    )
    ap.add_argument("-o", "--out", default=f"{L4}/reports/solve_rate.json")
    args = ap.parse_args()

    reports = load_reports(args.evals, args.runs, args.allow_legacy)
    if not reports:
        raise SystemExit(f"no evaluation reports found under {args.evals}")

    by_repeat_arm = collections.defaultdict(
        lambda: {
            "submitted": 0,
            "selected": 0,
            "resolved": 0,
            "unresolved": [],
            "unscored": [],
            "hosts_scored": 0,
            "empty": [],
            "error": [],
        }
    )
    fail_counter = collections.Counter()
    seen_cells = collections.Counter()
    for r in reports:
        k = (r["repeat"], r["arm"])
        acc = by_repeat_arm[k]
        acc["submitted"] += r["submitted"] or 0
        if r["selected"] is None:
            raise SystemExit(
                "legacy evaluation has no selected-task denominator; only its submitted counts may be inspected"
            )
        acc["selected"] += r["selected"]
        acc["resolved"] += len(r["resolved"])
        acc["unresolved"] += r["unresolved"]
        acc["unscored"] += r["unscored"]
        acc["hosts_scored"] += r["evaluation_status"] == "scored"
        acc["empty"] += r["empty_patch"]
        acc["error"] += r["error"]
        seen_cells[k] += 1
        for iid in r["unresolved"]:
            fail_counter[iid] += 1

    per_arm = {}
    for (rep, arm), acc in sorted(by_repeat_arm.items()):
        sub = acc["submitted"]
        selected = acc["selected"]
        per_arm[f"repeat{rep}-arm{arm}"] = {
            "hosts_attempted": seen_cells[(rep, arm)],
            "hosts_scored": acc["hosts_scored"],
            "evaluation_complete": not acc["unscored"],
            "unscored_ids": sorted(acc["unscored"]),
            "submitted": sub,
            "selected": selected,
            "resolved": acc["resolved"],
            "solve_rate_pct": round(acc["resolved"] / selected * 100, 1)
            if selected and not acc["unscored"]
            else None,
            "unresolved_ids": sorted(acc["unresolved"]),
            "empty_patch_ids": sorted(acc["empty"]),
            "error_ids": sorted(acc["error"]),
        }

    arm_totals = {}
    for arm in sorted({a for _, a in by_repeat_arm}):
        sub = sum(v["submitted"] for k, v in per_arm.items() if k.endswith(f"arm{arm}"))
        res = sum(v["resolved"] for k, v in per_arm.items() if k.endswith(f"arm{arm}"))
        selected = sum(
            v["selected"] for k, v in per_arm.items() if k.endswith(f"arm{arm}")
        )
        complete = all(
            v["evaluation_complete"]
            for k, v in per_arm.items()
            if k.endswith(f"arm{arm}")
        )
        if selected:
            # 成功数不能超过已提交 patch 数；缺失评分的 cell 不得从分母静默消失。
            if res > sub:
                raise SystemExit(
                    f"arm {arm}: resolved {res} > submitted {sub}; an eval report is "
                    "missing submitted_instances -- fix the inputs, do not pool"
                )
            arm_totals[arm] = {
                "submitted": sub,
                "selected": selected,
                "resolved": res,
                "evaluation_complete": complete,
                "solve_rate_pct": round(res / selected * 100, 1) if complete else None,
            }

    doc = {
        "metric": "SWE-bench resolved: FAIL_TO_PASS must flip to passing and "
        "PASS_TO_PASS must stay passing; anything less is unresolved",
        "caveat": "the corpus is guided -- prompts name the file to edit and often "
        "reproduce gold-patch lines -- so this rate characterises the "
        "corpus as much as the agent and is not comparable to published "
        "SWE-bench numbers",
        "prediction_verification": "legacy_unverified"
        if any(row["prediction_verification"] == "legacy_unverified" for row in reports)
        else "incomplete"
        if any(row["evaluation_status"] == "unscored" for row in reports)
        else "current",
        "cells": reports,
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
        print(
            f"  arm {arm}: {v['resolved']}/{v['selected']} selected tasks = {v['solve_rate_pct']}%"
        )
    print(
        "  most frequent failures:",
        ", ".join(
            f"{i['instance_id']}({i['failed_cells']})"
            for i in doc["instances_failing_most_often"][:6]
        )
        or "none",
    )


if __name__ == "__main__":
    main()
