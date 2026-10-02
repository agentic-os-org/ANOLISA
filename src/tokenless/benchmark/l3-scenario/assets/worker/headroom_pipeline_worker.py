#!/usr/bin/env python3
# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""the reference side of the L3 comparison: one conversation in, one out.

Line-delimited JSON over stdin/stdout, one JSON object per line:

  handshake (on start):  {"ready": true, "revision": ..., "dirty": ...,
                          "untracked": ..., "tokenizer": "mock-chars4",
                          "variants": ["pure_stage", "router"]}
                         {"ready": false, "error": "..."}   (then exit 1)
  request:               {"messages": [...], "model": "...", "model_limit": N,
                          "variant": "pure_stage"|"router"}
  response:              {"ok": true, "messages": [...], "compress_ms": ...,
                          "tokens_before": N, "tokens_after": N,
                          "transforms_applied": [...], "timing": {...}}
                         {"ok": false, "error": "..."}

Kept as a long-lived process rather than one invocation per scenario: importing
importing it costs far more than a single compression, so per-call spawning would
bury the measurement in interpreter start-up.

Two pipeline variants are built up front and selected per request, because they
answer different questions and neither alone is a fair summary:

- `pure_stage`: `CacheAligner -> SmartCrusher`, exactly what the reference's
  `bench_transforms.py` fixture assembles (its comment notes ContentRouter is
  "omitted here to keep the fixture pure-stage"). Comparable against the reference's
  own 5/30/50 ms targets.
- `router`: the same two plus `ContentRouter`, which the reference's own docstring
  calls "the recommended entry point for The reference's compression". Without it the
  content-specific compressors (CodeAware, Log, Diff, Search, Html, TextCrusher)
  never run, so source code and prose would report 0% for reasons of pipeline
  assembly rather than capability.

Token counting inside the reference uses the same chars/4 estimator its benchmark
fixtures inject, so crushing decisions and latency match the terms the reference sets
for itself. This is *not* the authoritative count: the harness re-counts both
sides with tiktoken, and `tokens_before`/`tokens_after` here are the reference's own
self-report, useful only as corroboration.

`compress_ms` is measured inside this process around `pipeline.apply` only, so
pipe and JSON framing overhead stay out of it. That basis differs from the
tokenless side's in-process timing and the two must not be compared directly.

