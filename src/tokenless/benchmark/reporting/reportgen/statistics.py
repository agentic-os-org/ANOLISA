# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""对已有样本计算描述统计，不把重复测量当作独立样本。"""

import random
import statistics
from copy import deepcopy


def percentile(values, share):
    if not values:
        return None
    values = sorted(values)
    position = (len(values) - 1) * share
    left = int(position)
    right = min(left + 1, len(values) - 1)
    return values[left] + (values[right] - values[left]) * (position - left)


def describe(values):
    if not values:
        return None
    rng = random.Random(42)
    samples = sorted(statistics.fmean(rng.choices(values, k=len(values))) for _ in range(1000))
    return {
        "n": len(values),
        "mean": statistics.fmean(values),
        "median": statistics.median(values),
        "std": statistics.stdev(values) if len(values) > 1 else None,
        "iqr": [percentile(values, 0.25), percentile(values, 0.75)],
        "ci95": [percentile(samples, 0.025), percentile(samples, 0.975)]
        if len(values) > 1
        else None,
    }


def independent(rows):
    grouped = {}
    for row in rows:
        grouped.setdefault(row.independent_key, []).append(row)
    measured = []
    for aliases in grouped.values():
        outputs = {(row.output_hash, row.before, row.after) for row in aliases}
        if len(outputs) != 1 or any(
            row.rate is None or len(row.output_hashes) > 1 for row in aliases
        ):
            continue
        observation = deepcopy(aliases[0])
        observation.latency_ms = [value for row in aliases for value in row.latency_ms]
        measured.append(observation)
    return measured


def summary(rows):
    # 相同输入出现不同输出时，不选其中一个假装稳定，也不把两者都加入收益平均。
    measured = independent(rows)
    before = sum(row.before for row in measured)
    after = sum(row.after for row in measured)
    return {
        "all_rows": len(rows),
        "measured": len(measured),
        "saving_rows": sum(row.after < row.before for row in measured),
        "weighted": 1 - after / before if before else None,
        "rates": describe([row.rate for row in measured]),
        "latency_ms": describe(
            [statistics.median(row.latency_ms) for row in measured if row.latency_ms]
        ),
    }
