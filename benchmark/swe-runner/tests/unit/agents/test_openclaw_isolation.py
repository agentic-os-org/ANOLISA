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

"""OpenClaw attempts own their host paths and Docker cleanup identities."""

from __future__ import annotations

import json
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

import pytest

from swe_runner.agents.openclaw import adapter, sandbox
from swe_runner.common.commands import CommandResult
from swe_runner.common.models import AgentConfig, Settings, SWEInstance
from swe_runner.run.workspace import docker
from swe_runner.trace_extraction.openclaw_jsonl import iter_openclaw_jsonl_traces


class OfflineCommands:
    """Respond at CLI boundaries while retaining real preparation and cleanup code."""

    def __init__(self, home: Path) -> None:
        self.home = home
        self.containers: dict[str, str] = {}
        self.prep_names: list[str] = []
        self.commands: list[list[str]] = []
        self.lock = threading.Lock()
        self.copy_barrier: threading.Barrier | None = None

    def run(self, cmd: list[str], **kwargs: object) -> CommandResult:
        stdout = ""
        with self.lock:
            self.commands.append(cmd)
            if cmd[:2] == ["docker", "create"]:
                name = cmd[cmd.index("--name") + 1]
                assert name not in self.containers, "Preparation container name already owned"
                self.prep_names.append(name)
                self.containers[name] = "prep"
            elif cmd[:3] == ["docker", "rm", "-f"]:
                for name in cmd[3:]:
                    self.containers.pop(name, None)
            elif cmd[:3] == ["docker", "ps", "-aq"]:
                label = cmd[-1].removeprefix("label=")
                stdout = "\n".join(name for name, owner in self.containers.items() if owner == label)
            elif cmd[:2] == ["docker", "cp"]:
                assert cmd[2].split(":", 1)[0] in self.containers
                repo = Path(cmd[3])
                (repo / ".git" / "info").mkdir(parents=True)
                (repo / "source.py").write_text("base source\n", encoding="utf-8")
            elif cmd[0] == "openclaw" and "explain" in cmd:
                profile = cmd[cmd.index("--profile") + 1]
                agent_id = cmd[cmd.index("--agent") + 1]
                config = json.loads((self.home / f".openclaw-{profile}" / "openclaw.json").read_text())
                entry = next(item for item in config["agents"]["list"] if item["id"] == agent_id)
                stdout = json.dumps({"sandbox": {"workspaceRoot": entry["sandbox"]["workspaceRoot"]}})
            else:
                assert cmd[:2] == ["docker", "pull"] or (cmd[0] == "openclaw" and "recreate" in cmd)
        if cmd[:2] == ["docker", "cp"] and self.copy_barrier is not None:
            self.copy_barrier.wait(timeout=5)
        return CommandResult(args=tuple(cmd), returncode=0, stdout=stdout, stderr="")

    def add_sandbox(self, name: str, agent_id: str) -> None:
        self.containers[name] = f"openclaw.sessionKey=agent:{agent_id}:main"


def make_instance(instance_id: str = "django__django-13448") -> SWEInstance:
    return SWEInstance(
        instance_id=instance_id,
        repo="django/django",
        version="1",
        base_commit="base",
        problem_statement="Fix it",
        patch="",
        test_patch="",
    )


