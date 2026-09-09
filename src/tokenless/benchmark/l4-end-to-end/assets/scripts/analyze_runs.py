#!/usr/bin/env python3
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Recompute every figure the L4 report cites, from the recovered run trees, into
# reports/report.json. The markdown report quotes this file and nothing else, so
# the prose and the numbers cannot drift apart.
#
# Usage (from the anolisa repo root, or with --runs pointing at a run tree):
#     python3 src/tokenless/benchmark/l4-end-to-end/assets/scripts/analyze_runs.py
#
# The run trees themselves are gitignored and machine-specific: this script is
# committed, its inputs are not. It is read-only with respect to the run trees.
"""Emit reports/report.json for the L4 end-to-end report."""
from __future__ import annotations

import argparse
import glob
import json
import os
import random
import re
import statistics as st
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
L4 = os.path.normpath(os.path.join(HERE, "..", ".."))
ARMS = ["a", "b", "c", "d"]
# Arms that route through a headroom proxy (everything but the baseline `a` and
# the tokenless hook `b`); only these have a proxy request log to read. Kept in
# step with DESIGN["arm_definitions"] and remote_run.sh's arm_spec.
PROXY_ARMS = ("c", "d", "e", "f")

# Bootstrap resample count and seed. Fixed so a re-run of this script on the same
# run tree reproduces the published intervals byte for byte.
BOOTSTRAP_REPS = 5000
BOOTSTRAP_SEED = 42

# Which repeat's tree every reader below resolves against. A campaign runs several
# repeats side by side under out/repeat-N, and mixing two of them into one pooled
# statistic would silently average away the very drift the repeats measure. Set
# once from --repeat rather than threaded through every function, because a single
# reader left on the default would produce a plausible mixed-repeat number.
REPEAT = 1


def hosts(runs: str) -> dict[str, str]:
    """Map `hostN` to its on-disk directory name, which carries the IP."""
    out = {}
    for d in sorted(glob.glob(f"{runs}/host*")):
        out[os.path.basename(d).split("-", 1)[0]] = os.path.basename(d)
    if not out:
        raise SystemExit(f"no host* run trees under {runs}")
    return out


def arm_dir(runs: str, hosts_map: dict[str, str], host: str, arm: str) -> str:
    return f"{runs}/{hosts_map[host]}/out/repeat-{REPEAT}/arm-{arm}"


def discover_arms(runs: str, hosts_map: dict[str, str]) -> list[str]:
    """Arm letters that actually have a run tree, across all hosts.

    Deriving the set from disk rather than trusting the ``a,b,c,d`` default keeps
    the analysis from silently ignoring an arm the sweep was told to run: a
    ``L4_ARMS=a,b,c,d,e,f`` campaign would otherwise be read as four arms, with
    e/f dropped from every metric and no trace of the omission in report.json.
    """
    found = set()
    for hdir in hosts_map.values():
        for d in glob.glob(f"{runs}/{hdir}/out/repeat-{REPEAT}/arm-*"):
            letter = os.path.basename(d).split("-", 1)[1]
            if letter:
                found.add(letter)
    return sorted(found)


def load_arm(runs, hosts_map, host, arm):
    """Return the arm's token report and its per-instance result documents."""
    d = arm_dir(runs, hosts_map, host, arm)
    rep = json.load(open(f"{d}/report.json"))
    res = {}
    for f in glob.glob(f"{d}/run/results/*.json"):
        o = json.load(open(f))
        res[o["instance_id"]] = o
    return rep, res


# Markers that mean an arm's *activation* evidence failed, i.e. the arm may not have
# been running the component it claims. These void a host x arm cell outright.
# A `token-report refused` line is deliberately NOT here: it fires when one
# instance's per-turn token sum disagrees with the aggregate OpenClaw recorded for
# the same session, which invalidates that instance's numbers and nothing else.
# `paired()` drops those instances individually, so voiding the other eleven would
# discard valid measurements. Sweeps run before this distinction existed recorded
# token-only failures as arm failures, so the reason is re-derived here to keep
# every repeat on one rule.
ACTIVATION_FAILURE_MARKERS = (
    "GUARD FAILED",
    "OPENCLAW_HEADROOM_NO_PROXY_TRAFFIC",
    "plugin not found",
    "PER_CASE_PROMPT_SKIPPED",
    "per-case prompts served for",
    "activation evidence MISSING",
    "foreign flag leaked",
    "never became ready",
    "config does not reference port",
    "config missing",
)


