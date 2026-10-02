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

"""OpenClaw sandbox management for per-instance local profiles."""

from __future__ import annotations

import json
import logging
import shutil
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path

from swe_runner.agents.openclaw.config import load_openclaw_config
from swe_runner.common.commands import run_command

logger = logging.getLogger(__name__)

_TESTBED_ENV_BIN = "/opt/miniconda3/envs/testbed/bin"
_WORKSPACE_PATH = "/workspace"
_TESTBED_MOUNT_PATH = "/testbed"
_RUNNER_SUPPORT_DIR = "/workspace/.runner"
_HOST_TOKENLESS_BIN_DIR = Path("/usr/share/tokenless/bin")
_HOST_TOKENLESS_EXTRA_BIN_DIRS = (
    Path.home() / ".local" / "bin",
    Path.home() / ".openclaw" / "extensions" / "tokenless" / "bin",
    Path.home() / ".openclaw" / "extensions" / "tokenless",
)
_HOST_OPENCLAW_EXTENSIONS_DIR = Path.home() / ".openclaw" / "extensions"
_TOKENLESS_RUNTIME_BIN_DIR = f"{_RUNNER_SUPPORT_DIR}/tokenless/bin"
_TOKENLESS_BINARY_NAMES = ("rtk", "tokenless")
_TOKENLESS_PLUGIN_ID = "tokenless"
_DEFAULT_PATH_SUFFIX = f"{_TESTBED_ENV_BIN}:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
_PYTEST_CACHE_DIR = "/tmp/swe-runner-pytest-cache"
_HYPOTHESIS_CACHE_DIR = "/tmp/swe-runner-hypothesis"
_AGENTS_FILE = "AGENTS.md"
_OPENCLAW_SANDBOX_SESSION_LABEL = "openclaw.sessionKey"
_SETUP_COMMAND = (
    f"mkdir -p {_PYTEST_CACHE_DIR} {_HYPOTHESIS_CACHE_DIR}/examples "
    "&& git config --global --add safe.directory /testbed || true"
)


def build_openclaw_agent_scope_key(agent_id: str) -> str:
    """Return the OpenClaw sandbox scope key used for agent-scoped local runs."""
    return f"agent:{agent_id}:main"


def _openclaw_stability_params() -> dict[str, object]:
    return {
        "temperature": 0,
        "top_p": 1,
        "seed": 42,
    }


def _apply_openclaw_stability_defaults(config: dict[str, object]) -> None:
    agents = config.setdefault("agents", {})
    if not isinstance(agents, dict):
        raise RuntimeError("Invalid OpenClaw config: agents must be an object")

    defaults = agents.setdefault("defaults", {})
    if not isinstance(defaults, dict):
        raise RuntimeError("Invalid OpenClaw config: agents.defaults must be an object")

    params = defaults.setdefault("params", {})
    if not isinstance(params, dict):
        raise RuntimeError("Invalid OpenClaw config: agents.defaults.params must be an object")

    extra_body = params.get("extra_body")
    if extra_body is not None and not isinstance(extra_body, dict):
        raise RuntimeError("Invalid OpenClaw config: agents.defaults.params.extra_body must be an object")

    if isinstance(extra_body, dict):
        extra_body.pop("enable_thinking", None)

    defaults.pop("thinkingDefault", None)
    defaults["skipBootstrap"] = True

    stability_params = _openclaw_stability_params()
    for key, value in stability_params.items():
        params[key] = value


@dataclass(frozen=True)
class OpenClawSandboxSpec:
    """Static OpenClaw agent configuration for one SWE-bench instance."""

    agent_id: str
    image_name: str
    workspace_root: Path
    testbed_dir: Path
    agents_text: str | None = None


def _write_agents(workspace: Path, agents_text: str | None) -> Path | None:
    if agents_text is None:
        return None
    agents_path = workspace / _AGENTS_FILE
    agents_path.write_text(f"{agents_text.rstrip()}\n", encoding="utf-8")
    return agents_path


def _iter_host_tokenless_binary_candidates(binary_name: str) -> Iterator[Path]:
    seen: set[Path] = set()

    path_match = shutil.which(binary_name)
    if path_match:
        candidate = Path(path_match)
        seen.add(candidate)
        yield candidate

    for candidate_dir in (_HOST_TOKENLESS_BIN_DIR, *_HOST_TOKENLESS_EXTRA_BIN_DIRS):
        candidate = candidate_dir / binary_name
        if candidate not in seen:
            seen.add(candidate)
            yield candidate

    if not _HOST_OPENCLAW_EXTENSIONS_DIR.is_dir():
        return

    for plugin_dir in sorted(_HOST_OPENCLAW_EXTENSIONS_DIR.iterdir()):
        if not plugin_dir.is_dir():
            continue
        for candidate in (plugin_dir / "bin" / binary_name, plugin_dir / binary_name):
            if candidate not in seen:
                seen.add(candidate)
                yield candidate


