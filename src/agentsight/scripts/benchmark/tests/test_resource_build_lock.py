"""Cargo unlocks its target before Inputs.build copies the selected binary."""

import hashlib
import shutil
import threading
from pathlib import Path
from unittest.mock import patch

import pytest
from test_resource_run import resource_inputs


@pytest.mark.parametrize("ffi", [False, True])
def test_concurrent_build_cannot_replace_binary_before_publication(
    tmp_path: Path, ffi: bool
) -> None:
    cache = tmp_path / "cache"
    artifact = "libagentsight.so" if ffi else "agentsight"
    target = cache / "target/release" / artifact
    target.parent.mkdir(parents=True)
    sources = []
    for label in ("before", "after"):
        directory = tmp_path / label
        directory.mkdir()
        sources.append(
            {"kind": "git", "component": str(directory), "binary": str(directory / "agentsight")}
        )
    copying = threading.Event()
    second_built = threading.Event()
    errors: list[BaseException] = []
    original_copy = shutil.copy2

    def run(command: list[str], log: Path, **kwargs: object) -> int:
        if "-o" in command:
            runner = Path(command[command.index("-o") + 1])
            runner.write_bytes(runner.parent.name.encode())
            return 0
        label = Path(str(kwargs["cwd"])).name
        target.write_bytes(label.encode())
        if label == "after":
            second_built.set()
        return 0

    def copy(source: Path, destination: str, **kwargs: object) -> str:
        if Path(destination) == Path(sources[0]["binary"]).parent / artifact:
            copying.set()
            second_built.wait(timeout=0.4)
        return original_copy(source, destination, **kwargs)

    inputs = resource_inputs.Inputs(tmp_path, tmp_path, ffi=ffi)

    def build(index: int) -> None:
        try:
            inputs.build(sources[index], run, cache)
        except BaseException as error:
            errors.append(error)

    try:
        with patch.object(resource_inputs.shutil, "copy2", side_effect=copy):
            first = threading.Thread(target=build, args=(0,))
            second = threading.Thread(target=build, args=(1,))
            first.start()
            assert copying.wait(timeout=3)
            second.start()
            first.join(timeout=5)
            second.join(timeout=5)
            assert not first.is_alive() and not second.is_alive()
        assert not errors
        for label, source in zip(("before", "after"), sources):
            assert Path(source["binary"]).read_bytes() == label.encode()
            assert source["sha256"] == hashlib.sha256(label.encode()).hexdigest()
            if ffi:
                assert Path(source["library"]).read_bytes() == label.encode()
                assert source["library_sha256"] == hashlib.sha256(label.encode()).hexdigest()
    finally:
        inputs.close()