def arm_activation_failed(text: str) -> dict[str, bool]:
    """Per arm, whether its last attempt showed an activation failure.

    The summary is an append log, so only the final block for each arm counts:
    an earlier aborted attempt must not condemn the run that replaced it.
    """
    out: dict[str, bool] = {}
    for block in re.split(r"^\[sweep\] --- arm ", text, flags=re.M)[1:]:
        arm = block.split()[0]
        out[arm] = any(mark in block for mark in ACTIVATION_FAILURE_MARKERS)
    return out


def guard_clean(runs, hosts_map, repeat):
    """Derive each host's per-arm evidence verdict from its sweep summary.

    ``repeat`` has no default on purpose: a reader left on repeat 1 while the rest
    of the analysis ran another repeat would silently mix trees (the reversed
    anti-pattern the module header warns about), so the caller must state it.

    The summary is an append log across whole and partial re-runs. Each verdict
    therefore updates only the arms named by its preceding ``sweep ... arms=``
    marker; the last verdict for each arm wins. This preserves an earlier A/B/D
    verdict when a later repair run executes C alone.

    A flagged arm is only voided when its final attempt actually failed an
    activation check (see :data:`ACTIVATION_FAILURE_MARKERS`).
    """
    start_re = re.compile(r"sweep repeat=\d+ arms=([^ ]+)")
    bad_re = re.compile(r"arms whose evidence did not hold:([^\n]*)")
    out = {}
    for host, hdir in hosts_map.items():
        path = f"{runs}/{hdir}/out/repeat-{repeat}/sweep-summary.txt"
        if not os.path.exists(path):
            raise SystemExit(f"{host}: sweep-summary.txt missing; cannot derive guards")
        text = open(path, errors="ignore").read()
        activation_failed = arm_activation_failed(text)
        verdict = {a: None for a in ARMS}
        current = set(ARMS)
        for line in text.splitlines():
            start = start_re.search(line)
            if start:
                current = set(start.group(1).split(","))
                continue
            bad = bad_re.search(line)
            if bad:
                flagged = set(bad.group(1).split())
                for arm in current:
                    voided = arm in flagged and activation_failed.get(arm, True)
                    verdict[arm] = 0 if voided else 1
            elif "all arms produced evidence-backed results" in line:
                for arm in current:
                    verdict[arm] = 1
        missing = [arm for arm, value in verdict.items() if value is None]
        if missing:
            raise SystemExit(f"{host}: sweep summary has no verdict for arms {missing}")
        out[host] = verdict
    return out


def ok_instance(o):
    """An instance is comparable only if it succeeded and actually emitted a patch."""
    rc = str(o.get("openclaw_returncode"))
    return bool(o.get("success")) and o.get("patch_produced") and rc in ("0", "None")


def boot_ci(vals, reps=BOOTSTRAP_REPS):
    """Percentile bootstrap CI of the median. Input order must already be stable."""
    if len(vals) < 2:
        return [None, None]
    random.seed(BOOTSTRAP_SEED)
    n = len(vals)
    meds = sorted(st.median([vals[random.randrange(n)] for _ in range(n)]) for _ in range(reps))
    return [round(meds[int(0.025 * reps)], 4), round(meds[int(0.975 * reps)], 4)]


