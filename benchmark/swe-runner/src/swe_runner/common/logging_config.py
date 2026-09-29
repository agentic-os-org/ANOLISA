# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Logging configuration for swe-runner."""

import logging
from pathlib import Path


def setup_logging(output_dir: Path, verbose: bool = False, suffix: str | None = None) -> None:
    """Configure file-only logging for the application.

    Args:
        output_dir: Directory where the log file will be written.
        verbose: If True, set log level to DEBUG; otherwise INFO.
        suffix: Optional command suffix for the log filename.
    """
    output_dir.mkdir(parents=True, exist_ok=True)

    root = logging.getLogger()
    root.handlers.clear()

    log_name = "swe-runner.log" if not suffix else f"swe-runner.{suffix}.log"
    log_file = output_dir / log_name
    file_handler = logging.FileHandler(str(log_file), mode="w", encoding="utf-8")

    formatter = logging.Formatter(
        "%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )
    file_handler.setFormatter(formatter)

    root.setLevel(logging.DEBUG if verbose else logging.INFO)
    root.addHandler(file_handler)
