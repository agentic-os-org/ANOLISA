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

"""Isolated Headroom L1 components, without ContentRouter or generative calls.

The component_cases synthetic/shared inputs use one contract. Each row retains
candidate disposition, fact checks, verified CCR originals and internal timing;
missing components are no-entry, never zero-rate failures. An isolated empty
TOIN store prevents cross-process learning from changing repeat measurements.
"""

from __future__ import annotations

import argparse
import atexit
import hashlib
import json
import os
from pathlib import Path
import re
import statistics
import tempfile
import time

os.environ["HEADROOM_BEACON"] = "off"
os.environ["DO_NOT_TRACK"] = "1"
_TOIN_DIRECTORY = None
if "HEADROOM_TOIN_PATH" not in os.environ:
    _TOIN_DIRECTORY = tempfile.TemporaryDirectory(prefix="headroom-toin-")
    atexit.register(_TOIN_DIRECTORY.cleanup)
    os.environ["HEADROOM_TOIN_PATH"] = os.path.join(_TOIN_DIRECTORY.name, "toin.json")

# Full recovery-marker prefixes exclude ordinary hashes and longer digest prefixes.
CCR_HASH_RE = re.compile(r"(?:Retrieve (?:more|original): hash=|<<ccr:)([0-9a-f]{12,24})(?![0-9a-f])(?=[.\], >]|$)")
MARKDOWN_ESCAPABLE = set("\\`*_{}[]()#+-.!|<>~")


class Component:
    """One isolated Headroom component and how to read its result."""

    def __init__(self, name: str, call, text_attr: str = "compressed"):
        self.name = name
        self.call = call
        self.text_attr = text_attr

    def __call__(self, text: str):
        result = self.call(text)
        if isinstance(result, str):
            return result, None
        output = getattr(result, self.text_attr)
        if not isinstance(output, str):
            raise TypeError(f"{self.name} returned a non-text output")
        return output, getattr(result, "strategy", None)


def build_component(domain: str, mode: str) -> Component | None:
    """The Headroom component for a Tokenless component domain, or None.

    Written against the pinned reference revision (4d7765c); a module the
    imported revision lacks makes that domain `no-entry` instead of failing.
    """
    import importlib
    import inspect

    def optional(module: str):
        try:
            return importlib.import_module(module)
        except ImportError:
            return None

    if domain == "json":
        module = optional("headroom.transforms.smart_crusher")
        if module is None or not hasattr(module, "SmartCrusher"):
            return None
        engine = module.SmartCrusher()
        # Single-component calls must not feed TOIN learning into later rows.
        if "record_toin" in inspect.signature(engine.crush).parameters:
            return Component("SmartCrusher.crush/default,record_toin=False", lambda text: engine.crush(text, record_toin=False))
        return Component("SmartCrusher.crush/default", engine.crush)
    if domain == "search":
        module = optional("headroom.transforms.search_compressor")
        if module is None or not hasattr(module, "SearchCompressor"):
            return None
        engine = module.SearchCompressor()
        return Component("SearchCompressor.compress/default", engine.compress)
    if domain == "paths":
        module = optional("headroom.transforms.lossless_compaction")
        if module is None or not hasattr(module, "path_heading"):
            return None
        return Component("lossless_compaction.path_heading", module.path_heading)
    if domain == "tabular":
        module = optional("headroom.transforms.tabular_ingest")
        if module is None or not hasattr(module, "TabularCompressor"):
            return None
        engine = module.TabularCompressor()
        return Component("TabularCompressor.compress/default", engine.compress)
    if domain == "buildlog":
        module = optional("headroom.transforms.log_compressor")
        if module is None or not all(hasattr(module, name) for name in ("LogCompressor", "LogCompressorConfig")):
            return None
        enable = mode == "recovery"
        engine = module.LogCompressor(module.LogCompressorConfig(enable_ccr=enable))
        return Component(f"LogCompressor.compress/enable_ccr={enable}", engine.compress)
    if domain == "html":
        module = optional("headroom.transforms.html_extractor")
        if module is None or not hasattr(module, "HTMLExtractor"):
            return None
        engine = module.HTMLExtractor()
        return Component("HTMLExtractor.extract/default", engine.extract, "extracted")
    if domain == "diff":
        module = optional("headroom.transforms.diff_compressor")
        if module is None or not hasattr(module, "DiffCompressor"):
            return None
        engine = module.DiffCompressor()
        return Component("DiffCompressor.compress/default", engine.compress)
    if domain == "source_code":
        module = optional("headroom.transforms.code_compressor")
        if module is None or not hasattr(module, "CodeAwareCompressor"):
            return None
        engine = module.CodeAwareCompressor()
        return Component("CodeAwareCompressor.compress/default", engine.compress)
    if domain == "plain_text":
        module = optional("headroom.transforms.text_crusher")
        if module is None or not hasattr(module, "TextCrusher"):
            return None
        engine = module.TextCrusher()
        return Component("TextCrusher.compress/default", engine.compress)
    if domain == "schema":
        module = optional("headroom.proxy.tool_schema_compaction")
        compact = getattr(module, "compact_tool_schema_value", None) if module else None
        name = "tool_schema_compaction.compact_tool_schema_value"
        if compact is None:
            handler = optional("headroom.proxy.handlers.openai")
            compact = getattr(handler, "_compact_openai_tool_schema_value", None) if handler else None
            name = "openai._compact_openai_tool_schema_value"
        if compact is None:
            return None
        return Component(
            name,
            lambda text: json.dumps(compact(json.loads(text)), ensure_ascii=False, separators=(",", ":")),
        )
    # Unclassified payloads have no isolated Headroom component.
    return None