def paired(runs, hosts_map, clean, target, clean_only):
    """Per-instance target/baseline ratios, pooled across hosts.

    Pairing is within an instance, so per-instance difficulty and the host both
    cancel. `clean_only` restricts to host x arm cells whose evidence held.
    """
    pt, rq = [], []
    for host in hosts_map:
        if clean_only and not (clean[host]["a"] and clean[host][target]):
            continue
        arep, ares = load_arm(runs, hosts_map, host, "a")
        brep, bres = load_arm(runs, hosts_map, host, target)
        ainst, binst = arep["instances"], brep["instances"]
        # Sorted: set iteration order varies with per-process string hash
        # randomization and the bootstrap draws indices positionally, so an
        # unsorted order makes the published CI bounds jitter between runs.
        for iid in sorted(set(ares) & set(bres)):
            if not (ok_instance(ares[iid]) and ok_instance(bres[iid])):
                continue
            a, b = ainst.get(iid), binst.get(iid)
            # A disagreeing aggregate proves OpenClaw dropped or duplicated turn
            # rows in that instance's store. Exclude only that pair: promoting the
            # suspect per-turn total is wrong, while rejecting the other eleven
            # instances in the host×arm cell throws away valid measurements.
            if not a or not b or a.get("aggregate_agrees") is False or b.get("aggregate_agrees") is False:
                continue
            if not a.get("prompt_tokens"):
                continue
            pt.append(b["prompt_tokens"] / a["prompt_tokens"])
            if a.get("requests"):
                rq.append(b["requests"] / a["requests"])
    return {
        "n": len(pt),
        "prompt_token_ratio_median": round(st.median(pt), 4) if pt else None,
        "prompt_token_ratio_ci95": boot_ci(pt),
        "requests_ratio_median": round(st.median(rq), 4) if rq else None,
        "requests_ratio_ci95": boot_ci(rq),
    }


def totals(runs, hosts_map):
    """Absolute per-arm aggregates. Bases differ per arm, so only the within-arm
    cache composition is cross-comparable."""
    out = {}
    for a in ARMS:
        g = {"instances": 0, "requests": 0, "prompt_tokens": 0, "input_tokens": 0,
             "cache_read_tokens": 0, "output_tokens": 0}
        durations = []
        for host in hosts_map:
            rep, res = load_arm(runs, hosts_map, host, a)
            t = rep.get("totals", {})
            for k in g:
                g[k] += t.get(k, 0)
            durations += [o["duration_seconds"] for o in res.values()
                          if o.get("duration_seconds") is not None]
        pt = g["prompt_tokens"]
        out[a] = dict(
            g,
            fresh_input_share=round(g["input_tokens"] / pt, 4) if pt else None,
            cache_read_share=round(g["cache_read_tokens"] / pt, 4) if pt else None,
            median_duration_seconds=round(st.median(durations), 1) if durations else None,
        )
    return out


def proxy(runs, hosts_map):
    """Wire-level savings from the comparison side's own per-request proxy log."""
    out = {}
    for arm in [a for a in ARMS if a in PROXY_ARMS]:
        orig = opt = n = active = 0
        per_host = {}
        for host, hdir in hosts_map.items():
            jf = f"{runs}/{hdir}/logs/proxy_{arm}.repeat{REPEAT}.requests.jsonl"
            if not os.path.exists(jf):
                continue
            ho = hp = 0
            for line in open(jf):
                line = line.strip()
                if not line:
                    continue
                try:
                    r = json.loads(line)
                except json.JSONDecodeError:
                    continue
                n += 1
                ho += r.get("input_tokens_original") or 0
                hp += r.get("input_tokens_optimized") or 0
                if (r.get("tokens_saved") or 0) > 0:
                    active += 1
            orig += ho
            opt += hp
            per_host[host] = round((ho - hp) / ho * 100, 2) if ho else None
        vals = sorted(v for v in per_host.values() if v is not None)
        # No requests and "ran but saved nothing" must not both read as 0.0: an
        # arm with no proxy log has no data (null), a different verdict from a
        # measured zero. Null tells the reader the wire metric is simply absent
        # for this arm rather than that compression achieved nothing.
        out[arm] = {
            "has_data": n > 0,
            "requests": n, "orig_input_tokens": orig, "optimized_input_tokens": opt,
            "tokens_saved": (orig - opt) if orig else None,
            "wire_savings_pct": round((orig - opt) / orig * 100, 2) if orig else None,
            "requests_active_pct": round(active / n * 100, 1) if n else None,
            "wire_savings_pct_by_host": per_host,
            "wire_savings_pct_range": [vals[0], vals[-1]] if vals else None,
        }
    return out