@pytest.mark.parametrize("same_output", [False, True])
@pytest.mark.parametrize("cleanup_first", [0, 1])
def test_overlapping_attempts_preserve_other_repository_profile_and_sandbox(
    tmp_path: Path, same_output: bool, cleanup_first: int
) -> None:
    home = tmp_path / "home"
    home.mkdir()
    host = tmp_path / "host"
    host.mkdir()
    base = tmp_path / "base.json"
    base.write_text("{}", encoding="utf-8")
    commands = OfflineCommands(home)
    instance = make_instance()
    settings_a = Settings(agent=AgentConfig(name="openclaw"), output={"output_dir": tmp_path / "out-A"})
    settings_b = Settings(
        agent=AgentConfig(name="openclaw"), output={"output_dir": tmp_path / ("out-A" if same_output else "out-B")}
    )
    agent = adapter.OpenClawAdapter(base_config_path=base, profile_link_root=home)
    prepared = []
    with (
        patch.object(adapter, "default_workspace_root", return_value=host / "swebench_work_case"),
        patch("tempfile.tempdir", str(host)),
        patch.object(docker, "run_command", side_effect=commands.run),
        patch.object(sandbox, "run_command", side_effect=commands.run),
        patch.object(adapter, "get_git_revision", return_value="base"),
    ):
        try:
            first = agent.prepare(instance, settings_a)
            prepared.append(first)
            source_a = first.work_dir / "source.py"
            source_a.write_text("first live edit\n", encoding="utf-8")
            profile_a = Path(first.metadata["openclaw_profile_dir"])
            session_dir = profile_a / "agents" / first.metadata["agent_id"] / "sessions"
            session_dir.mkdir(parents=True)
            session_a = session_dir / f"{first.metadata['session_id']}.jsonl"
            session_entries = [
                {"type": "session", "id": first.metadata["session_id"]},
                {
                    "type": "message",
                    "message": {
                        "role": "user",
                        "content": [{"type": "text", "text": f"Issue ID: {instance.instance_id}"}],
                    },
                },
                {
                    "type": "message",
                    "message": {
                        "role": "assistant",
                        "content": [{"type": "text", "text": "First session"}],
                        "usage": {"input": 10, "output": 5},
                    },
                },
            ]
            session_text = "\n".join(json.dumps(entry) for entry in session_entries) + "\n"
            session_a.write_text(session_text, encoding="utf-8")
            commands.add_sandbox("live-A", first.metadata["agent_id"])
            second = agent.prepare(instance, settings_b)
            prepared.append(second)
            commands.add_sandbox("live-B", second.metadata["agent_id"])

            assert source_a.read_text() == "first live edit\n"
            assert session_a.read_text() == session_text
            # Both metadata-directed collection and root discovery retain the original case identity.
            for source in ({"profile_dirs": [profile_a]}, {"profiles_root": profile_a.parent}):
                traces = iter_openclaw_jsonl_traces(
                    **source,
                    start_ns=0,
                    end_ns=2**63 - 1,
                    instance_ids={instance.instance_id},
                    session_ids={first.metadata["session_id"]},
                )
                assert len(traces) == 1
                assert traces[0]["issue_id"] == instance.instance_id
                assert traces[0]["session_id"] == first.metadata["session_id"]
            assert "live-A" in commands.containers
            assert first.work_dir != second.work_dir
            for key in ("agent_id", "session_id", "openclaw_profile", "openclaw_profile_dir"):
                assert first.metadata[key] != second.metadata[key]
            assert len(set(commands.prep_names)) == 2
            assert len(first.metadata["agent_id"]) <= 63
            assert first.metadata["agent_id"].startswith("django__django-13448-")

            survivor = prepared[1 - cleanup_first]
            owner = prepared[cleanup_first]
            owner.cleanup()
            assert not owner.work_dir.exists()
            assert survivor.work_dir.is_dir()
            assert Path(survivor.metadata["openclaw_config_path"]).is_file()
            assert (home / f".openclaw-{survivor.metadata['openclaw_profile']}").is_symlink()
            assert ("live-B" if cleanup_first == 0 else "live-A") in commands.containers
            assert ("live-A" if cleanup_first == 0 else "live-B") not in commands.containers
        finally:
            for item in prepared:
                item.cleanup()
    assert not commands.containers
    assert not list(host.iterdir())


def test_concurrent_image_preparation_owns_distinct_containers(tmp_path: Path) -> None:
    commands = OfflineCommands(tmp_path)
    commands.copy_barrier = threading.Barrier(2)
    with patch.object(docker, "run_command", side_effect=commands.run), ThreadPoolExecutor(max_workers=2) as executor:
        futures = [
            executor.submit(
                docker.prepare_workspace_from_image, "image:latest", instance_id="same-case", work_dir=tmp_path / str(i)
            )
            for i in range(2)
        ]
        repositories = [future.result(timeout=10) for future in futures]
    assert len(set(commands.prep_names)) == 2
    assert not commands.containers
    assert all((repo / "source.py").read_text() == "base source\n" for repo in repositories)


@pytest.mark.parametrize("phase", ["image", "configure"])
def test_failed_preparation_releases_owned_workspace_and_link(tmp_path: Path, phase: str) -> None:
    home = tmp_path / "home"
    home.mkdir()
    host = tmp_path / "host"
    host.mkdir()
    base = tmp_path / "base.json"
    base.write_text("{}", encoding="utf-8")
    commands = OfflineCommands(home)
    original = commands.run

    def failing_command(cmd: list[str], **kwargs: object) -> CommandResult:
        if (phase == "image" and cmd[:2] == ["docker", "cp"]) or (phase == "configure" and "explain" in cmd):
            raise RuntimeError("fixture preparation failed")
        return original(cmd, **kwargs)

    with (
        patch.object(adapter, "default_workspace_root", return_value=host / "swebench_work_case"),
        patch("tempfile.tempdir", str(host)),
        patch.object(docker, "run_command", side_effect=failing_command),
        patch.object(sandbox, "run_command", side_effect=failing_command),
        patch.object(adapter, "get_git_revision", return_value="base"),
        pytest.raises(RuntimeError, match="fixture preparation failed"),
    ):
        adapter.OpenClawAdapter(base_config_path=base, profile_link_root=home).prepare(
            make_instance(), Settings(agent=AgentConfig(name="openclaw"), output={"output_dir": tmp_path / "out"})
        )
    assert not list(host.iterdir())
    assert not list(home.iterdir())
    assert not commands.containers