def normalize_format(text: str) -> str:
    """L2's format-only normalization: Markdown escapes removed, whitespace collapsed."""
    out = []
    index = 0
    while index < len(text):
        char = text[index]
        if char == "\\" and index + 1 < len(text) and text[index + 1] in MARKDOWN_ESCAPABLE:
            index += 1
            continue
        out.append(char)
        index += 1
    return " ".join("".join(out).split())


def rust_lines(text: str) -> list[str]:
    """`str::lines` semantics: split on LF, drop one trailing CR, no final empty line."""
    parts = text.split("\n")
    if parts and parts[-1] == "":
        parts.pop()
    return [part[:-1] if part.endswith("\r") else part for part in parts]


DIGITS = re.compile(r"[0-9]+")


def normalized_search_records(text: str) -> str:
    """L2's path/line/body binding for grep facts, including shared-path headers."""
    out = []
    header = None
    for line in rust_lines(text):
        if line.startswith("File="):
            try:
                header = json.loads(line[len("File="):])
            except ValueError:
                header = None
            continue
        parts = line.split(":", 2)
        if len(parts) == 3 and DIGITS.fullmatch(parts[1]) and ("/" in parts[0] or "." in parts[0]):
            path = parts[0]
            if isinstance(header, str) and header.endswith("/") and "/" not in path:
                path = header + path
            out.append(f"{path}:{parts[1]}:{parts[2].strip()}")
            continue
        parts = line.split(":", 1)
        if len(parts) == 2 and DIGITS.fullmatch(parts[0]) and isinstance(header, str):
            out.append(f"{header}:{parts[0]}:{parts[1].strip()}")
            continue
        header = line if ":" not in line and ("/" in line or "." in line) else None
    return "".join(f"{line}\n" for line in out)


def check_facts(case: dict, output: str) -> dict:
    """The L1/L2 fact rules: substrings with a normalized retry, regexes, search records."""
    facts = case.get("facts", [])
    patterns = case.get("patterns", [])
    missing = []
    if case.get("search_records"):
        normalized = normalized_search_records(output)
        records = {normalize_format(record) for record in rust_lines(normalized)}
        missing += [fact for fact in facts if normalize_format(fact) not in records]
        missing += [f"/{p}/" for p in patterns if not re.search(p, normalized)]
    else:
        normalized = None
        for fact in facts:
            if fact in output:
                continue
            normalized = normalized if normalized is not None else normalize_format(output)
            if normalize_format(fact) not in normalized:
                missing.append(fact)
        missing += [f"/{p}/" for p in patterns if not re.search(p, output)]
    total = len(facts) + len(patterns)
    return {"passed": total - len(missing), "total": total, "missing": missing}


def ccr_entries(output: str) -> list[dict]:
    """Originals behind each CCR marker, read from the store backend.

    `CompressionStore.retrieve()` records access and feeds TOIN, which would
    change later compressions in this process, so the backend is read directly.
    """
    keys = list(dict.fromkeys(CCR_HASH_RE.findall(output)))
    if not keys:
        return []
    from headroom.cache.compression_store import get_compression_store

    backend = get_compression_store()._backend
    entries = []
    for key in keys:
        entry = backend.get(key)
        entries.append({"key": key, "original": None if entry is None else entry.original_content})
    return entries


def percentile(values: list[float], quantile: float) -> float:
    ordered = sorted(values)
    index = (len(ordered) - 1) * quantile
    lo = int(index)
    hi = min(lo + 1, len(ordered) - 1)
    return ordered[lo] + (ordered[hi] - ordered[lo]) * (index - lo)


def disposition(output: str | None, text: str, before: int, after: int | None, error_kind: str | None) -> str:
    if error_kind == "no-entry":
        return "no-entry"
    if error_kind == "unavailable":
        return "unavailable"
    if error_kind is not None:
        return "candidate-rejected"
    if output == text:
        return "passthrough"
    if after is not None and after > before:
        return "expanded"
    return "compressed"