def tokenless_activation(runs, hosts_map):
    """Structural activation evidence for the component under test."""
    out = {}
    for host, hdir in hosts_map.items():
        fs = glob.glob(
            f"{runs}/{hdir}/out/repeat-{REPEAT}/arm-b/run/openclaw-tokenless-evidence/*.json"
        )
        out[host] = {
            "instances": len(fs),
            "strong": sum(bool(json.load(open(f)).get("strong")) for f in fs),
        }
    return out


def prompt_corpus(runs, hosts_map):
    """Prompt sizes and the positive per-case-prompt check.

    Counting the instances the loader actually served is stronger evidence than
    the absence of a PER_CASE_PROMPT_SKIPPED warning, so both are recorded.
    """
    sizes, loaded, prov = {}, {}, {}
    pat = re.compile(r"CUSTOM_PROMPT_LOADED instance=(\S+) file=(\S+) size=(\d+)")
    for host, hdir in hosts_map.items():
        for arm in ARMS:
            lf = f"{runs}/{hdir}/out/repeat-{REPEAT}/arm-{arm}/run/swe-runner.run.log"
            if not os.path.exists(lf):
                continue
            seen = set()
            for m in pat.finditer(open(lf, errors="ignore").read()):
                seen.add(m.group(1))
                sizes[m.group(1)] = int(m.group(3))
            loaded[f"{host}/arm-{arm}"] = len(seen)
        pf = f"{runs}/{hdir}/prompts.provenance"
        if os.path.exists(pf):
            prov[host] = dict(l.strip().split("=", 1) for l in open(pf) if "=" in l)
    v = sorted(sizes.values())
    return {
        "provenance_by_host": prov,
        "custom_prompt_loaded_per_host_arm": loaded,
        "instances_with_prompt": len(sizes),
        "prompt_bytes": {"min": v[0], "median": int(st.median(v)), "max": v[-1],
                         "total": sum(v)} if v else None,
    }


def prompt_guidance(runs, hosts_map):
    """How much the guided prompts give away.

    The named-source-file counts and the gold-patch disclosure both need inputs
    absent from this repository (the prompt corpus and the SWE-bench gold
    patches), so they are produced on a benchmark host by
    ``audit_prompt_disclosure.py`` and picked up here from the run tree if that
    artefact was synced back. When it is missing the disclosure block is marked
    unaudited rather than filled from the archive host's audit of a different,
    earlier corpus — that audit is reported only as an instance overlap so no
    rate from it can leak in.
    """
    disclosure = {
        "audited": False,
        "note": "run audit_prompt_disclosure.py on a benchmark host and sync "
                "prompt_disclosure_audit.json into the run tree to populate this",
    }
    # Derived from the same first-party audit as fix_disclosure, never a constant:
    # hardcoding these counts pinned the report to an earlier, different corpus
    # and could not be recomputed from what a campaign actually consumed.
    named = {
        "audited": False,
        "note": "populated from prompt_disclosure_audit.json when synced back",
    }
    for hdir in hosts_map.values():
        p = f"{runs}/{hdir}/prompt_disclosure_audit.json"
        if os.path.exists(p):
            doc = json.load(open(p))
            s = doc["summary"]
            disclosure = {
                "audited": True,
                "method": s["method"],
                "measured_shard": s["measured_shard"],
                "all_corpus": s["all_corpus"],
            }
            # How many gold source files each prompt names, bucketed over the
            # corpus this campaign actually audited (not a frozen distribution).
            hist = {"0": 0, "1": 0, "2": 0, "3+": 0}
            for row in doc.get("instances", []):
                c = row.get("gold_files_named") or 0
                hist["3+" if c >= 3 else str(c)] += 1
            named = {
                "audited": True,
                "source": "derived from prompt_disclosure_audit.json for the "
                          "corpus this campaign consumed",
                "corpus_prompts": len(doc.get("instances", [])),
                "prompts_by_files_named": hist,
            }
            break

    # Optional cross-check against an earlier, different prompt corpus audited on
    # an evaluation host. That archive is run-specific and is not committed to
    # this repo (see .gitignore: archive/), so the path is read from
    # L4_ARCHIVE_AUDIT when set and skipped otherwise.
    audit = os.environ.get("L4_ARCHIVE_AUDIT", "")
    unrelated = None
    if os.path.exists(audit):
        ids = {o["instance_id"] for o in json.load(open(audit))["instances"]}
        sel = {l.strip() for l in open(f"{L4}/assets/instances/n48-all.txt") if l.strip()}
        unrelated = {
            "corpus": "an earlier, different prompt corpus on the archive host",
            "audited_prompts": len(ids),
            "measured_instances": len(sel),
            "instances_in_both": len(ids & sel),
            "carried_into_report": "nothing — superseded by the first-party audit",
        }
    return {
        "named_source_files": named,
        "fix_disclosure": disclosure,
        "unrelated_archive_audit": unrelated,
    }


