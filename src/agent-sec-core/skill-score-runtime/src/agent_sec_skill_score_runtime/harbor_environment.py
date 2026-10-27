"""Keep Harbor's host-side finalizers outside the subject's writable Docker mounts."""

import shutil
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_skill_score_runtime.files import snapshot_tree
from harbor.environments.capabilities import EnvironmentCapabilities
from harbor.environments.docker.docker import DockerEnvironment
from harbor.models.trial.config import ServiceVolumeConfig


class ScoreDockerEnvironment(DockerEnvironment):
    def __init__(
        self, *, log_staging: str, mounts: list[ServiceVolumeConfig], **kwargs: Any
    ) -> None:
        expected = ("/logs/agent", "/logs/verifier", "/logs/artifacts")
        if sorted(m["target"] for m in mounts) != sorted(expected):
            raise ContractError("unexpected Harbor trial mounts")

        self.log_sources = {
            target: Path(log_staging) / Path(target).name for target in expected
        }
        for path in self.log_sources.values():
            path.mkdir(mode=0o700)
        self.snapshot_failed = False
        super().__init__(
            mounts=[
                ServiceVolumeConfig(type="bind", source=str(path), target=target)
                for target, path in self.log_sources.items()
            ],
            **kwargs,
        )

    @property
    def capabilities(self) -> EnvironmentCapabilities:
        # Trial must explicitly download logs before conversion, reward reads and step archiving.
        return super().capabilities.model_copy(update={"mounted": False})

    async def download_dir(self, source_dir: str, target_dir: Path | str) -> None:
        target = Path(target_dir)
        # Harbor reuses its host agent directory across resumed steps. A failed
        # snapshot must leave neither a previous trajectory nor partially copied input.
        try:
            source = self.log_sources[source_dir]
            await self.prepare_logs_for_host()
            if target.exists():
                shutil.rmtree(target)
            snapshot_tree(
                source,
                target,
                exclude_root=(
                    ("trajectory.json",) if source_dir == "/logs/agent" else ()
                ),
            )
        except Exception:
            self.snapshot_failed = True
            if target.exists():
                shutil.rmtree(target)
            target.mkdir(mode=0o700)
            raise

    async def download_file(self, source_path: str, target_path: Path | str) -> None:
        self.snapshot_failed = True
        raise ContractError(
            "the score trial only collects its configured log directories"
        )

    async def upload_dir(self, source_dir: Path | str, target_dir: str) -> None:
        # Resumed Claude sessions already live in the container. The ASC verifier
        # consumes observer logs and never needs the host-generated trajectory.
        if target_dir != "/logs/agent":
            await super().upload_dir(source_dir, target_dir)
