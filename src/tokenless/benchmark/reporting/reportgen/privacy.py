# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""输出采用字段白名单与匿名标识；不自动公开输入标签或环境信息。"""

import hashlib
import hmac
import os
import re

from .model import ReportError

_PSEUDONYM_KEY = os.environ.get("REPORTGEN_SALT", "").encode() or os.urandom(32)

PATTERNS = {
    "network_address": re.compile(r"(?<![\w.])(?:\d{1,3}\.){3}\d{1,3}(?![\w.])"),
    "personal_directory": re.compile(r"/(?:Users|home)/[^/\s]+"),
    "email": re.compile(r"\b[\w.+-]+@[\w.-]+\.[A-Za-z]{2,}\b"),
    "credential": re.compile(
        r"\b(?:sk-[A-Za-z0-9]{16,}|LTAI[A-Za-z0-9]{12,}|AKIA[A-Z0-9]{16}|gh[pousr]_[A-Za-z0-9]{20,})\b|-----BEGIN [A-Z ]*PRIVATE KEY-----"
    ),
    "authorization": re.compile(r"(?i)authorization\s*[:=]\s*(?:bearer|basic)\s+\S+"),
}


def anonymous(value):
    """同次生成使用稳定假名；跨次关联仅在调用方提供私有 salt 时开启。"""
    return "样本-" + hmac.new(_PSEUDONYM_KEY, str(value).encode(), hashlib.sha256).hexdigest()[:12]


def check_output(text, forbidden=()):
    for category, pattern in PATTERNS.items():
        if pattern.search(text):
            raise ReportError(f"输出敏感信息检查未通过：{category}")
    if any(value and value in text for value in forbidden):
        raise ReportError("输出命中调用方禁止公开的标识")


def safe_choice(value, known, fallback="未记录/不适用"):
    """只展示受控类型名；未经审核的自由文本不透传。"""
    return known.get(value, fallback) if isinstance(value, str) else fallback
