"""Evaluation service works on platforms without os.getuid (native Windows).

`_ensure_docker_host_for_rootless_context` used to evaluate the default
``rootless_socket`` argument ``Path(f"/run/user/{os.getuid()}/docker.sock")``
whenever DOCKER_HOST is unset — os.getuid does not exist on Windows, so
every `evaluate` invocation crashed with AttributeError before swebench was
even driven.
"""

import os
from pathlib import Path

from swe_runner.evaluation.service import _ensure_docker_host_for_rootless_context


def test_default_socket_no_crash_on_platforms_without_getuid(
    tmp_path: Path, monkeypatch
) -> None:
    """无 os.getuid 的平台上（原生 Windows）默认 socket 探测不应 AttributeError。

    修复前：DOCKER_HOST 未设置且未显式传 rootless_socket 时，默认参数
    f"/run/user/{os.getuid()}/..." 立即裸崩（实测 AttributeError）。
    """
    monkeypatch.delenv("DOCKER_HOST", raising=False)
    monkeypatch.delattr(os, "getuid", raising=False)
    # 默认 rootless_socket 参数的求值路径必须被平台守卫接住：不抛即通过
    _ensure_docker_host_for_rootless_context(docker_config=tmp_path / "config.json")


def test_rootless_detection_still_works_with_getuid(tmp_path: Path, monkeypatch) -> None:
    """正常链路保护：有 getuid 的平台显式传参照常执行 rootless 检测。"""
    monkeypatch.delenv("DOCKER_HOST", raising=False)
    config = tmp_path / "config.json"
    config.write_text('{"currentContext": "rootless"}')
    sock = tmp_path / "docker.sock"
    sock.touch()
    _ensure_docker_host_for_rootless_context(docker_config=config, rootless_socket=sock)
    assert os.environ.get("DOCKER_HOST") is not None
