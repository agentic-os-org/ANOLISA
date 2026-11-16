"""Read deployment configuration through AW's authoritative offline parser."""

import json
import os
import selectors
import signal
import time
from pathlib import Path
import subprocess

from aw_sdk.transport import AwError


def load_configuration(
    config_file: str, adapter: str, target: str | None, executable: str
) -> tuple[dict, str, str]:
    if not isinstance(config_file, str) or not Path(config_file).is_absolute():
        raise AwError("sdk_configuration_required")
    spec = json.loads(_configuration_output(executable, config_file))["spec"]
    candidates = [name for name, agent in spec["agents"].items() if agent["adapter"] == adapter]
    if target is None and len(candidates) == 1:
        target = candidates[0]
    if target not in candidates:
        raise AwError("sdk_target_required")
    daemon = spec["daemon"]
    endpoint = daemon["endpoint"]
    if endpoint == "auto":
        endpoint = str(Path(daemon["state_dir"]) / "aw.sock")
    if not Path(endpoint).is_absolute():
        raise AwError("sdk_explicit_service_path_required")
    return spec, target, endpoint


def _configuration_output(executable: str, config_file: str) -> bytes:
    command = [executable, "config", "show", "--config", config_file]
    deadline = time.monotonic() + 5
    buffers = {"stdout": bytearray(), "stderr": bytearray()}
    limits = {"stdout": 9 * 1024 * 1024, "stderr": 65536}
    with subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    ) as process, selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ, "stdout")
        selector.register(process.stderr, selectors.EVENT_READ, "stderr")
        try:
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise subprocess.TimeoutExpired(command, 5)
                for key, _ in selector.select(remaining):
                    chunk = os.read(key.fd, 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    buffer = buffers[key.data]
                    if len(buffer) + len(chunk) > limits[key.data]:
                        raise AwError("sdk_configuration_too_large")
                    buffer.extend(chunk)
            code = process.wait(timeout=max(0, deadline - time.monotonic()))
            if code:
                raise subprocess.CalledProcessError(
                    code, command, bytes(buffers["stdout"]), bytes(buffers["stderr"])
                )
            return bytes(buffers["stdout"])
        except BaseException:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
            raise
