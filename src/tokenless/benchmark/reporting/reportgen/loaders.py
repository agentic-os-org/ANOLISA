# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""读取各层已有测量文件；输入路径由调用方提供，不依赖活动批次。"""

import json
from pathlib import Path

from .model import ReportError, Row, count, digest, number, read_json


def latency(value, scale=1):
    value = number(value)
    return [value * scale] if value is not None and value >= 0 else []


def facts(value):
    if isinstance(value, dict):
        passed, total = count(value.get("passed")), count(value.get("total"))
    elif isinstance(value, list) and len(value) == 2:
        passed, total = map(count, value)
    else:
        return None
    if passed is None or total is None or total == 0:
        return None
    if passed > total:
        raise ReportError("重要信息通过数超过检查项总数")
    return passed, total


def fingerprint(value, text=None):
    valid = (
        isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value)
    )
    actual = digest(text) if isinstance(text, str) else None
    if value is not None and (not valid or actual is not None and value != actual):
        raise ReportError("声明指纹与实际正文不一致或指纹格式无效")
    return actual if actual is not None else value if valid else None


def text_field(value, layer, index, field, default=None):
    if value is None and default is not None:
        return default
    if not isinstance(value, str) or not value:
        raise ReportError(f"{layer.upper()} 行 {index} 的 {field} 必须是非空字符串")
    return value


def aggregate_row(layer, side, category, values, mode="unknown", tokenizer="unknown"):
    row = Row(
        layer,
        side,
        mode,
        category,
        category,
        None,
        None,
        tokenizer=tokenizer,
        evidence_kind="aggregate",
    )
    row.source_aggregate = {
        "rate": number(values.get("compression_mean")),
        "n": count(values.get("n")),
        "facts": facts([values.get("retention_passed"), values.get("retention_total")]),
    }
    rate = row.source_aggregate["rate"]
    if rate is not None and rate > 1:
        raise ReportError(f"{layer.upper()} 源汇总节省率不能大于 100%")
    row.notes.append("源汇总结果，不反推逐行样本、Token实测或独立N")
    return row


def legacy_l1(doc):
    rows = []
    source = doc.get("legacy_estimates") or doc
    for category, values in (source.get("canonical") or {}).items():
        if not isinstance(values, dict):
            raise ReportError("L1 legacy canonical 必须是对象")
        row = aggregate_row("l1", "tokenless", category, {}, "legacy-estimate", "bytes/4")
        value = number(values.get("savings_pct"))
        if value is not None and value > 100:
            raise ReportError("L1 源汇总节省率不能大于 100%")
        row.source_aggregate["rate"] = value / 100 if value is not None else None
        row.dataset = "source-document-" + digest(json.dumps(doc, sort_keys=True))
        row.notes.append("历史 bytes/4 辅助估算，不作为主计数或账单")
        rows.append(row)
    return rows


def legacy_l2(doc):
    rows = []
    if not isinstance(doc.get("categories"), list):
        raise ReportError("L2 输入缺少 rows 或 categories")
    mode = (doc.get("summary") or {}).get("mode") or "unknown"
    for index, category in enumerate(doc["categories"]):
        label = text_field(category.get("category"), "l2", index, "category")
        for side in ("tokenless", "headroom"):
            values = category.get(side)
            if values is not None:
                if not isinstance(values, dict):
                    raise ReportError(f"L2 行 {index} 的 {side} 汇总必须是对象")
                row = aggregate_row(
                    "l2",
                    side,
                    label,
                    values,
                    mode if side == "tokenless" else "router",
                    "o200k_base",
                )
                row.dataset = "source-document-" + digest(json.dumps(doc, sort_keys=True))
                rows.append(row)
    return rows


def resolve_text(doc, raw, field):
    reference = raw.get(f"{field}_ref")
    if reference is None:
        return raw.get(field)
    if not isinstance(reference, str) or not isinstance(doc.get("text_store"), dict):
        raise ReportError(f"L2 text_store 的 {field} 引用无效")
    text = doc["text_store"].get(reference)
    if not isinstance(text, str) or digest(text) != reference:
        raise ReportError(f"L2 text_store 的 {field} 引用或指纹无效")
    return text


def joined_flags(value, layer, index, field):
    if value is None:
        return "unknown"
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ReportError(f"{layer.upper()} 行 {index} 的 {field} 必须是字符串数组")
    return " + ".join(value) or "unknown"


