# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""可选模型测题结果，严格按实际输入指纹匹配，不执行新的模型调用。"""

import sys

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
    matched, seen, used = {}, {}, set()
    for directory_index, (layer, path) in enumerate(directories, 1):
        plan = read_json(path / "plan.json")
        stats = read_json(path / "stats.json")
        if plan.get("layer") != layer or stats.get("layer") != layer:
            raise ReportError("模型测题计划与统计的层不一致")
        provenance = None
        if layer == "l3":
            provenance = plan.get("source_run")
            for name, document in (("plan", plan), ("stats", stats)):
                source = document.get("source_run")
                if source is not None and (
                    not isinstance(source, str)
                    or len(source) != 64
                    or any(char not in "0123456789abcdef" for char in source)
                ):
                    raise ReportError(
                        f"L3 模型目录 {directory_index} 的 {name}.source_run 格式无效"
                    )
            if provenance != stats.get("source_run"):
                raise ReportError(f"L3 模型目录 {directory_index} 的 plan/stats.source_run 不一致")
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
            base_key = layer, *key, original, compressed
            seen.setdefault(base_key, set()).add(provenance)
            lookup = *base_key, provenance
            if lookup in matched:
                raise ReportError("同一个测题输入重复导入")
            matched[lookup] = row, directory_index
    for row in rows:
        row.semantic = {}
        if not row.applicable:
            continue
        identifier = f"{row.dataset}/{row.sample}" if row.layer == "l1" else row.sample
        key = row.layer, identifier, row.side, row.mode, row.input_hash, row.output_hash
        provenance = row.source_run or None if row.layer == "l3" else None
        evidence = matched.get((*key, provenance))
        if evidence is None or row.input_hash is None or row.output_hash is None:
            if row.layer == "l3" and key in seen:
                note = (
                    "模型测题缺来源运行标识；不绑定或发布成绩"
                    if None in seen[key]
                    else "模型测题来源运行与本报告不一致；不绑定或发布成绩"
                )
                if note not in row.notes:
                    row.notes.append(note)
            continue
        candidate, directory_index = evidence
        if row.layer == "l3" and provenance is None:
            row.notes.append("模型测题缺来源运行标识；不绑定或发布成绩")
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
        used.add(directory_index)
    for directory_index, (layer, _) in enumerate(directories, 1):
        if directory_index not in used:
            print(
                f"{layer.upper()} 模型目录 {directory_index} 的显式证据未绑定；未发布成绩",
                file=sys.stderr,
            )
            for row in rows:
                if row.layer == layer:
                    note = "显式提供的模型测题证据未绑定；测量保留但成绩未发布"
                    if note not in row.notes:
                        row.notes.append(note)
    return rows
