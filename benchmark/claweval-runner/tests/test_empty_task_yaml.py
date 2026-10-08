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

"""Empty or comment-only task.yaml files must behave like ``{}``.

``yaml.safe_load`` returns None for a document with no nodes; every
consumer then called ``.get()`` on it and crashed with an
AttributeError. load_task_yaml() now normalises None to {}.
"""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from ce_runner._common import is_sandbox_task, load_task_yaml  # noqa: E402
from ce_runner.preflight import find_missing_fixtures  # noqa: E402


class TestEmptyTaskYaml(unittest.TestCase):
    def test_load_empty_yaml_returns_empty_dict(self):
        with tempfile.TemporaryDirectory() as tmp:
            task_yaml = str(Path(tmp) / "task.yaml")
            Path(task_yaml).write_text("")
            self.assertEqual(load_task_yaml(task_yaml), {})

    def test_load_comment_only_yaml_returns_empty_dict(self):
        with tempfile.TemporaryDirectory() as tmp:
            task_yaml = str(Path(tmp) / "task.yaml")
            Path(task_yaml).write_text("# nothing here\n")
            self.assertEqual(load_task_yaml(task_yaml), {})

    def test_is_sandbox_task_on_empty_yaml(self):
        with tempfile.TemporaryDirectory() as tmp:
            task_yaml = str(Path(tmp) / "task.yaml")
            Path(task_yaml).write_text("")
            self.assertFalse(is_sandbox_task(task_yaml))

    def test_find_missing_fixtures_on_empty_yaml(self):
        with tempfile.TemporaryDirectory() as tmp:
            task_yaml = Path(tmp) / "task.yaml"
            task_yaml.write_text("")
            self.assertEqual(find_missing_fixtures([str(Path(tmp))]), {})


if __name__ == "__main__":
    unittest.main()