def run_integrity(runs, hosts_map):
    """Per-arm timing, because the four arms were not one contiguous sweep on
    every host: some arms survive from an earlier attempt that was re-run."""
    def ts(s):
        return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()

    out, durations = {}, []
    for host, hdir in hosts_map.items():
        rows = {}
        for arm in ARMS:
            mf = f"{runs}/{hdir}/out/repeat-{REPEAT}/arm-{arm}/run/run_metadata.json"
            if not os.path.exists(mf):
                continue
            d = json.load(open(mf))
            rows[arm] = {
                "started_at": d["started_at"], "ended_at": d["ended_at"],
                "instance_count": d.get("instance_count"),
                "succeeded": d.get("succeeded"), "failed": d.get("failed"),
                "workers": d.get("workers"),
                "duration_minutes": round((ts(d["ended_at"]) - ts(d["started_at"])) / 60, 1),
            }
            durations.append(rows[arm]["duration_minutes"])
        seq = sorted(rows.items(), key=lambda kv: ts(kv[1]["started_at"]))
        # Gap from one arm's end to the next arm's start. A large gap means the
        # arm came from an earlier sweep; an hour-boundary crossing does not.
        gaps = [round(ts(b["started_at"]) - ts(a["ended_at"]))
                for (_, a), (_, b) in zip(seq, seq[1:])]
        out[host] = {
            "arms": rows,
            "arm_order": [a for a, _ in seq],
            "inter_arm_gap_seconds": gaps,
            "max_gap_seconds": max(gaps) if gaps else None,
            # With fewer than two arms there is no gap to judge, so contiguity is
            # undefined (null), not vacuously True: all([]) would otherwise
            # certify a single-arm host as one contiguous sweep.
            "contiguous": all(g <= 1800 for g in gaps) if gaps else None,
        }
    return {
        "by_host": out,
        "arm_duration_minutes_range": [min(durations), max(durations)] if durations else None,
    }


def gates(runs, hosts_map):
    """Validity gates whose failure voids a run rather than degrading it."""
    out = {}
    for host, hdir in hosts_map.items():
        h = {}
        for key, rel in (("reference_toolchain", "logs/headroom_toolchain.txt"),
                         ("rtk_commit", "logs/rtk_commit.txt")):
            p = f"{runs}/{hdir}/{rel}"
            if os.path.exists(p):
                h[key] = open(p).read().strip()
        for when in ("before", "after"):
            mf = f"{runs}/{hdir}/out/repeat-{REPEAT}/model-created.{when}.json"
            if not os.path.exists(mf):
                continue
            e = [x for x in json.load(open(mf)).get("data", [])
                 if x.get("id") == "qwen3-coder-plus"]
            h[f"model_created_{when}"] = e[0]["created"] if e else None
        # The rolling alias can be repointed mid-run, which would make earlier
        # and later instances incomparable with no other visible symptom. When
        # either reading is missing the guard cannot speak: report null, not the
        # vacuous True that `None == None` yields -- a missing pair is the very
        # case remote_run.sh treats as fatal, so the analysis must not silently
        # certify it as stable.
        before, after = h.get("model_created_before"), h.get("model_created_after")
        h["model_created_stable"] = (
            (before == after) if before is not None and after is not None else None
        )
        out[host] = h
    return out


