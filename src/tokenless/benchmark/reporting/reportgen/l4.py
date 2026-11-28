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


def input_window_hash(arm):
    fingerprints = {}
    for relative in (
        "selection.json",
        "run/run_metadata.json",
        "run/preds.json",
        "report.json",
        "tool-results.json",
    ):
        path = arm / relative
        fingerprints[relative] = (
            hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None
        )
    if not any(fingerprints.values()):
        raise ReportError("L4 arm 目录没有已识别的采集证据")
    return digest(json.dumps(fingerprints, sort_keys=True))


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
                submitted = len(prediction_doc) if preds.is_file() or finished else None
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
                run_id = meta.get("run_id")
                if run_id is None:
                    run_id = meta.get("id")
                if run_id is not None and (not isinstance(run_id, str) or not run_id):
                    raise ReportError("L4 run_id/id 必须是非空字符串")
                identified = run_id is not None

                # 元数据是可变累计窗口；摘要只能定位输入，不能冒充生产端记录的运行身份。
                window_hash = input_window_hash(arm)
                identity = run_id if identified else "input-window:" + window_hash
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
                row.source_run = digest(run_id) if identified else window_hash
                row.input_hash = window_hash
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
                    row.notes.append("输入标识由采集证据原始字节摘要派生；不是生产端记录的运行标识")
                if count(meta.get("run_count")) is not None and meta["run_count"] > 1:
                    row.notes.append("该输入目录合并了多次运行；本行是累计窗口，不是单次运行")
                if row.task["status"] == "unscored":
                    row.notes.append("选定任务保留在分母清单中；未完成评分不发布解决率")
                rows.append(row)
    return rows
