# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""可选模型测题结果，严格按实际输入指纹匹配，不执行新的模型调用。"""

from .model import ReportError, count, number, read_json


def confidence_interval(value):
    if value is None:
        return None
    if not isinstance(value, list) or len(value) != 2:
        raise ReportError("模型保持率置信区间必须含两个端点")
    bounds = [number(bound) for bound in value]
    if any(bound is None for bound in bounds) or not 0 <= bounds[0] <= bounds[1] <= 1:
        raise ReportError("模型保持率置信区间超出有效范围")
    return bounds


def attach(rows, directories):
    """缺少本输入的测题结果时仍生成离线报告，但明确标注未测。"""
    matched = {}
    for layer, path in directories:
        plan = read_json(path / "plan.json")
        stats = read_json(path / "stats.json")
        if plan.get("layer") != layer or stats.get("layer") != layer:
            raise ReportError("模型测题计划与统计的层不一致")
        originals = {}
        for unit in plan.get("units", []):
            key = unit.get("row_id"), unit.get("side"), unit.get("variant")
            if key in originals:
                raise ReportError("模型测题计划的样本标识重复")
            originals[key] = unit.get("original"), unit.get("compressed")
        for row in stats.get("rows", []):
            key = row.get("row_id"), row.get("side"), row.get("variant")
            original, compressed = originals.get(key, (None, None))
            if not isinstance(original, str) or len(original) != 64:
                raise ReportError("模型测题统计不能绑定实际输入")
            lookup = layer, *key, original, compressed
            if lookup in matched:
                raise ReportError("同一个测题输入重复导入")
            matched[lookup] = row
    for row in rows:
        identifier = f"{row.dataset}/{row.sample}" if row.layer == "l1" else row.sample
        candidate = matched.get(
            (row.layer, identifier, row.side, row.mode, row.input_hash, row.output_hash)
        )
        if candidate is None or row.input_hash is None or row.output_hash is None:
            continue
        conditions = {}
        for name, measured in candidate.get("conditions", {}).items():
            missing = count(measured.get("missing_pairs"))
            if missing is None or missing > 0:
                note = "模型测题作答不完整，保持率不作为完整成绩"
                if note not in row.notes:
                    row.notes.append(note)
                continue
            semantic = measured.get("semantic_retention") or {}
            task = measured.get("task_retention") or {}
            conditions[name] = {
                "information_retained": number(semantic.get("pooled")),
                "task_retained": number(task.get("pooled")),
                "information_ci95": confidence_interval(semantic.get("ci95")),
                "task_original": number((measured.get("task_solve_original") or {}).get("mean")),
                "task_processed": number((measured.get("task_solve_compressed") or {}).get("mean")),
                "rounds": count((measured.get("accuracy_original") or {}).get("n")),
                "latency_change_s": number(
                    (measured.get("latency_paired_diff_s") or {}).get("median")
                ),
                "lost": count((measured.get("mcnemar") or {}).get("lost")),
                "gained": count((measured.get("mcnemar") or {}).get("gained")),
            }
        for condition in conditions.values():
            for field in (
                "information_retained",
                "task_retained",
                "task_original",
                "task_processed",
            ):
                value = condition.get(field)
                if value is not None and not 0 <= value <= 1:
                    raise ReportError("模型保持率或答对率超出有效范围")
        row.semantic = conditions
    return rows
