"""Build a wheel, install outside the checkout and run the shared SDK contracts."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from semantic_contract import run as semantic
from event_contract import run as events


def run(command: list[str], cwd: Path, environment: dict[str, str]) -> None:
    result = subprocess.run(
        command,
        cwd=cwd,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=90,
    )
    if result.returncode:
        raise RuntimeError(result.stdout)


def main(binary: str) -> None:
    sdk = Path(__file__).resolve().parents[1]
    package = sdk / "python"
    # CI keeps its packaging interpreter separate from trusted native fixtures.
    python = (
        str(Path(os.environ["pythonLocation"]) / "bin/python3")
        if "pythonLocation" in os.environ
        else sys.executable
    )
    environment = dict(
        os.environ,
        AW_TEST_BINARY=str(Path(binary).resolve()),
        AW_CONTRACT_FIXTURE=str(sdk / "tests/fixtures/contract.json"),
    )
    with tempfile.TemporaryDirectory(prefix="aw-python-package-") as temporary:
        root = Path(temporary)
        run(
            [
                python,
                "-m",
                "pip",
                "wheel",
                "--no-build-isolation",
                "--no-deps",
                "--no-index",
                str(package),
                "--wheel-dir",
                str(root),
            ],
            root,
            environment,
        )
        (wheel,) = root.glob("*.whl")
        installed = root / "installed"
        run(
            [
                python,
                "-m",
                "pip",
                "install",
                "--no-deps",
                "--no-index",
                "--target",
                str(installed),
                str(wheel),
            ],
            root,
            environment,
        )
        environment["PYTHONPATH"] = str(installed)
        semantic(
            binary, [sys.executable, str(sdk / "tests/semantic_consumer.py")], root, environment
        )
        events(binary, [sys.executable, str(sdk / "tests/event_consumer.py")], root, environment)
        run(
            [sys.executable, "-m", "unittest", "discover", "-s", str(package / "tests")],
            root,
            environment,
        )
        run(
            [
                sys.executable,
                "-c",
                "import aw_sdk,sys; from aw_sdk.claude import ResidentClaude; "
                "from aw_sdk.qwenpaw import AfterOnlyPlugin; "
                "assert 'claude_agent_sdk' not in sys.modules; "
                "assert 'agentscope' not in sys.modules; assert 'qwenpaw' not in sys.modules",
            ],
            root,
            environment,
        )
    print(json.dumps({"status": "passed", "package": "python"}))


if __name__ == "__main__":
    main(sys.argv[1])
