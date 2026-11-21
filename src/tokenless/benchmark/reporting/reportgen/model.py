# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""报告内部的测量行；未知值与零值分开，输入指纹参与配对。"""

import hashlib
import json
import math
from dataclasses import dataclass, field
from pathlib import Path


class ReportError(ValueError):
    """输入或输出不能满足报告可信度要求。"""


def read_json(path):
    try:
        with Path(path).open(encoding="utf-8") as stream:
            return json.load(stream)
    except (OSError, ValueError) as error:
        # 文件可能含身份信息，错误只报告类型，不展示内容或绝对路径。
        raise ReportError(f"输入文件不可读取：{type(error).__name__}") from error


def number(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return value if math.isfinite(value) else None


def count(value):
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None


def digest(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


@dataclass
class Row:
    """仅存报告需要的事实，不复制原文、主机环境或凭据。"""

    layer: str
    side: str
    mode: str
    sample: str
    category: str
    before: int | None
    after: int | None
    input_hash: str | None = None
    output_hash: str | None = None
    dataset: str = ""
    tokenizer: str = "unknown"
    disposition: str = "unknown"
    recovery: str = "unknown"
    mechanism: str = "unknown"
    latency_ms: list[float] = field(default_factory=list)
    facts: tuple[int, int] | None = None
    output_hashes: list[str] = field(default_factory=list)
    observations: int = 1
    semantic: dict = field(default_factory=dict)
    task: dict = field(default_factory=dict)
    notes: list[str] = field(default_factory=list)
    processing_condition: str = ""
    evidence_kind: str = "row"
    source_aggregate: dict = field(default_factory=dict)
    applicable: bool = True

    @property
    def rate(self):
        if (
            self.evidence_kind != "row"
            or not self.applicable
            or count(self.before) is None
            or count(self.after) is None
            or self.before == 0
        ):
            return None
        return 1 - self.after / self.before

    @property
    def key(self):
        return (
            self.layer,
            self.dataset,
            self.side,
            self.mode,
            self.sample,
            self.category,
            self.processing_condition,
            self.evidence_kind,
            self.applicable,
            self.input_hash,
            self.tokenizer,
        )

    @property
    def independent_key(self):
        return (
            self.layer,
            self.dataset,
            self.side,
            self.mode,
            self.category,
            self.processing_condition,
            self.applicable,
            self.input_hash if self.input_hash is not None else ("sample", self.sample),
            self.tokenizer,
        )

    @property
    def pair_key(self):
        return self.layer, self.dataset, self.category, self.input_hash, self.tokenizer


def combine(rows):
    """同输入重复计时不增加样本数；不同输出仍显示并记录不一致。"""
    grouped = {}
    for row in rows:
        key = (*row.key, row.output_hash, row.before, row.after)
        if key not in grouped:
            grouped[key] = row
        else:
            existing = grouped[key]
            existing.latency_ms.extend(row.latency_ms)
            existing.observations += row.observations
    outputs = {}
    for row in grouped.values():
        if row.input_hash is not None and row.output_hash is not None:
            outputs.setdefault(row.independent_key, set()).add(row.output_hash)
    for row in grouped.values():
        row.output_hashes = sorted(outputs.get(row.independent_key, []))
        if len(row.output_hashes) > 1:
            row.notes.append("同一输入观察到不同输出；不能声明确定性")
    return sorted(
        grouped.values(),
        key=lambda row: tuple(str(part) for part in (*row.key, row.output_hash)),
    )