def self_report(runs, hosts_map):
    """Agent self-reported completion. NOT a solve rate: the dataset's own tests
    were never run, so this is harness liveness only."""
    out = {}
    for a in ARMS:
        n = s = p = 0
        for host in hosts_map:
            _, res = load_arm(runs, hosts_map, host, a)
            for o in res.values():
                n += 1
                s += bool(o.get("success"))
                p += bool(o.get("patch_produced"))
        out[a] = {"instances": n, "success_self_report": s, "patch_produced_self_report": p}
    return out


# Design facts transcribed from assets/configs, assets/scripts and the harness
# README, so the JSON carries the method next to the numbers.
DESIGN = {
    "arm_definitions": {
        "a": {"role": "baseline / reference point", "config": "arm-a-baseline.json",
              "runner_flag": None, "proxy_port": None, "proxy_flags": [], "executed": True},
        "b": {"role": "tokenless, component under test (hook plugin)",
              "config": "arm-b-tokenless.json", "runner_flag": "--tokenless",
              "proxy_port": None, "proxy_flags": [], "executed": True},
        "c": {"role": "the reference, defaults (context-engine)",
              "config": "arm-c-headroom-default.json", "runner_flag": "--headroom",
              "proxy_port": 8801, "proxy_flags": [], "executed": True},
        "d": {"role": "the reference + tool-result interception",
              "config": "arm-d-headroom-tool-results.json", "runner_flag": "--headroom",
              "proxy_port": 8802, "proxy_flags": ["--intercept-tool-results"],
              "executed": True},
        "e": {"role": "the reference + code-aware", "config": "arm-e-headroom-code-aware.json",
              "runner_flag": "--headroom", "proxy_port": 8803,
              "proxy_flags": ["--code-aware"], "executed": False},
        "f": {"role": "the reference + cache mode", "config": "arm-f-headroom-cache.json",
              "runner_flag": "--headroom", "proxy_port": 8804,
              "proxy_flags": ["--mode", "cache"], "executed": False},
    },
    "intervention_layers": {
        "tokenless": "hook plugin (activation.onCapabilities=[hook]); rewrites tool "
                     "results at the context tail, leaving the cached prefix intact",
        "reference": "kind=context-engine; takes over the whole context pipeline and "
                     "additionally exposes the headroom_retrieve tool",
    },
    "shared_config": {
        "provider": "bench-qwen", "api": "openai-completions",
        "model": "qwen3-coder-plus (rolling alias)",
        "context_window": 1000000, "max_tokens": 65536,
        "context_window_authored": True,
        "reasoning": False, "provider_timeout_seconds": 300,
        "cost": "all zeros; unused by every metric here",
        "plugins_slots_memory": "none",
        "api_key": "${DASHSCOPE_API_KEY}, resolved from the process environment only",
        "auto_start": False,
    },
    "instance_selection": {
        "list": "assets/instances/n48-all.txt",
        "method": "deterministic equal-interval traversal of the corpus sorted by "
                  "prompt byte size; no random seed",
        "cost_driver": "docker image cost scales with families (repo x version), not "
                       "with instance count",
        "options_considered": [
            {"instances": 227, "families": 58, "image_gb": 159, "agent_runs": 908,
             "slowest_host": "~15h for downloads alone", "chosen": False},
            {"instances": 48, "families": 23, "image_gb": 53, "agent_runs": 192,
             "slowest_host": "~8h", "chosen": True},
            {"instances": 64, "families": 30, "image_gb": 70, "agent_runs": 256,
             "slowest_host": "~10h", "chosen": False},
            {"instances": 96, "families": 39, "image_gb": 93, "agent_runs": 384,
             "slowest_host": "~15h", "chosen": False},
        ],
    },
    "sharding": {
        "shards": 4, "instances_per_shard": 12, "arms_per_host": ARMS,
        "rationale": "pairing is within an instance, so an instance must meet all of "
                     "its arms on one machine; the host then cancels in the difference",
        "partition_check": "remote_fleet.sh: no instance in two shards, union equals "
                           "n48-all.txt",
    },
    "invocation": {
        "command": "swe-runner run -a openclaw -i <12 ids> --per-case-prompt "
                   "--prompts-dir <prompts> --base-config <arm cfg> "
                   "--docker-pull-registry docker.m.daocloud.io [--tokenless|--headroom] "
                   "-o out/repeat-1/arm-<x> --workers 4 --timeout 1200 -v",
        "workers": 4, "timeout_seconds": 1200, "repeats": 1,
        "pull_registry": "docker.m.daocloud.io",
        "temperature": "unset (greedy); --seed is inert at temperature 0, so repeats "
                       "would only detect server-side non-determinism",
        "no_overlap_rule": "container label agent:<instance_id>:main is daemon-global, "
                           "so parallelism must come from --workers within one run",
    },
    "acceptance_checks": [
        "'plugin not found' absent from the run log (else the arm degraded to baseline)",
        "PER_CASE_PROMPT_SKIPPED absent",
        "CUSTOM_PROMPT_LOADED count equals the instance count (positive check)",
        "OPENCLAW_HEADROOM_NO_PROXY_TRAFFIC absent for reference arms",
        "arm D proven by a populated HEADROOM_BINARIES_CACHE, since the flag is "
        "invisible to the banner and to /stats",
    ],
}


