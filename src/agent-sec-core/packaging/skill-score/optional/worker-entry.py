"""Enter the isolated optional worker without relying on ambient Python paths."""

import sys
from pathlib import Path

root = Path(__file__).resolve().parent
sys.path[:0] = [str(root / name) for name in ("worker", "shared", "site-packages")]
from agent_sec_skill_score_runtime.worker import main  # noqa: E402

main()
