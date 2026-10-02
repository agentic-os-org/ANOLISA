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

"""Test CLI argument parsing for ce-runner.

Covers:
- run subcommand defaults and options
- batch subcommand parameters
- Task filtering (--tasks-file, --prefix, --filter, --tag, --range)
- Backward compatibility (no subcommand)

Task type coverage:
- T tasks: always-sandbox mode
- M tasks: always-sandbox mode (multimodal)
- C tasks: user_agent enabled via config.yaml
"""

import sys
from pathlib import Path
from unittest.mock import patch

import pytest

# Add src to path
REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


class TestRunSubcommand:
    """Test 'ce-runner run' subcommand argument parsing."""

    def test_run_defaults(self):
        """Test default values for run subcommand."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'run', 'T001zh_email_triage']):
            with patch('ce_runner.run_task.run_single') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.task == 'T001zh_email_triage'
                assert args.timeout == 600
                assert args.config is None
                assert args.trace_prefix == 'openclaw'

    def test_run_with_timeout(self):
        """Test run with custom timeout."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'run', 'T001', '--timeout', '300']):
            with patch('ce_runner.run_task.run_single') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.timeout == 300

    def test_run_with_config(self):
        """Test run with config file."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'run', 'T001', '--config', 'config.yaml']):
            with patch('ce_runner.run_task.run_single') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.config == 'config.yaml'

    def test_run_trace_prefix(self):
        """Test run with custom trace prefix."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'run', 'T001', '--trace-prefix', 'custom']):
            with patch('ce_runner.run_task.run_single') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.trace_prefix == 'custom'


class TestBatchSubcommand:
    """Test 'ce-runner batch' subcommand argument parsing."""

    def test_batch_defaults(self):
        """Test default values for batch subcommand."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'batch']):
            with patch('ce_runner.run_task.batch_runner.run_batch') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.parallel == 4
                assert args.timeout == 600
                assert args.trials == 1
                assert args.grade_parallel == 0
                assert args.trace_prefix == 'openclaw'
                assert args.skip_preflight is False

    def test_batch_skip_preflight(self):
        """Test batch with --skip-preflight flag."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'batch', '--skip-preflight']):
            with patch('ce_runner.run_task.batch_runner.run_batch') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.skip_preflight is True

    def test_batch_with_tasks_file(self):
        """Test batch with --tasks-file."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'batch', '--tasks-file', 'tasks.txt']):
            with patch('ce_runner.run_task.batch_runner.run_batch') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.tasks_file == 'tasks.txt'

    def test_batch_with_tasks_string(self):
        """Test batch with --tasks-string comma-separated names."""
        from ce_runner.run_task import main

        argv = ['ce-runner', 'batch', '--tasks-string',
                'T091_pinbench_humanize_blog,T096_pinbench_business_metrics_summary']
        with patch.object(sys, 'argv', argv):
            with patch('ce_runner.run_task.batch_runner.run_batch') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.tasks_string == (
                    'T091_pinbench_humanize_blog,T096_pinbench_business_metrics_summary'
                )

    def test_batch_with_prefix(self):
        """Test batch with --prefix filter."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'batch', '--prefix', 'C']):
            with patch('ce_runner.run_task.batch_runner.run_batch') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.prefix == 'C'

    def test_batch_with_filter(self):
        """Test batch with --filter substring match."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'batch', '--filter', 'email']):
            with patch('ce_runner.run_task.batch_runner.run_batch') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.filter == 'email'

    def test_batch_with_tag(self):
        """Test batch with --tag filter."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'batch', '--tag', 'multimodal']):
            with patch('ce_runner.run_task.batch_runner.run_batch') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.tag == 'multimodal'

    def test_batch_with_range(self):
        """Test batch with --range filter."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'batch', '--range', '1-10']):
            with patch('ce_runner.run_task.batch_runner.run_batch') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.range == '1-10'

    def test_batch_combined_filters(self):
        """Test batch with combined --prefix and --range."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'batch', '--prefix', 'M', '--range', '1-5']):
            with patch('ce_runner.run_task.batch_runner.run_batch') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.prefix == 'M'
                assert args.range == '1-5'

    def test_batch_parallel_workers(self):
        """Test batch with custom parallel workers."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'batch', '--parallel', '8']):
            with patch('ce_runner.run_task.batch_runner.run_batch') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.parallel == 8

    def test_batch_grade_parallel(self):
        """Test batch with custom grade-parallel."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'batch', '--grade-parallel', '4']):
            with patch('ce_runner.run_task.batch_runner.run_batch') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.grade_parallel == 4

    def test_batch_trials(self):
        """Test batch with multiple trials."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'batch', '--trials', '3']):
            with patch('ce_runner.run_task.batch_runner.run_batch') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.trials == 3


class TestTaskTypeFlags:
    """Test argument parsing for different task types."""

    def test_t_task_run(self):
        """Test T task run with defaults (always sandbox)."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'run', 'T001zh_email_triage']):
            with patch('ce_runner.run_task.run_single') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.task == 'T001zh_email_triage'

    def test_m_task_run(self):
        """Test M task run with defaults (always sandbox)."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'run', 'M001_clock']):
            with patch('ce_runner.run_task.run_single') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.task == 'M001_clock'

    def test_c_task_user_agent_via_config(self):
        """Test C task relies on config.yaml for user_agent (no CLI flag)."""
        from ce_runner.run_task import main

        with patch.object(sys, 'argv', ['ce-runner', 'run', 'C01zh_mortgage_prepay']):
            with patch('ce_runner.run_task.run_single') as mock_run:
                main()
                args = mock_run.call_args[0][0]
                assert args.task == 'C01zh_mortgage_prepay'