def _resolve_host_tokenless_binary(binary_name: str) -> Path:
    candidates = list(_iter_host_tokenless_binary_candidates(binary_name))
    for candidate in candidates:
        if candidate.is_file():
            return candidate

    searched = ", ".join(str(candidate) for candidate in candidates) or "<none>"
    raise RuntimeError(
        f"Tokenless binary {binary_name!r} not found on host. "
        f"Ensure it is on PATH or installed in a known tokenless/OpenClaw plugin directory. Searched: {searched}"
    )


def _resolve_host_tokenless_extension() -> Path:
    extension_dir = _HOST_OPENCLAW_EXTENSIONS_DIR / _TOKENLESS_PLUGIN_ID
    if (extension_dir / "openclaw.plugin.json").is_file() or (extension_dir / "package.json").is_file():
        return extension_dir

    raise RuntimeError(
        f"Tokenless OpenClaw plugin extension not found on host: {extension_dir}. "
        "Install the tokenless OpenClaw plugin before running with --tokenless."
    )


def _expose_tokenless_plugin_extension(profile_dir: Path) -> None:
    source = _resolve_host_tokenless_extension().resolve(strict=False)
    target = profile_dir / "extensions" / _TOKENLESS_PLUGIN_ID
    target.parent.mkdir(parents=True, exist_ok=True)

    if target.is_symlink():
        if target.resolve(strict=False) == source:
            return
        raise RuntimeError(f"OpenClaw tokenless extension link already points elsewhere: {target}")
    if target.exists():
        raise RuntimeError(f"OpenClaw tokenless extension path already exists and is not a symlink: {target}")

    target.symlink_to(source, target_is_directory=True)
    logger.info("OPENCLAW_TOKENLESS_EXTENSION_LINKED source=%s target=%s", source, target)


def _tokenless_binary_record(binary_name: str, source: Path, target: Path) -> dict[str, object]:
    return {
        "name": binary_name,
        "source": str(source),
        "source_exists": source.is_file(),
        "source_is_symlink": source.is_symlink(),
        "source_realpath": str(source.resolve(strict=False)),
        "copied": str(target),
        "copied_exists": target.is_file(),
        "copied_is_symlink": target.is_symlink(),
        "copied_size": target.stat().st_size if target.is_file() else None,
    }


def _inject_tokenless_binaries(workspace: Path) -> None:
    tokenless_root = workspace / ".runner" / "tokenless"
    tokenless_target = tokenless_root / "bin"
    tokenless_target.mkdir(parents=True, exist_ok=True)
    records: list[dict[str, object]] = []

    for binary_name in _TOKENLESS_BINARY_NAMES:
        source = _resolve_host_tokenless_binary(binary_name)
        logger.info("OPENCLAW_TOKENLESS_BINARY_RESOLVED name=%s path=%s", binary_name, source)
        target = tokenless_target / binary_name
        shutil.copy2(source, target)
        records.append(_tokenless_binary_record(binary_name, source, target))

    (tokenless_root / "injection.json").write_text(
        json.dumps({"schema_version": 1, "binaries": records}, indent=2),
        encoding="utf-8",
    )


def _enable_tokenless_plugin(config: dict[str, object]) -> None:
    plugins = config.setdefault("plugins", {})
    if not isinstance(plugins, dict):
        raise RuntimeError("Invalid OpenClaw config: plugins must be an object")

    entries = plugins.setdefault("entries", {})
    if not isinstance(entries, dict):
        raise RuntimeError("Invalid OpenClaw config: plugins.entries must be an object")
    tokenless_entry = entries.setdefault(_TOKENLESS_PLUGIN_ID, {})
    if not isinstance(tokenless_entry, dict):
        raise RuntimeError("Invalid OpenClaw config: plugins.entries.tokenless must be an object")
    tokenless_entry["enabled"] = True

    if "allow" in plugins:
        allowed_plugins = plugins["allow"]
        if not isinstance(allowed_plugins, list):
            raise RuntimeError("Invalid OpenClaw config: plugins.allow must be an array")
        if _TOKENLESS_PLUGIN_ID not in allowed_plugins:
            allowed_plugins.append(_TOKENLESS_PLUGIN_ID)


