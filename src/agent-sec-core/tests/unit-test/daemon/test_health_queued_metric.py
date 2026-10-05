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

"""daemon.health queues.queued must reflect the real skill-ledger backlog.

QueueState.queued had no writer anywhere in the codebase: a real
skill-ledger activation backlog was invisible to monitors and the metric
read as a constant 0.
"""

from __future__ import annotations

from pathlib import Path

from agent_sec_cli.daemon.health import build_health_snapshot
from agent_sec_cli.daemon.jobs.skill_ledger.activation import (
    SKILL_LEDGER_ACTIVATION_JOB,
    SkillLedgerActivationJob,
)
from agent_sec_cli.daemon.runtime import DaemonRuntime


def test_queued_reports_skill_ledger_backlog(tmp_path):
    runtime = DaemonRuntime(socket_path=str(tmp_path / "daemon.sock"))
    snapshot = build_health_snapshot(runtime)
    assert snapshot["queues"]["queued"] == 0
    assert snapshot["queues"]["inflight"] == 0

    job = SkillLedgerActivationJob()
    # Simulate a coalesced backlog without starting the loop.
    job._pending[Path("/skills/weather")] = None
    runtime.jobs.register(job)
    assert job.name == SKILL_LEDGER_ACTIVATION_JOB

    snapshot = build_health_snapshot(runtime)
    assert snapshot["queues"]["queued"] == 1


def test_queued_zero_without_registered_job(tmp_path):
    runtime = DaemonRuntime(socket_path=str(tmp_path / "daemon.sock"))
    snapshot = build_health_snapshot(runtime)
    assert snapshot["queues"]["queued"] == 0
