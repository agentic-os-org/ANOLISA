"""Helpers shared by the subprocess output encoding regressions.

Each regression runs a child process whose textual output is decoded by the
caller under a forced C locale, where Python falls back to an ASCII codec. The
helpers here keep paths and bytes independent of the parent's codec so the
fixture itself keeps working when the locale cannot represent a path.
"""

import base64
import os
import subprocess
import sys
from pathlib import Path

BENCHMARK_DIR = Path(__file__).parents[1]
SCRIPTS = BENCHMARK_DIR.parent
NON_ASCII = "\u4e2d\u6587"

sys.path.insert(0, str(BENCHMARK_DIR / "campaign"))
sys.path.insert(0, str(BENCHMARK_DIR / "single_run"))


def encode(value: str) -> str:
    """Pass a path through argv without depending on the parent's codec."""
    return base64.urlsafe_b64encode(os.fsencode(value)).decode("ascii")


def non_ascii_directory(root: Path) -> Path:
    """Create a non-ASCII directory under ``root`` using raw bytes."""
    target = os.fsdecode(os.fsencode(root) + b"/" + NON_ASCII.encode("utf-8"))
    os.mkdir(target)
    return Path(target)


def c_locale_env(**extra: str) -> dict:
    """A host locale that cannot decode UTF-8, with the console kept UTF-8."""
    return {
        **os.environ,
        "LC_ALL": "C",
        "LANG": "C",
        "PYTHONCOERCECLOCALE": "0",
        "PYTHONUTF8": "0",
        "PYTHONIOENCODING": "utf-8",
        **extra,
    }


def fake_utility(root: Path, name: str, stdout: bytes, stderr: bytes, status: int) -> Path:
    """Stand in for a command so the fixture prints its own raw bytes."""
    bin_dir = root / "bin"
    bin_dir.mkdir(exist_ok=True)
    utility = bin_dir / name
    utility.write_text(
        f"#!{sys.executable}\n"
        "import sys\n"
        f"sys.stdout.buffer.write({stdout!r})\n"
        f"sys.stderr.buffer.write({stderr!r})\n"
        f"sys.exit({status})\n",
        encoding="utf-8",
    )
    utility.chmod(0o700)
    return bin_dir


def run_helper(
    root: Path, body: str, *arguments: str, path_prefix: Path | None = None
) -> subprocess.CompletedProcess:
    """Run a helper script under the C locale and return its decoded output."""
    helper = root / "helper.py"
    helper.write_text(body, encoding="utf-8")
    extra = {}
    if path_prefix is not None:
        extra["PATH"] = f"{path_prefix}:{os.environ.get('PATH', '')}"
    return subprocess.run(
        [sys.executable, str(helper), *arguments],
        env=c_locale_env(**extra),
        cwd=root,
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=30,
    )