the reference writes advisory notices (e.g. CacheAligner's volatile-prefix warning)
to stdout, which would corrupt the line-delimited protocol. stdout is therefore
rebound to stderr for everything except the protocol writer below.
"""

from __future__ import annotations

import json
import subprocess
import sys
import time
from typing import Any

# Real stdout, captured before anything can print to it, and used only by
# `_emit`. Everything else in this process (including the reference's own notices)
# goes to stderr, which the parent forwards for diagnosis.
_PROTOCOL_OUT = sys.stdout
sys.stdout = sys.stderr


def _emit(obj: dict) -> None:
    _PROTOCOL_OUT.write(json.dumps(obj, ensure_ascii=False, default=str) + "\n")
    _PROTOCOL_OUT.flush()


def _provenance(module: Any) -> tuple[str | None, bool | None, int | None]:
    """Return (revision, dirty, untracked) of the imported module's checkout.

    `dirty` counts modifications to tracked files only, matching
    `git describe --dirty`. `untracked` is reported separately because an
    untracked file inside the package changes what ran without moving the
    revision or the tracked-dirty flag.

    A checkout owned by another user makes git refuse it as "dubious
    ownership", so `safe.directory` is granted for this read-only query alone
    rather than mutating the user's git config.
    """
    src = getattr(module, "__file__", None)
    if not src:
        return None, None, None
    root = src.rsplit("/", 2)[0]
    base = ["git", "-c", f"safe.directory={root}", "-C", root]
    try:
        rev = subprocess.run(
            [*base, "rev-parse", "HEAD"], capture_output=True, text=True, timeout=10
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


class MockTokenCounter:
    """The chars/4 estimator the reference's own benchmark fixtures inject.

    Replicated verbatim from `benchmarks/conftest.py` because it lives in a
    pytest conftest and is not importable. Using the reference's estimator keeps its
    crushing decisions and latency on the same terms as the 5/30/50 ms targets
    it publishes for these scenarios.
    """

    def count_text(self, text: str) -> int:
        return max(1, len(text) // 4)

    def count_message(self, message: dict[str, Any]) -> int:
        content = message.get("content", "")
        if isinstance(content, str):
            return self.count_text(content) + 4  # role overhead
        if isinstance(content, list):
            total = 0
            for block in content:
                if not isinstance(block, dict):
                    continue
                if block.get("type") == "text":
                    total += self.count_text(block.get("text", ""))
                elif block.get("type") == "tool_result":
                    total += self.count_text(str(block.get("content", "")))
                elif block.get("type") == "tool_use":
                    total += self.count_text(json.dumps(block.get("input", {})))
            return total + 4
        return 10

    def count_messages(self, messages: list[dict[str, Any]]) -> int:
        return sum(self.count_message(m) for m in messages)


class MockProvider:
    """Minimal provider handing the reference the estimator above."""

    def __init__(self) -> None:
        self._counter = MockTokenCounter()

    def get_token_counter(self, model: str) -> MockTokenCounter:  # noqa: ARG002
        return self._counter


def _build_pipelines() -> dict[str, Any]:
    """Build both pipeline variants keyed by request `variant`.

    # Errors

    Propagates any import or construction failure so the handshake reports it
    rather than the harness silently measuring a pipeline that is missing a
    stage.
    """
    from headroom.config import CacheAlignerConfig, SmartCrusherConfig
    from headroom.transforms.cache_aligner import CacheAligner
    from headroom.transforms.content_router import ContentRouter
    from headroom.transforms.pipeline import TransformPipeline
    from headroom.transforms.smart_crusher import SmartCrusher

    # Verbatim from the reference's bench_transforms.py fixtures.
    crusher_config = SmartCrusherConfig(
        enabled=True,
        min_items_to_analyze=5,
        min_tokens_to_crush=0,
        max_items_after_crush=15,
        variance_threshold=2.0,
    )
    aligner_config = CacheAlignerConfig(
        enabled=True,
        normalize_whitespace=True,
        collapse_blank_lines=True,
    )
    provider = MockProvider()

    def stages() -> list[Any]:
        return [CacheAligner(aligner_config), SmartCrusher(crusher_config)]

    return {
        "pure_stage": TransformPipeline(transforms=stages(), provider=provider),
        "router": TransformPipeline(
            transforms=[*stages(), ContentRouter()], provider=provider
        ),
    }


def main() -> int:
    try:
        import headroom
    except Exception as exc:  # noqa: BLE001 - report any import failure verbatim
        _emit({"ready": False, "error": f"{type(exc).__name__}: {exc}"})
        return 1

    try:
        pipelines = _build_pipelines()
    except Exception as exc:  # noqa: BLE001 - config/API drift must be visible
        _emit({"ready": False, "error": f"{type(exc).__name__}: {exc}"})
        return 1

    revision, dirty, untracked = _provenance(headroom)
    _emit(
        {
            "ready": True,
            "revision": revision,
            "dirty": dirty,
            "untracked": untracked,
            "tokenizer": "mock-chars4",
            "variants": sorted(pipelines),
        }
    )

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            _emit({"ok": False, "error": f"bad request json: {exc}"})
            continue

        messages = request.get("messages")
        if not isinstance(messages, list):
            _emit({"ok": False, "error": "request has no messages list"})
            continue
        variant = request.get("variant") or "pure_stage"
        pipeline = pipelines.get(variant)
        if pipeline is None:
            _emit({"ok": False, "error": f"unknown variant {variant!r}"})
            continue
        model = request.get("model") or "benchmark-model"
        model_limit = request.get("model_limit") or 200_000

        try:
            start = time.perf_counter()
            result = pipeline.apply(messages, model, model_limit=model_limit)
            elapsed_ms = (time.perf_counter() - start) * 1000.0
        except Exception as exc:  # noqa: BLE001 - a failing scenario degrades, not aborts
            _emit({"ok": False, "error": f"{type(exc).__name__}: {exc}"})
            continue

        _emit(
            {
                "ok": True,
                "variant": variant,
                "messages": result.messages,
                "compress_ms": elapsed_ms,
                "tokens_before": result.tokens_before,
                "tokens_after": result.tokens_after,
                "transforms_applied": list(result.transforms_applied),
                "timing": dict(result.timing),
                "warnings": list(result.warnings),
            }
        )

    return 0


if __name__ == "__main__":
    sys.exit(main())
