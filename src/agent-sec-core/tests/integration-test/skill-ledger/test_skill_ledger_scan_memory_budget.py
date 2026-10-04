"""Linux integration test: static scanner peak memory respects the budget.

Reproduces the shape of the Skill Ledger worker peak reported in the
memory-bound issue (a single managed Skill whose aggregate text exceeds the
scan budget), scaled down for CI: one Skill with 60 text files of 900,000
bytes each (54,000,000 bytes total > the 50 MiB aggregate budget).

The test drives the scan in a dedicated worker subprocess — the same
``scan_skill`` entry point the daemon's Skill Ledger worker reaches through
the certifier path — while sampling, from this (Main) process:

- the worker's PSS from ``/proc/<pid>/smaps_rollup``,
- the daemon-analog MainPID RSS of this process,
- the worker's cgroup ``memory.current`` (best effort; v2 only, often
  unavailable or inflated by page cache in containers).

The budget is respected when (a) the scan stops with the structured
``total-size-limit`` diagnostic instead of scanning unbounded content, and
(b) the worker's peak PSS stays below the aggregate-byte budget — the
pre-fix scanner retained every decoded file until all rules finished, which
drove peak memory above the corpus size.
"""

import json
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

from agent_sec_cli.skill_ledger.scanner.limits import MAX_TOTAL_BYTES
from agent_sec_cli.skill_ledger.scanner.names import STATIC_SCANNER_NAME

_IS_LINUX = sys.platform.startswith("linux")
_PROC = Path("/proc")

_WORKER_SCRIPT = """
import json
import sys

from agent_sec_cli.skill_ledger.scanner.builtins.cisco_static.scanner import (
    scan_skill,
)

findings = scan_skill(sys.argv[1])
print(json.dumps([finding["rule"] for finding in findings]))
"""

_FILE_COUNT = 60
_FILE_BYTES = 900_000
_CORPUS_BYTES = _FILE_COUNT * _FILE_BYTES


def _read_proc_int(path: Path, key: str) -> int | None:
    try:
        for line in path.read_text().splitlines():
            if line.startswith(key):
                return int(line.split()[1])
    except (OSError, ValueError):
        return None
    return None


def _worker_pss(pid: int) -> int | None:
    return _read_proc_int(_PROC / str(pid) / "smaps_rollup", "Pss:")


def _main_pid_rss() -> int | None:
    return _read_proc_int(_PROC / "self" / "status", "VmRSS:")


def _worker_cgroup_memory_current(pid: int) -> int | None:
    try:
        cgroup_line = (_PROC / str(pid) / "cgroup").read_text().splitlines()[0]
    except (OSError, IndexError):
        return None
    path = cgroup_line.split("::", 1)[-1].strip()
    if not path.startswith("/"):
        return None
    memory_file = Path("/sys/fs/cgroup") / path.lstrip("/") / "memory.current"
    try:
        return int(memory_file.read_text().strip())
    except (OSError, ValueError):
        return None


def _build_large_skill(root: Path) -> Path:
    skill_dir = root / "oversized-skill"
    skill_dir.mkdir()
    (skill_dir / "SKILL.md").write_text(
        "---\nname: oversized\ndescription: Local synthetic corpus\n---\n# Oversized\n",
        encoding="utf-8",
    )
    chunk = "x" * 4096
    for index in range(_FILE_COUNT):
        with (skill_dir / f"corpus-{index:04d}.txt").open("w") as fh:
            written = 0
            while written < _FILE_BYTES:
                fh.write(chunk)
                written += len(chunk)
    return skill_dir


@unittest.skipUnless(_IS_LINUX, "Linux /proc memory sampling required")
class TestStaticScannerMemoryBudget(unittest.TestCase):
    """A single oversized Skill cannot scale worker peak memory without bound."""

    def test_large_skill_scan_respects_budget(self) -> None:
        with tempfile.TemporaryDirectory(prefix="asc-mem-budget-") as tmp:
            skill_dir = _build_large_skill(Path(tmp))
            self.assertGreater(_CORPUS_BYTES, MAX_TOTAL_BYTES)

            worker = subprocess.Popen(
                [sys.executable, "-c", _WORKER_SCRIPT, str(skill_dir)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            peak_pss = 0
            peak_cgroup = 0
            main_pid_rss_samples: list[int] = []
            try:
                while worker.poll() is None:
                    pss = _worker_pss(worker.pid)
                    if pss is not None:
                        peak_pss = max(peak_pss, pss)
                    cgroup_mem = _worker_cgroup_memory_current(worker.pid)
                    if cgroup_mem is not None:
                        peak_cgroup = max(peak_cgroup, cgroup_mem)
                    main_rss = _main_pid_rss()
                    if main_rss is not None:
                        main_pid_rss_samples.append(main_rss)
                    time.sleep(0.005)
            finally:
                stdout, stderr = worker.communicate(timeout=60)

            self.assertEqual(
                worker.returncode,
                0,
                f"worker scan failed: {stderr}",
            )
            rules = json.loads(stdout)

            # (a) The aggregate budget stopped the scan with a structured
            # diagnostic. For an ASCII corpus the cumulative decoded-text
            # cap (50M chars) trips just before the tree-byte cap (50 MiB).
            self.assertIn("cumulative-text-limit", rules)

            # (b) Peak worker PSS stayed under the aggregate-byte budget.
            # The pre-fix scanner retained all decoded text (~54 MB here),
            # pushing peak PSS above the corpus size; the streaming scan
            # holds at most one file plus interpreter overhead.
            # smaps_rollup reports Pss in KiB; the budget is in bytes.
            self.assertGreater(peak_pss, 0, "failed to sample worker PSS")
            self.assertLess(
                peak_pss * 1024,
                MAX_TOTAL_BYTES,
                "worker peak PSS exceeded the configured aggregate-byte budget",
            )

            # Sampling context: MainPID RSS (daemon analog) must not grow
            # with the corpus, and cgroup memory (when readable) is recorded
            # for diagnosis — page cache makes it an upper bound, not an
            # anonymous-memory oracle.
            if main_pid_rss_samples:
                self.assertLess(max(main_pid_rss_samples), MAX_TOTAL_BYTES)
            print(
                f"\npeak worker PSS={peak_pss} KiB "
                f"(budget {MAX_TOTAL_BYTES} bytes, corpus {_CORPUS_BYTES} bytes), "
                f"peak cgroup memory.current={peak_cgroup or 'n/a'} bytes, "
                f"MainPID RSS samples={len(main_pid_rss_samples)}"
            )

    def test_scanner_name_unchanged(self) -> None:
        self.assertEqual(STATIC_SCANNER_NAME, "static-scanner")


if __name__ == "__main__":
    unittest.main()
