# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""L4 实例账本与官方评分；缺失任务、未知计量和未认证结果分别处理。"""

import hashlib
import json
import re
from datetime import datetime
from pathlib import Path

from .model import ReportError, Row, count, digest, number, read_json


def selected_ids(arm):
    for path in (arm / "selection.json", arm / "run" / "run_metadata.json"):
        if path.is_file():
            doc = read_json(path)
            ids = doc.get("instance_ids")
            if (
                not isinstance(ids, list)
                or not ids
                or not all(isinstance(i, str) and i for i in ids)
                or len(set(ids)) != len(ids)
            ):
                raise ReportError("L4 选定任务集缺失、重复或格式错误")
            return set(ids), (
                doc.get("attempt_complete") is True
                or (path.name == "run_metadata.json" and bool(doc.get("ended_at")))
            )
    return None, False


def read_predictions(path):
    if not path.is_file():
        return {}, None
    try:
        raw = path.read_bytes()
        doc = json.loads(raw)
    except (OSError, ValueError) as error:
        raise ReportError("L4 predictions 无法读取或解析") from error
    if not isinstance(doc, (dict, list)):
        raise ReportError("L4 predictions 必须是对象或数组")
    return doc, hashlib.sha256(raw).hexdigest()


def verified_score(root, repeat, arm, selected, predictions_hash=None):
    evaluation = root / "eval" / f"repeat-{repeat}-arm-{arm.name.split('-', 1)[1]}"
    marker = evaluation / ".eval-complete"
    predictions = arm / "run" / "preds.json"
    if not marker.is_file() or not predictions.is_file():
        return None
    completed = marker.read_text(encoding="utf-8").split()
    if (
        len(completed) != 2
        or not re.fullmatch(r"[A-Za-z0-9_-]+", completed[1])
        or completed[0] != (predictions_hash or read_predictions(predictions)[1])
    ):
        return None
    matches = list((evaluation / "evaluate").glob(f"*.{completed[1]}.json"))
    if len(matches) != 1:
        raise ReportError("L4 完成标记没有唯一对应的评分报告")
    report = read_json(matches[0])
    resolved = report.get("resolved_ids")
    if (
        not isinstance(resolved, list)
        or not all(isinstance(iid, str) and iid for iid in resolved)
        or len(set(resolved)) != len(resolved)
        or selected is None
        or not set(resolved) <= selected
    ):
        raise ReportError("L4 评分任务与选定任务集不一致")
    return len(resolved)


def usage(arm, selected=None):
    path = arm / "report.json"
    if not path.is_file():
        return None, None, "未记录模型计量"
    report = read_json(path)
    instances = report.get("instances")
    if (
        not isinstance(instances, dict)
        or not instances
        or selected is None
        or not selected <= set(instances)
    ):
        return None, None, "选定任务的实例计量缺失"
    totals = {
        "input_tokens": 0,
        "cache_read_tokens": 0,
        "output_tokens": 0,
        "requests": 0,
    }
    for iid in selected:
        instance = instances[iid]
        if not isinstance(instance, dict):
            return None, None, "计量字段缺失或非法，不作为零费用"
        if instance.get("aggregate_agrees") is False:
            return None, None, "逐回合计量与总量不一致"
        for field in totals:
            value = count(instance.get(field))
            if value is None:
                return None, None, "计量字段缺失或非法，不作为零费用"
            totals[field] += value
    return (
        totals["input_tokens"] + totals["cache_read_tokens"],
        totals["requests"],
        "实例计量可读；不认证完整费用"
        + (
            f"；未选中任务计量已排除：{len(set(instances) - selected)}"
            if set(instances) - selected
            else ""
        ),
    )


def load(path):
    path = Path(path)
    roots = (
        [path]
        if (path / "out").is_dir()
        else sorted(
            child for child in path.iterdir() if child.is_dir() and (child / "out").is_dir()
        )
    )
    if not roots:
        raise ReportError("L4 输入没有 out/repeat-N/arm-X 运行目录")
    rows = []
    for root in roots:
        for repeat_path in sorted((root / "out").glob("repeat-*")):
            if not repeat_path.is_dir():
                continue
            repeat = repeat_path.name.split("-", 1)[1]
            for arm in sorted(repeat_path.glob("arm-*")):
                if not arm.is_dir():
                    continue
                letter = arm.name.split("-", 1)[1]
                if letter not in tuple("abcdef"):
                    raise ReportError("L4 arm 标识不在已知配置集合中")
                side = "baseline" if letter == "a" else "tokenless" if letter == "b" else "headroom"
                selected, finished = selected_ids(arm)
                preds = arm / "run" / "preds.json"
                prediction_doc, predictions_hash = read_predictions(preds)
                resolved = verified_score(root, repeat, arm, selected, predictions_hash)
                submitted = len(prediction_doc)
                no_predictions = finished and submitted == 0
                if resolved is None and no_predictions and selected is not None:
                    resolved = 0
                prompt, requests, meter_note = usage(arm, selected)
                metadata_path = arm / "run" / "run_metadata.json"
                meta = read_json(metadata_path) if metadata_path.is_file() else {}
                duration = None
                if meta.get("started_at") and meta.get("ended_at"):
                    try:
                        duration = (
                            datetime.fromisoformat(meta["ended_at"])
                            - datetime.fromisoformat(meta["started_at"])
                        ).total_seconds()
                    except (ValueError, TypeError) as error:
                        raise ReportError("L4 时间记录不可解析") from error
                capture_path = arm / "tool-results.json"
                capture = read_json(capture_path) if capture_path.is_file() else {}
                run_id = meta.get("run_id") or meta.get("id")
                identified = isinstance(run_id, str) and bool(run_id)
                if (
                    not identified
                    and isinstance(meta.get("started_at"), str)
                    and meta["started_at"]
                ):
                    # 使用生产端实际记录的运行内容，移动采集目录不改变身份。
                    run_id = "metadata:" + digest(json.dumps(meta, sort_keys=True))
                    identified = True
                identity = run_id if identified else "unidentified:" + digest(str(root))
                row = Row(
                    "l4",
                    side,
                    f"arm-{letter}",
                    digest(identity)[:12] + f"/repeat-{repeat}",
                    "agent-task",
                    None,
                    None,
                    dataset="end-to-end",
                    disposition="not_certified",
                    recovery="unobserved",
                    mechanism="hook"
                    if side == "tokenless"
                    else "context-engine"
                    if side == "headroom"
                    else "baseline",
                )
                row.task = {
                    "selected": len(selected) if selected is not None else None,
                    "resolved": resolved,
                    "submitted": submitted,
                    "status": "completed_no_predictions"
                    if no_predictions
                    else "scored"
                    if resolved is not None
                    else "unscored",
                    "prompt_tokens": prompt,
                    "requests": requests,
                    "duration_s": number(duration),
                    "changed_results": count(capture.get("changed_results")),
                }
                row.notes = [
                    meter_note,
                    "官方评分只证明补丁任务结果；不证明压缩已生效或语义保持率",
                ]
                if not identified:
                    row.notes.append("运行标识未记录；仅作本次输入定位，不能跨采集关联")
                if row.task["status"] == "unscored":
                    row.notes.append("选定任务保留在分母清单中；未完成评分不发布解决率")
                rows.append(row)
    return rows
