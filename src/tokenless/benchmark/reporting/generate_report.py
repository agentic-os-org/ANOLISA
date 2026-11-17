#!/usr/bin/env python3
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""从任意工作目录调用报告生成器；只读取显式提供的测试结果。"""

from reportgen.__main__ import main

if __name__ == "__main__":
    raise SystemExit(main())