def l1(path):
    doc = read_json(path)
    domain_report = "component_domains" in doc or "headline_tokenizer" in doc
    source = doc.get("component_domains", doc)
    if not isinstance(source.get("rows"), list):
        if isinstance(doc.get("canonical"), dict) or isinstance(doc.get("legacy_estimates"), dict):
            return legacy_l1(doc)
        raise ReportError("L1 输入缺少 rows；请使用组件测量 JSON 或 legacy canonical")
    result = []
    for index, raw in enumerate(source["rows"]):
        if not isinstance(raw, dict):
            raise ReportError(f"L1 行 {index} 必须是对象")
        case = raw.get("case") or {}
        if not isinstance(case, dict):
            raise ReportError(f"L1 行 {index} 的 case 必须是对象")
        reference = "sample_id" in raw and "case" not in raw and not domain_report
        text, output = case.get("input", raw.get("input")), raw.get("output")
        before, after = count(raw.get("tokens_before")), count(raw.get("tokens_after"))
        mode = text_field(case.get("mode", raw.get("mode")), "l1", index, "mode", "unknown")
        sample = text_field(case.get("id", raw.get("sample_id")), "l1", index, "sample")
        dataset = text_field(doc.get("set", source.get("set")), "l1", index, "set", "component")
        milliseconds = raw.get("latency_ms")
        if isinstance(milliseconds, dict):
            milliseconds = milliseconds.get("p50")
        measured_latency = latency(raw.get("latency_s"), 1000) or latency(milliseconds)
        row = Row(
            "l1",
            "headroom" if reference else "tokenless",
            mode,
            sample,
            text_field(case.get("domain", raw.get("domain")), "l1", index, "domain", "unknown"),
            before,
            after,
            input_hash=fingerprint(raw.get("input_sha256", raw.get("payload_hash")), text),
            output_hash=fingerprint(raw.get("output_sha256", raw.get("output_hash")), output),
            dataset=dataset,
            tokenizer=text_field(
                raw.get("tokenizer")
                or source.get("headline_tokenizer")
                or doc.get("headline_tokenizer"),
                "l1",
                index,
                "tokenizer",
                "o200k_base",
            ),
            disposition=raw.get("disposition", "unknown"),
            recovery=raw.get("retention_form", "unknown"),
            mechanism=raw.get("mechanism", raw.get("entry", "unknown")),
            latency_ms=measured_latency,
            facts=facts(raw.get("facts")),
        )
        row.notes.append("单组件尝试结果，不是生产入口最终结果")
        result.append(row)
    return result


def l2(path):
    doc = read_json(path)
    if not isinstance(doc.get("rows"), list):
        return legacy_l2(doc)
    result = []
    for index, entry in enumerate(doc["rows"]):
        if not isinstance(entry, dict) or not isinstance(entry.get("row"), dict):
            raise ReportError(f"L2 行 {index} 的 row 必须是对象")
        raw = entry["row"]
        post = (raw.get("entry") or {}).get("post_tool") or {}
        side = entry.get("side")
        if side not in ("tokenless", "headroom"):
            raise ReportError("L2 side 必须明确为 tokenless/headroom")
        mode = text_field(entry.get("mode"), "l2", index, "mode", "unknown")
        if side == "headroom":
            mode = "router"
        row = Row(
            "l2",
            side,
            mode,
            text_field(raw.get("sample_id"), "l2", index, "sample_id"),
            text_field(entry.get("category"), "l2", index, "category", "unknown"),
            count(raw.get("tokens_before")),
            count(raw.get("tokens_after")),
            input_hash=fingerprint(raw.get("payload_hash"), resolve_text(doc, raw, "input")),
            output_hash=fingerprint(raw.get("output_hash"), resolve_text(doc, raw, "output")),
            tokenizer="o200k_base",
            disposition=post.get("disposition", "unknown"),
            recovery=post.get("recoverability", "unknown"),
            mechanism=" + ".join(post.get("applied_operations") or [])
            or entry.get("entry_kind", "ContentRouter"),
            latency_ms=latency(raw.get("latency_s"), 1000),
            facts=facts([raw.get("retention_passed"), raw.get("retention_total")]),
        )
        if side == "headroom":
            row.disposition = text_field(
                raw.get("disposition"), "l2", index, "disposition", "unknown"
            )
            row.recovery = text_field(
                raw.get("recoverability"), "l2", index, "recoverability", "unknown"
            )
            row.notes.append("参考侧未记录处理结果或恢复状态时保持未知，不从节省率推断")
        if (entry.get("entry_kind") or "").startswith("explicit RTK"):
            row.notes.append("显式手动 RTK 路径，不代表默认执行前处理")
            command = (raw.get("entry") or {}).get("command")
            row.processing_condition = digest(command) if isinstance(command, str) else row.sample
        basis = {
            "production entry: runtime in-process + rtk wrapped-minus-raw": "生产入口计时：含运行时仲裁；命令改写另计包装执行减原执行耗时，含进程启动",
            "in-process": "进程内调用计时，不含进程启动、模型或网络",
            "worker-internal": "参考进程内内容分派计时，不含进程启动、管道传输或JSON封装",
            "wrapped-minus-raw wall clock": "包装执行减原执行耗时，包含包装器进程启动",
        }.get(raw.get("latency_basis"), "具体计时范围未记录或无法识别；不同路径时延不可直接排名")
        row.notes.append(basis)
        result.append(row)
    return result


