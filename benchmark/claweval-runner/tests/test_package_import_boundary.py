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

"""Probe the real package entry point in dependency-isolated subprocesses."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest


def execute(code):
    source = Path(__file__).resolve().parents[1] / "src"
    environment = dict(os.environ, PYTHONPATH=str(source), PYTHONIOENCODING="utf-8")
    return subprocess.run(
        [sys.executable, "-c", code],
        capture_output=True,
        text=True,
        env=environment,
        timeout=15,
    )


def test_version_import_does_not_load_runtime_dependencies():
    result = execute("""
import importlib.abc
import sys
class BlockRuntime(importlib.abc.MetaPathFinder):
    def find_spec(self, fullname, path=None, target=None):
        if fullname == 'ce_runner.run_task' or fullname.split('.')[0] in {'resource','docker','openai','mcp'}:
            raise AssertionError('Unexpected runtime import: ' + fullname)
sys.meta_path.insert(0, BlockRuntime())
import ce_runner
assert ce_runner.__version__ == '1.0.0'
assert callable(ce_runner.main)
assert 'ce_runner.run_task' not in sys.modules
""")
    assert result.returncode == 0, result.stderr


@pytest.mark.parametrize("exit_code", [None, 7])
def test_main_invocation_keeps_delegate_and_exit_semantics(exit_code):
    result = execute(f"""
import sys
from types import ModuleType
runtime = ModuleType('ce_runner.run_task')
calls = []
def main():
    calls.append('called')
    if {exit_code!r} is not None:
        raise SystemExit({exit_code!r})
runtime.main = main
sys.modules['ce_runner.run_task'] = runtime
import ce_runner
try:
    ce_runner.main()
except SystemExit as exc:
    assert exc.code == {exit_code!r}
else:
    assert {exit_code!r} is None
assert calls == ['called']
""")
    assert result.returncode == 0, result.stderr
