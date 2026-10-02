"""Resident headroom compression worker driven by the Rust L2 harness.

Standalone by design: imports only the standard library plus headroom, since
the headroom venv has no harness package installed. Speaks a line-delimited
JSON protocol over stdin/stdout:

  handshake (on start):  {"ready": true, "revision": "...", "dirty": false,
                          "untracked": 0}
                         {"ready": false, "error": "..."}   (then exit 1)
  request  (one line):   {"id": "s1", "content": "...", "context": ""}
  response (one line):   {"id": "s1", "compressed": "...", "strategy_used": "...",
                          "wall_time_s": 0.0123,
                          "hr_tokens_before": 123, "hr_tokens_after": 45}
  shutdown:              stdin EOF

The handshake reports the revision of the headroom package that was actually
imported (and whether its checkout is dirty) so a report can attribute a
benchmark delta to a specific comparator build rather than to "some headroom".

wall_time_s wraps only router.compress() (perf_counter) so it measures pure
compression, not JSON framing. hr_tokens_before/after are headroom's own
token counts (RouterCompressionResult.total_original_tokens /
total_compressed_tokens) — cross-check evidence only; the authoritative token
counts are taken on the Rust side with tiktoken-rs.
"""

from __future__ import annotations

import json
import subprocess
import sys
import time


def _emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj, ensure_ascii=False, default=str) + "\n")
    sys.stdout.flush()


def _provenance(module) -> tuple[str | None, bool | None, int | None]:
    """Return (revision, dirty, untracked) of the imported module's checkout.

    Best-effort: a wheel install or a missing git binary yields (None, None,
    None), which the report renders as "unknown" rather than silently claiming
    the comparator was pinned.

    `dirty` counts modifications to tracked files only, matching
    `git describe --dirty`. `untracked` counts untracked files in the tree,
    reported separately because the editable package imports whatever sits in
    its source dir: an untracked module changes what ran without touching the
    revision or the tracked-dirty flag.

    The checkout is usually rsynced in and therefore owned by another uid, which
    makes git refuse it as "dubious ownership". A read-only revision query is
    safe there, so the exception is passed per-invocation with -c and scoped to
    the discovered root; no git config file is ever modified.
    """
    import pathlib

    src = getattr(module, "__file__", None)
    if not src:
        return None, None, None
    root = None
    for candidate in pathlib.Path(src).resolve().parents:
        if (candidate / ".git").exists():
            root = candidate
            break
    if root is None:
        return None, None, None
    base = ["git", "-c", f"safe.directory={root}", "-C", str(root)]
    try:
        rev = subprocess.run(
            [*base, "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        if rev.returncode != 0:
            return None, None, None
        status = subprocess.run(
            [*base, "status", "--porcelain", "--untracked-files=no"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        dirty = bool(status.stdout.strip()) if status.returncode == 0 else None
        others = subprocess.run(
            [*base, "ls-files", "--others", "--exclude-standard"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        untracked = (
            len([ln for ln in others.stdout.splitlines() if ln.strip()])
            if others.returncode == 0
            else None
        )
        return rev.stdout.strip(), dirty, untracked
    except (OSError, subprocess.SubprocessError):
        return None, None, None


def main() -> int:
    try:
        import headroom
        from headroom.transforms.content_router import ContentRouter
    except Exception as exc:  # noqa: BLE001 - report any import failure
        _emit({"ready": False, "error": f"{type(exc).__name__}: {exc}"})
        return 1

    try:
        router = ContentRouter(config=None, observer=None)
    except Exception as exc:  # noqa: BLE001
        _emit({"ready": False, "error": f"{type(exc).__name__}: {exc}"})
        return 1

    revision, dirty, untracked = _provenance(headroom)
    _emit(
        {
            "ready": True,
            "revision": revision,
            "dirty": dirty,
            "untracked": untracked,
        }
    )

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError as exc:
            _emit({"id": None, "error": f"bad request: {exc}"})
            continue
        req_id = req.get("id")
        try:
            start = time.perf_counter()
            result = router.compress(
                req.get("content", ""), context=req.get("context", "")
            )
            elapsed = time.perf_counter() - start
            _emit(
                {
                    "id": req_id,
                    "compressed": result.compressed,
                    "strategy_used": getattr(result, "strategy_used", None),
                    "wall_time_s": elapsed,
                    "hr_tokens_before": getattr(
                        result, "total_original_tokens", None
                    ),
                    "hr_tokens_after": getattr(
                        result, "total_compressed_tokens", None
                    ),
                }
            )
        except Exception as exc:  # noqa: BLE001 - keep the loop alive
            _emit({"id": req_id, "error": f"{type(exc).__name__}: {exc}"})
    return 0


if __name__ == "__main__":
    sys.exit(main())