def conversation_hash(document):
    """与外部测题计划使用同一会话展示规则；不把 JSON 转义文本重复计入。"""
    parts = []
    for index, message in enumerate(document.get("messages", []), 1):
        header = f"[message {index}] role={message.get('role')}"
        for key in ("name", "tool_call_id"):
            if message.get(key):
                header += f" {key}={message[key]}"
        parts.append(header)
        content = message.get("content")
        if isinstance(content, str):
            parts.append(content)
        elif content is not None:
            parts.append(json.dumps(content, ensure_ascii=False))
        if message.get("tool_calls"):
            parts.append(
                "tool_calls: "
                + json.dumps(message["tool_calls"], ensure_ascii=False, separators=(",", ":"))
            )
    if document.get("tools"):
        parts += [
            "[tools]",
            json.dumps(document["tools"], ensure_ascii=False, separators=(",", ":")),
        ]
    return digest("\n".join(parts))


def l3(path):
    path = Path(path)
    doc = read_json(path)
    if not isinstance(doc.get("scenarios"), list):
        raise ReportError("L3 输入缺少 scenarios；请使用 report.json")
    lifecycle_path = path.with_name("lifecycle.json")
    lifecycle = read_json(lifecycle_path) if lifecycle_path.is_file() else {}
    # 不存在详细过程文件时保留测量结果，但不套用无法核实的模型测题分数。
    entries = lifecycle.get("rows", []) if isinstance(lifecycle, dict) else lifecycle
    hashes, output_hashes = {}, {}
    for item in entries:
        if isinstance(item, dict) and isinstance(item.get("input"), dict):
            key = (
                item.get("suite"),
                item.get("scenario"),
                item.get("side"),
                item.get("mode_id"),
            )
            if key in hashes:
                raise ReportError("L3 生命周期样本标识重复，不能覆盖前一条输入指纹")
            hashes[key] = conversation_hash(item["input"])
            if isinstance(item.get("output"), dict):
                output_hashes[key] = conversation_hash(item["output"])
    result = []
    primary_mode = (doc.get("summary") or {}).get("tokenless_mode")
    companion_mode = lifecycle.get("tokenless_mode") if isinstance(lifecycle, dict) else None
    if primary_mode and companion_mode and primary_mode != companion_mode:
        raise ReportError("L3 主报告与生命周期恢复模式不一致")
    mode = text_field(primary_mode or companion_mode, "l3", 0, "tokenless_mode", "unknown")
    source_run = digest(json.dumps(doc, sort_keys=True, ensure_ascii=False))
    for index, raw in enumerate(doc["scenarios"]):
        if not isinstance(raw, dict):
            raise ReportError(f"L3 行 {index} 必须是对象")
        identity = (
            text_field(raw.get("suite"), "l3", index, "suite", "scenario"),
            text_field(raw.get("scenario"), "l3", index, "scenario"),
        )
        sample = "/".join(identity)
        before = count(raw.get("tokens_before"))
        tl_mode = mode
        details = raw.get("tokenless_lifecycle") or {}
        candidates = [
            (
                "tokenless",
                tl_mode,
                "tokenless_rate",
                "tokenless_ms",
                "tokenless_retention",
            ),
            (
                "headroom",
                "router",
                "headroom_router_rate",
                "headroom_ms",
                "headroom_retention",
            ),
            (
                "headroom",
                "pure_stage",
                "headroom_pure_rate",
                None,
                None,
            ),
        ]
        for side, variant, rate_key, time_key, fact_key in candidates:
            rate = number(raw.get(rate_key))
            if rate is not None and rate > 1:
                raise ReportError(f"L3 行 {index} 的 {rate_key} 大于 100%，会生成负 Token 数")
            after = round(before * (1 - rate)) if before is not None and rate is not None else None
            row = Row(
                "l3",
                side,
                variant,
                sample,
                text_field(raw.get("content_type"), "l3", index, "content_type", "unknown"),
                before,
                after,
                input_hash=hashes.get((*identity, side, variant)),
                output_hash=output_hashes.get((*identity, side, variant)),
                tokenizer="o200k_base",
                disposition=joined_flags(details.get("dispositions"), "l3", index, "dispositions")
                if side == "tokenless"
                else "unknown",
                recovery=joined_flags(details.get("recoverability"), "l3", index, "recoverability")
                if side == "tokenless"
                else "unknown",
                mechanism=" + ".join(details.get("operations") or [])
                if side == "tokenless"
                else " + ".join(raw.get("headroom_strategies") or []),
                latency_ms=latency(raw.get(time_key)),
                facts=facts(raw.get(fact_key)),
            )
            row.source_run = source_run
            row.notes.append("处理后 Token 由原报告比例还原；不是新增实测")
            if side == "tokenless":
                gap = raw.get("tokenless_gap_reason")
                points = raw.get("tokenless_entry_points")
                row.applicable = (
                    not gap
                    and isinstance(points, list)
                    and bool(points)
                    and all(isinstance(point, str) and bool(point) for point in points)
                )
                if not row.applicable:
                    row.facts = None
                if not row.applicable:
                    row.notes.append(
                        "生产入口不适用或恢复能力不可用；保留原计量，不参与节省率或配对统计"
                    )
            if side == "headroom":
                if variant == "pure_stage":
                    row.notes.append("基础会话流程未独立测量时延和重要信息；不借用内容分派流程成绩")
                else:
                    row.notes.append("原报告共享参考流程时延；不是本分支独立计时")
            result.append(row)
    return result


LOADERS = {"l1": l1, "l2": l2, "l3": l3}
