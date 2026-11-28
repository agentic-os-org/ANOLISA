# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""直接从源码运行测试，不安装报告生成器或修改全局环境。"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