class OpenClawSandboxManager:
    """Configure one sandbox agent inside one per-instance OpenClaw profile."""

    def __init__(
        self,
        *,
        config_path: Path,
        profile: str,
        cli_path: str = "openclaw",
        tokenless: bool = False,
    ) -> None:
        self._config_path = config_path
        self._profile = profile
        self._cli_path = cli_path
        self._tokenless = tokenless

    def configure(self, spec: OpenClawSandboxSpec) -> None:
        """Write one sandbox agent entry for this profile and recreate that sandbox."""
        spec.workspace_root.mkdir(parents=True, exist_ok=True)
        if not spec.testbed_dir.is_dir():
            raise RuntimeError(f"OpenClaw testbed bind source does not exist: {spec.testbed_dir}")

        config = load_openclaw_config(self._config_path)
        _write_agents(spec.workspace_root, spec.agents_text)
        if self._tokenless:
            _expose_tokenless_plugin_extension(self._config_path.parent)
            _inject_tokenless_binaries(spec.workspace_root)

        agent_list = self._ensure_agent_list(config)
        _apply_openclaw_stability_defaults(config)
        if self._tokenless:
            _enable_tokenless_plugin(config)

        agent_entry = next(
            (item for item in agent_list if isinstance(item, dict) and item.get("id") == spec.agent_id),
            None,
        )
        if agent_entry is None:
            agent_entry = {"id": spec.agent_id, "name": spec.agent_id}
            agent_list.append(agent_entry)

        agent_entry.update(
            self._agent_entry(
                spec.agent_id,
                spec.image_name,
                spec.workspace_root,
                spec.testbed_dir,
            )
        )
        self._write_config(config)

        self.remove_agent_containers(spec.agent_id)
        self._check_agent_config(spec)

    def remove_agent_containers(self, agent_id: str) -> None:
        run_command(
            [self._cli_path, "--profile", self._profile, "sandbox", "recreate", "--agent", agent_id, "--force"],
            check=True,
        )
        self.remove_stale_agent_containers(agent_id)

    def remove_stale_agent_containers(self, agent_id: str) -> None:
        scope_key = build_openclaw_agent_scope_key(agent_id)
        result = run_command(
            ["docker", "ps", "-aq", "--filter", f"label={_OPENCLAW_SANDBOX_SESSION_LABEL}={scope_key}"],
        )
        if result.returncode != 0:
            logger.warning(
                "OPENCLAW_STALE_SANDBOX_LIST_FAILED agent_id=%s stderr=%s",
                agent_id,
                result.stderr.strip(),
            )
            return

        container_ids = [line.strip() for line in result.stdout.splitlines() if line.strip()]
        if not container_ids:
            return

        remove_result = run_command(
            ["docker", "rm", "-f", *container_ids],
        )
        if remove_result.returncode != 0:
            logger.warning(
                "OPENCLAW_STALE_SANDBOX_REMOVE_FAILED agent_id=%s containers=%s stderr=%s",
                agent_id,
                ",".join(container_ids),
                remove_result.stderr.strip(),
            )
            return

        logger.info(
            "OPENCLAW_STALE_SANDBOX_REMOVED agent_id=%s containers=%s",
            agent_id,
            ",".join(container_ids),
        )

    def _ensure_agent_list(self, config: dict[str, object]) -> list[object]:
        agents = config.setdefault("agents", {})
        if not isinstance(agents, dict):
            raise RuntimeError("Invalid OpenClaw config: agents must be an object")
        agent_list = agents.setdefault("list", [])
        if not isinstance(agent_list, list):
            raise RuntimeError("Invalid OpenClaw config: agents.list must be an array")
        return agent_list

    def _agent_entry(
        self,
        agent_id: str,
        image_name: str,
        workspace_root: Path,
        testbed_dir: Path,
    ) -> dict[str, object]:
        docker_path = _DEFAULT_PATH_SUFFIX
        if self._tokenless:
            docker_path = f"{_TOKENLESS_RUNTIME_BIN_DIR}:{_DEFAULT_PATH_SUFFIX}"

        docker_config: dict[str, object] = {
            "image": image_name,
            "workdir": _WORKSPACE_PATH,
            "dangerouslyAllowExternalBindSources": True,
            "env": {
                "PATH": docker_path,
                "VIRTUAL_ENV": "/opt/miniconda3/envs/testbed",
                "PYTEST_ADDOPTS": f"-o cache_dir={_PYTEST_CACHE_DIR}",
                "HYPOTHESIS_STORAGE_DIRECTORY": _HYPOTHESIS_CACHE_DIR,
            },
            "setupCommand": _SETUP_COMMAND,
            "binds": [f"{testbed_dir}:{_TESTBED_MOUNT_PATH}:rw"],
        }

        return {
            "id": agent_id,
            "name": agent_id,
            "workspace": str(workspace_root),
            "sandbox": {
                "mode": "all",
                "backend": "docker",
                "scope": "agent",
                "workspaceAccess": "rw",
                "workspaceRoot": str(workspace_root),
                "docker": docker_config,
            },
        }

    def _write_config(self, config: dict[str, object]) -> None:
        self._config_path.parent.mkdir(parents=True, exist_ok=True)
        temp_path = self._config_path.with_suffix(f"{self._config_path.suffix}.tmp")
        temp_path.write_text(json.dumps(config, indent=2), encoding="utf-8")
        temp_path.replace(self._config_path)

    def _check_agent_config(self, spec: OpenClawSandboxSpec) -> None:
        result = run_command(
            [self._cli_path, "--profile", self._profile, "sandbox", "explain", "--agent", spec.agent_id, "--json"],
            check=True,
        )
        try:
            data = json.loads(result.stdout)
        except json.JSONDecodeError as exc:
            raise RuntimeError(f"OpenClaw sandbox explain returned invalid JSON for {spec.agent_id}") from exc

        sandbox = data.get("sandbox")
        workspace_root = sandbox.get("workspaceRoot") if isinstance(sandbox, dict) else None
        if workspace_root != str(spec.workspace_root):
            raise RuntimeError(
                "OpenClaw sandbox explain does not reference the expected workspaceRoot "
                f"for {spec.agent_id}: {spec.workspace_root}"
            )
