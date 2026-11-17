# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""任意层子集的离线结果报告，不依赖运行机、模型服务或固定批次。"""

from .model import ReportError, Row

__all__ = ["ReportError", "Row"]
