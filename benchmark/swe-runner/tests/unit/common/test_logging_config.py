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

import logging

from swe_runner.common.logging_config import setup_logging


def test_setup_logging_uses_command_suffix(tmp_path):
    setup_logging(tmp_path, suffix="evaluate")

    logging.getLogger("test").info("hello")

    log_file = tmp_path / "swe-runner.evaluate.log"
    assert log_file.exists()
    assert "hello" in log_file.read_text()


def test_setup_logging_keeps_default_name_without_suffix(tmp_path):
    setup_logging(tmp_path)

    logging.getLogger("test").info("hello")

    assert (tmp_path / "swe-runner.log").exists()
