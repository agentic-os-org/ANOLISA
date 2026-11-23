"""Select native wiring from trusted deployment hints or the offline parser."""

import json
import os
import selectors
import signal
import time
from pathlib import Path
import subprocess

from aw_sdk.transport import AwError


def load_configuration(
    config_file: str,
    profile: dict,
    target: str | None,
    executable: str,
    *,
    descriptor_file: str | None = None,
    requires_service: bool = False,
) -> dict:
    if not isinstance(config_file, str) or not Path(config_file).is_absolute():
        raise AwError("sdk_configuration_required")
    if descriptor_file is not None:
        from aw_sdk.deployment import load_descriptor

        return load_descriptor(
            config_file,
            descriptor_file,
            profile,
            target,
            requires_service=requires_service,
        )
    projection = json.loads(
        _configuration_output(executable, config_file, profile["adapter"], target)
    )
    selected = projection.get("target")
    if not isinstance(selected, str) or not selected or (target is not None and target != selected):
        raise AwError("sdk_target_required")
    if any(
        type(projection.get(key)) is not bool
        for key in ("input_enabled", "target_requires_service")
    ):
        raise AwError("sdk_configuration_invalid")
    daemon = projection["daemon"]
    endpoint = daemon["endpoint"]
    if endpoint == "auto":
        endpoint = str(Path(daemon["state_dir"]) / "aw.sock")
    if not Path(endpoint).is_absolute():
        raise AwError("sdk_explicit_service_path_required")
    return {
        "target": selected,
        "endpoint": endpoint,
        "input_enabled": projection["input_enabled"],
        "availability": (
            "required"
            if requires_service or projection["target_requires_service"]
            else "native_fallback"
        ),
    }


def _configuration_output(executable: str, config_file: str, adapter: str, target: str | None) -> bytes:
    selection = adapter if target is None else f"{adapter}/{target}"
    command = [executable, "config", "show", "--config", config_file, "--sdk-target", selection]
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
