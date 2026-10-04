"""Shared scan budgets for Skill content scanners.

The daemon Skill Ledger worker calls the certifier scan path directly, so
these module-level budgets are the single source of truth enforced inside
the scanners themselves (not only in ``analyze.py``'s read-only pre-flight).
They bound the file count, directory depth, and aggregate tree bytes a
single managed Skill can force a scanner worker to walk, and are shared by
the static scanner so its peak memory cannot scale without bound with a
Skill's aggregate text size. Keep them aligned across consumers; change
them only with a documented memory budget in mind.
"""

MAX_FILES = 2_000
MAX_TOTAL_BYTES = 50 * 1024 * 1024
MAX_DIRECTORY_DEPTH = 32