def retention_form(output: str | None, text: str, strategy, ccr: list[dict], domain: str) -> str:
    if output is None or output == text:
        return "inline"
    if any(entry["original"] for entry in ccr):
        return "offloaded-with-marker"
    if domain in ("search", "tabular", "paths") or (isinstance(strategy, str) and "lossless" in strategy):
        return "inline"
    return "unrecoverable-deletion"


def measure(entry: dict, samples: int, encoder) -> dict:
    case = entry["case"]
    text = case["input"]
    before = len(encoder.encode_ordinary(text))
    if entry.get("tokens_before") is not None and before != entry["tokens_before"]:
        raise AssertionError(f"{case['id']}: paired input tokenizer mismatch")
    original_facts = None if entry.get("error_kind") == "invalid-evidence" else check_facts(case, text)
    base = {
        "sample_id": case["id"], "domain": case["domain"], "mode": case["mode"],
        "origin": case.get("origin"), "input_sha256": hashlib.sha256(text.encode()).hexdigest(),
        "input": text, "tokens_before": before, "original_facts": original_facts,
        "latency_basis": "resident Python single-component wire API; initialization/tokenization excluded",
        "runtime_acceptance": "not measured", "llm_semantic": "not measured",
        "toin": "isolated empty TOIN path per process",
    }
    if entry.get("error_kind") == "invalid-evidence":
        return {**base, "entry": None, "output": None, "error_kind": "invalid-evidence",
                "error": "source fact contract is not valid for both component sides",
                "disposition": "invalid-evidence", "tokens_after": None,
                "compression_rate": None, "facts": None, "original_facts": None,
                "retention_form": "inline", "ccr": [], "latency_ms": None}
    try:
        component = build_component(case["domain"], case["mode"])
    except Exception as exc:
        component = None
        unavailable = f"component unavailable: {type(exc).__name__}: {exc}"
    else:
        unavailable = None
    if component is None:
        error = unavailable or f"no isolated component for domain {case['domain']}"
        kind = "unavailable" if unavailable else "no-entry"
        return {**base, "entry": None, "output": None, "error": error, "error_kind": kind, "tokens_after": None,
                "compression_rate": None, "disposition": disposition(None, text, before, None, kind),
                "retention_form": "inline", "facts": None, "ccr": [], "latency_ms": None}
    try:
        component(text)  # warm-up outside the timed samples
        durations, hashes = [], set()
        output = strategy = None
        for _ in range(samples):
            started = time.perf_counter()
            output, strategy = component(text)
            durations.append((time.perf_counter() - started) * 1000)
            hashes.add(hashlib.sha256(output.encode()).hexdigest())
    except Exception as exc:  # noqa: BLE001 - a failing component is a candidate rejection
        error = f"{type(exc).__name__}: {exc}"
        return {**base, "entry": component.name, "output": None, "error": error, "tokens_after": None,
                "compression_rate": None, "error_kind": "rejected", "disposition": "candidate-rejected", "retention_form": "inline",
                "facts": None, "ccr": [], "latency_ms": None}
    after = len(encoder.encode_ordinary(output))
    warnings = []
    try:
        ccr = ccr_entries(output)
    except Exception as exc:
        ccr = []
        warnings.append(f"recovery evidence unavailable: {type(exc).__name__}")
    return {
        **base, "entry": component.name, "strategy": strategy, "output": output, "error": None, "error_kind": None, "warnings": warnings,
        "output_sha256": hashlib.sha256(output.encode()).hexdigest(),
        "tokens_after": after, "compression_rate": 1 - after / before if before else None,
        "disposition": disposition(output, text, before, after, None),
        "retention_form": "recovery-evidence-unavailable" if warnings else retention_form(output, text, strategy, ccr, case["domain"]),
        "facts": check_facts(case, output), "ccr": ccr, "ccr_entries": len(ccr),
        "latency_ms": {"p50": statistics.median(durations), "p95": percentile(durations, .95),
                       "p99": percentile(durations, .99), "mean": statistics.fmean(durations),
                       "stdev": statistics.stdev(durations) if len(durations) > 1 else 0.0},
        "timing_samples": durations, "unique_output_hashes": len(hashes),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cases", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=100)
    args = parser.parse_args()
    if args.samples < 10:
        parser.error("at least 10 timing samples required")
    import tiktoken

    encoder = tiktoken.get_encoding("o200k_base")
    doc = json.loads(args.cases.read_text())
    rows = [measure(entry, args.samples, encoder) for entry in doc["rows"]]
    for row in rows:
        if row["original_facts"] is None:
            continue
        if row["original_facts"]["passed"] != row["original_facts"]["total"]:
            raise AssertionError(f"{row['sample_id']}: original fails its own facts {row['original_facts']}")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({
        "schema_version": 2, "set": doc.get("set", "synthetic"), "tokenizer": "o200k_base", "rows": rows,
    }, ensure_ascii=False, indent=2) + "\n")
    print(f"Headroom isolated component rows completed: {len(rows)}")


if __name__ == "__main__":
    main()
