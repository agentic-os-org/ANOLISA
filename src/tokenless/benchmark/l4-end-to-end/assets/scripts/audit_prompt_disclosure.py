#!/usr/bin/env python3
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""Audit how much of each gold patch the guided prompt discloses.

This runs on a benchmark host, not in CI, because it needs two inputs that are
deliberately absent from this repository: the per-instance prompt corpus (passed
to the runner via ``--prompts-dir``) and the SWE-bench_Lite gold patches (read
offline from the local HuggingFace cache). It lets the report quote a
first-party disclosure rate for the corpus a campaign actually consumed, instead
of borrowing a rate from an unrelated earlier corpus.

Disclosure is measured two independent ways because either alone misleads:

- ``names_any_gold_file`` -- does the prompt mention a file the gold patch edits.
  Coarse but unambiguous; a prompt can name the file without giving the fix.
- ``added_line_hit_ratio`` -- of the gold patch's added code lines, the fraction
  found verbatim (whitespace-normalised, trivial lines dropped) in the prompt.
  This is the "did the prompt hand over the fix itself" signal.

Both are reported per instance and summarised; neither is collapsed into a single
"grade", and no L0-L3 level scheme is used -- that vocabulary collided with the
repository's L1-L4 benchmark layers and is intentionally gone.
"""
import argparse
import json
import os
import re
import statistics as st
import sys


def _read(path):
    """Read a UTF-8 text file, tolerating stray bytes in the corpus."""
    with open(path, encoding="utf-8", errors="ignore") as fh:
        return fh.read()


def added_lines(patch):
    """Added code lines of a unified diff, minus the ``+++`` header and trivia.

    Whitespace is collapsed so indentation differences between patch and prompt
    cannot hide a match, and lines with fewer than four non-space characters
    (closing braces, lone keywords) are dropped: they match by coincidence, not
    by disclosure, and would inflate the hit ratio.
    """
    out = []
    for ln in patch.splitlines():
        if ln.startswith("+") and not ln.startswith("+++"):
            body = re.sub(r"\s+", " ", ln[1:].strip())
            if len(body.replace(" ", "")) >= 4:
                out.append(body)
    return out


def gold_files(patch):
    """Paths the gold patch modifies, from its ``diff --git a/... b/...`` headers."""
    return set(re.findall(r"diff --git a/(\S+) b/\S+", patch))


def bucket(ratios):
    """Distribute hit ratios into fixed disclosure bands for the summary table."""
    b = {"0%": 0, "1-25%": 0, "25-50%": 0, "50-75%": 0, "75-100%": 0}
    for r in ratios:
        pct = r * 100
        if pct == 0:
            b["0%"] += 1
        elif pct < 25:
            b["1-25%"] += 1
        elif pct < 50:
            b["25-50%"] += 1
        elif pct < 75:
            b["50-75%"] += 1
        else:
            b["75-100%"] += 1
    return b


def summarise(subset, label):
    """Aggregate one instance subset into the disclosure figures §4 cites."""
    with_lines = [r for r in subset if r["gold_added_lines"]]
    tot_add = sum(r["gold_added_lines"] for r in with_lines)
    tot_hit = sum(r["gold_added_lines_in_prompt"] for r in with_lines)
    ratios = sorted(r["added_line_hit_ratio"] for r in with_lines)
    return {
        "label": label,
        "instances": len(subset),
        "names_any_gold_file": sum(1 for r in subset if r["names_any_gold_file"]),
        "gold_added_lines_total": tot_add,
        "gold_added_lines_in_prompt_total": tot_hit,
        "disclosed_pct_overall": round(tot_hit / tot_add * 100, 1) if tot_add else None,
        "median_added_line_hit_ratio": round(st.median(ratios), 4) if ratios else None,
        "hit_ratio_buckets": bucket(ratios),
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    # Default to the deployed workspace layout. The shard list is installed by
    # remote_fleet.sh as $L4_REMOTE_WORK/l4_instances.txt; the old
    # /root/l4/n48-all.txt default did not exist on the host, so the plain
    # invocation crashed on a missing file.
    work = os.environ.get("L4_REMOTE_WORK", "/root/l4")
    ap.add_argument("--prompts", default=f"{work}/prompts",
                    help="directory of per-instance prompt files (one per instance_id)")
    ap.add_argument("--instances", default=f"{work}/l4_instances.txt",
                    help="file listing the measured instance shard, one id per line")
    ap.add_argument("--dataset", default="princeton-nlp/SWE-bench_Lite")
    ap.add_argument("--out", default="-", help="output path, or - for stdout")
    args = ap.parse_args()

    os.environ.setdefault("HF_DATASETS_OFFLINE", "1")
    os.environ.setdefault("HF_HUB_OFFLINE", "1")
    from datasets import load_dataset

    sel = {x.strip() for x in _read(args.instances).splitlines() if x.strip()}
    if not sel:
        sys.exit(f"no instance ids in {args.instances}; nothing to audit")
    ds = load_dataset(args.dataset, split="test")
    gold = {r["instance_id"]: r["patch"] for r in ds}

    rows = []
    for iid in sorted(os.listdir(args.prompts)):
        pf = os.path.join(args.prompts, iid)
        if not os.path.isfile(pf) or iid not in gold:
            continue
        norm = re.sub(r"\s+", " ", _read(pf))
        adds = added_lines(gold[iid])
        hit = sum(1 for a in adds if a in norm)
        files = gold_files(gold[iid])
        # Count a full-path match and a basename-only match separately. A common
        # basename (__init__.py, setup.py, conftest.py, test_*.py) appears in
        # almost any prompt, so folding basename hits into the headline count
        # systematically overstates disclosure. gold_files_named is therefore the
        # strict full-path count; looser basename-only hits are reported beside
        # it rather than inflating it.
        named_by_path = sum(1 for f in files if f in norm)
        named_by_basename = sum(
            1 for f in files if f not in norm and os.path.basename(f) in norm
        )
        rows.append({
            "instance_id": iid,
            "in_shard": iid in sel,
            "gold_files": len(files),
            "gold_files_named": named_by_path,
            "gold_files_named_by_basename_only": named_by_basename,
            "names_any_gold_file": named_by_path > 0,
            "gold_added_lines": len(adds),
            "gold_added_lines_in_prompt": hit,
            "added_line_hit_ratio": round(hit / len(adds), 4) if adds else None,
        })

    summary = {
        "corpus_prompts_audited": len(rows),
        "all_corpus": summarise(rows, "all corpus prompts"),
        "measured_shard": summarise([r for r in rows if r["in_shard"]],
                                    "the measured instance shard"),
        "method": "added_line_hit_ratio = whitespace-normalised gold '+' lines "
                  "(>=4 non-space chars) found verbatim in the prompt text",
    }
    payload = json.dumps({"summary": summary, "instances": rows}, ensure_ascii=False)
    if args.out == "-":
        sys.stdout.write(payload + "\n")
    else:
        with open(args.out, "w") as fh:
            fh.write(payload + "\n")
    sys.stderr.write(json.dumps(summary, ensure_ascii=False, indent=1) + "\n")


if __name__ == "__main__":
    main()
