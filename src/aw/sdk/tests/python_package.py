"""Build core and optional Python SDK wheels outside the checkout and run contracts."""

import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import zipfile
from semantic_contract import run as semantic
from event_contract import run as events
from source_contract import run as language_source


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


def build_wheel(python: str, package: Path, out: Path, environment: dict[str, str]) -> Path:
    out.mkdir(parents=True, exist_ok=True)
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
            str(out),
        ],
        out,
        environment,
    )
    (wheel,) = out.glob("*.whl")
    return wheel


def assert_core_wheel_boundary(wheel: Path) -> None:
    with zipfile.ZipFile(wheel) as archive:
        names = archive.namelist()
    forbidden = [
        name
        for name in names
        if name.startswith(("aw_sdk/_native/", "aw_sdk/_native\\"))
    ]
    assert not forbidden, f"core wheel must not bundle framework adapters: {forbidden}"
    for required in (
        "aw_sdk/client.py",
        "aw_sdk/admission.py",
        "aw_sdk/task_scope.py",
        "aw_sdk/deployment.py",
        "aw_sdk/control.py",
        "aw_sdk/sources.py",
        "aw_sdk/_source_mutation.py",
        "aw_sdk/_auto_runtime.py",
        "aw_sdk/auto_instrument.py",
    ):
        assert required in names, f"core wheel missing {required}"


def main(binary: str) -> None:
    sdk = Path(__file__).resolve().parents[1]
    core_package = sdk / "python"
    adapters = sdk / "python-adapters"
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
        # Seed a stale _native file in the core build dir to prove incremental rebuilds purge it.
        stale_native = sdk.parent / "target/python-sdk/build/lib/aw_sdk/_native/qwenpaw/stale.py"
        stale_native.parent.mkdir(parents=True, exist_ok=True)
        stale_native.write_text("# stale legacy bridge\n")
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
            core_future = pool.submit(
                build_wheel, python, core_package, root / "wheels-core", environment
            )
            claude_future = pool.submit(
                build_wheel, python, adapters / "claude", root / "wheels-claude", environment
            )
            qwenpaw_future = pool.submit(
                build_wheel, python, adapters / "qwenpaw", root / "wheels-qwenpaw", environment
            )
            core_wheel = core_future.result()
            claude_wheel = claude_future.result()
            qwenpaw_wheel = qwenpaw_future.result()
        assert_core_wheel_boundary(core_wheel)
        core_installed = root / "installed-core"
        run(
            [
                python,
                "-m",
                "pip",
                "install",
                "--no-deps",
                "--no-index",
                "--target",
                str(core_installed),
                str(core_wheel),
            ],
            root,
            environment,
        )
        core_env = dict(environment, PYTHONPATH=str(core_installed))
        # Core-only install loads zero framework adapters and gives actionable install errors.
        run(
            [
                sys.executable,
                "-c",
                "import aw_sdk, aw_sdk.sources, sys\n"
                "assert 'aw_sdk_claude' not in sys.modules and 'aw_sdk_qwenpaw' not in sys.modules\n"
                "for mod, attr, pkg in (\n"
                "    ('aw_sdk.claude', 'ResidentClaude', 'anolisa-aw-sdk-claude'),\n"
                "    ('aw_sdk.qwenpaw', 'QwenPawPlugin', 'anolisa-aw-sdk-qwenpaw'),\n"
                "    ('aw_sdk.qwenpaw', 'AfterOnlyPlugin', 'anolisa-aw-sdk-qwenpaw'),\n"
                "):\n"
                "    try:\n"
                "        getattr(__import__(mod, fromlist=[attr]), attr)\n"
                "    except ModuleNotFoundError as exc:\n"
                "        assert pkg in str(exc), str(exc)\n"
                "    else:\n"
                "        raise AssertionError(f'missing optional adapter {pkg} succeeded')\n"
                "import aw_sdk.qwenpaw\n"
                "try:\n"
                "    aw_sdk.qwenpaw.prepare_runtime()\n"
                "except ModuleNotFoundError as exc:\n"
                "    assert 'anolisa-aw-sdk-qwenpaw' in str(exc), str(exc)\n"
                "else:\n"
                "    raise AssertionError('prepare_runtime succeeded without optional adapter')\n",
            ],
            root,
            core_env,
        )
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            futures = [
                pool.submit(
                    semantic,
                    binary,
                    [sys.executable, str(sdk / "tests/semantic_consumer.py")],
                    root,
                    core_env,
                ),
                pool.submit(
                    events,
                    binary,
                    [sys.executable, str(sdk / "tests/event_consumer.py")],
                    root,
                    core_env,
                ),
                pool.submit(
                    language_source,
                    binary,
                    [sys.executable, str(sdk / "tests/source_consumer.py")],
                    root,
                    core_env,
                ),
                pool.submit(
                    run,
                    [sys.executable, "-m", "unittest", "discover", "-s", str(core_package / "tests")],
                    root,
                    core_env,
                ),
            ]
            for future in futures:
                future.result()

        # Install optional framework adapter wheels alongside core and verify zero regression.
        run(
            [
                python,
                "-m",
                "pip",
                "install",
                "--no-deps",
                "--no-index",
                "--target",
                str(core_installed),
                str(claude_wheel),
                str(qwenpaw_wheel),
            ],
            root,
            environment,
        )
        run(
            [
                sys.executable,
                "-c",
                "import aw_sdk, sys; "
                "from aw_sdk.claude import ResidentClaude; "
                "from aw_sdk_claude import ResidentClaude as DirectClaude; "
                "from aw_sdk.qwenpaw import AfterOnlyPlugin; "
                "from aw_sdk_qwenpaw import AfterOnlyPlugin as DirectAfter; "
                "assert ResidentClaude is DirectClaude and AfterOnlyPlugin is DirectAfter; "
                "assert 'claude_agent_sdk' not in sys.modules; "
                "assert 'agentscope' not in sys.modules; assert 'qwenpaw' not in sys.modules",
            ],
            root,
            core_env,
        )
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            adapter_futures = [
                pool.submit(
                    run,
                    [
                        sys.executable,
                        "-m",
                        "unittest",
                        "discover",
                        "-s",
                        str(adapters / adapter / "tests"),
                    ],
                    root,
                    core_env,
                )
                for adapter in ("claude", "qwenpaw")
            ]
            for future in adapter_futures:
                future.result()
    print(json.dumps({"status": "passed", "package": "python"}))


if __name__ == "__main__":
    main(sys.argv[1])