def provenance(runs, hosts_map):
    """Provenance read from the run tree, never hardcoded.

    Any field that cannot be recovered from the synced artefacts is left null and
    ``incomplete`` is set, so a partial or empty tree yields an honestly
    incomplete record instead of stale constants from an earlier campaign.
    """
    # sweep-summary.txt carries `revision=<short-sha>` (remote_run.sh); take the
    # readable, non-"unknown" value and surface a cross-host disagreement rather
    # than silently picking one.
    revs = set()
    for hdir in hosts_map.values():
        path = f"{runs}/{hdir}/out/repeat-{REPEAT}/sweep-summary.txt"
        if not os.path.exists(path):
            continue
        for line in open(path, errors="ignore"):
            m = re.search(r"revision=(\S+)", line)
            if m and m.group(1) != "unknown":
                revs.add(m.group(1))
    anolisa_commit = next(iter(revs)) if len(revs) == 1 else (sorted(revs) or None)

    # Plugin versions from the `openclaw plugins list` capture taken at setup.
    tokenless_plugin = reference_plugin = None
    for hdir in hosts_map.values():
        p = f"{runs}/{hdir}/logs/plugins_list.log"
        if not os.path.exists(p):
            continue
        for line in open(p, errors="ignore"):
            low = line.lower()
            m = re.search(r"(\d+\.\d+\.\d+)", line)
            if not m:
                continue
            if "tokenless" in low and tokenless_plugin is None:
                tokenless_plugin = m.group(1)
            elif "headroom" in low and reference_plugin is None:
                reference_plugin = m.group(1)
        if tokenless_plugin and reference_plugin:
            break

    prov = {
        "anolisa_commit": anolisa_commit,
        "tokenless_plugin": tokenless_plugin,
        "reference_plugin": reference_plugin,
        "model_alias": "qwen3-coder-plus",
    }
    prov["incomplete"] = not (
        isinstance(anolisa_commit, str) and tokenless_plugin and reference_plugin
    )
    return prov


def report_date(runs, hosts_map):
    """The run's own date (UTC, YYYY-MM-DD) from the latest arm run_metadata.

    Falls back to the analysis time when no metadata is present, so the field is
    never a stale constant.
    """
    latest = None
    for hdir in hosts_map.values():
        for mf in glob.glob(f"{runs}/{hdir}/out/repeat-{REPEAT}/arm-*/run/run_metadata.json"):
            try:
                ended = json.load(open(mf)).get("ended_at")
            except Exception:  # noqa: BLE001
                continue
            if ended and (latest is None or ended > latest):
                latest = ended
    return latest[:10] if latest else datetime.now(timezone.utc).date().isoformat()


def instance_count(runs, hosts_map):
    """Distinct instance ids measured across all hosts, from the arm result docs."""
    ids = set()
    for host in hosts_map:
        for a in ARMS:
            for f in glob.glob(f"{arm_dir(runs, hosts_map, host, a)}/run/results/*.json"):
                try:
                    ids.add(json.load(open(f))["instance_id"])
                except Exception:  # noqa: BLE001
                    continue
    return len(ids)


def main():
    global REPEAT, ARMS
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--runs", default=f"{L4}/runs", help="run tree root (default: L4 runs/)")
    ap.add_argument("--repeat", type=int, default=1,
                    help="which out/repeat-N tree to analyse (default: 1)")
    ap.add_argument("-o", "--out", default=f"{L4}/reports/report.json")
    args = ap.parse_args()
    REPEAT = args.repeat

    hosts_map = hosts(args.runs)
    # Analyse whatever arms are on disk, not a hardcoded a,b,c,d, so a wider sweep
    # is never silently narrowed. Fall back to the default only when no arm tree
    # exists yet (an empty run tree still yields an honest, empty report).
    discovered = discover_arms(args.runs, hosts_map)
    if discovered:
        ARMS = discovered
    clean = guard_clean(args.runs, hosts_map, REPEAT)
    prov = provenance(args.runs, hosts_map)
    n_instances = instance_count(args.runs, hosts_map)
    doc = {
        "schema_version": 1,
        "layer": "L4-end-to-end",
        "report": "v1",
        # Recomputed from the run tree, never a constant: a stale date/platform/
        # provenance/headline is exactly the drift this file exists to prevent.
        "date": report_date(args.runs, hosts_map),
        "repeat": REPEAT,
        "arms_analysed": ARMS,
        "platform": f"linux/x86_64; {len(hosts_map)} host(s); "
                    f"N={n_instances} distinct instances; repeat {REPEAT}",
        "provenance": prov,
        # The prose headline lives in the markdown report; this file is data-only
        # and must not carry a cached conclusion that can outlive its numbers.
        "headline": None,
        "solve_rate": "NOT_EVALUATED: dataset tests not run; only agent self-report exists",
        "design": DESIGN,
        "guard_clean_map": clean,
        "prompt_corpus": prompt_corpus(args.runs, hosts_map),
        "prompt_guidance": prompt_guidance(args.runs, hosts_map),
        "validity_gates": gates(args.runs, hosts_map),
        "run_integrity": run_integrity(args.runs, hosts_map),
        "totals_by_arm": totals(args.runs, hosts_map),
        "reference_proxy_activation": proxy(args.runs, hosts_map),
        "tokenless_activation_by_host": tokenless_activation(args.runs, hosts_map),
        "agent_self_report_only_not_solve_rate": self_report(args.runs, hosts_map),
        "paired_prompt_tokens": {
            "method": "target/baseline per-instance ratio; "
                      f"{BOOTSTRAP_REPS}-sample bootstrap median CI (seed {BOOTSTRAP_SEED}); "
                      "instances kept only where both arms succeeded, produced a patch, "
                      "and returned rc in {0, None}",
            "guard_clean_pool": {t: paired(args.runs, hosts_map, clean, t, True)
                                 for t in ("b", "c", "d")},
            "salvage_all_hosts_pool": {t: paired(args.runs, hosts_map, clean, t, False)
                                       for t in ("b", "c", "d")},
        },
    }
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
