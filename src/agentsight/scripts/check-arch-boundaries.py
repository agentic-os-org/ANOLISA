#!/usr/bin/env python3
"""Architecture boundary checker for AgentSight.

Validates that `use crate::` imports respect the L0-L8 layer constraints
defined in docs/ARCHITECTURE.md.

Exit codes:
  0 — no new violations (known violations in allowlist are OK)
  1 — new violations found
"""

import re
import sys
from pathlib import Path

# Layer numbers (informational, used for diagnostic output).
LAYER_MAP = {
    "bpf":         0,   # L0: Kernel
    "probes":      1,   # L1: Capture
    "event":       1,   # L1: Capture
    "parser":      2,   # L2: Parse
    "aggregator":  3,   # L3: Aggregate
    "analyzer":    4,   # L4: Analyze
    "tokenizer":   4,   # L4: Analyze
    "genai":       5,   # L5: Semantic
    "atif":        5,   # L5: Semantic
    "grounding":   5,   # L5: Semantic (deterministic ATIF analysis, no crate deps)
    "storage":     6,   # L6: Persist
    "storage_status": 6,  # L6: Cross-platform read-only storage status
    "agent_sec":   7,   # L7: Serve
    "grader":      7,   # L7: Serve
    "enforcement": 7,   # L7: Security control plane
    "security":    7,   # L7: Security audit and containment
    "server":      7,   # L7: Serve
    "health":      7,   # L7: Serve
    "preferences": 7,   # L7: Serve (on-demand analysis shared by both servers)
    "reuse":       7,   # L7: Serve (trajectory reuse labels shared by both servers)
    "semantic_search": 7,  # L7: Serve (session search contract shared by both servers)
    "bin":         8,   # L8: Entry
    "unified":     8,   # L8: Entry
    "config":      8,   # L8: Entry
    "ffi":         8,   # L8: Entry
}

# Allowed dependency edges. A set lists allowed targets; "*" allows any module.
ALLOWED_DEPS = {
    "event":       {"probes"},
    "probes":      {"event"},
    "parser":      {"probes", "event"},
    "aggregator":  {"parser", "probes", "event"},
    "analyzer":    {"aggregator", "tokenizer", "parser"},
    "tokenizer":   set(),
    "genai":       {"analyzer", "aggregator", "parser"},
    "atif":        {"genai", "storage"},
    # Reads only the `agentsight_atif` crate, never `crate::` — which is what lets
    # more than one subsystem share it.
    "grounding":   set(),
    "storage":     {"analyzer", "genai", "security"},
    "storage_status": set(),
    "grader":      {"storage"},
    "enforcement": {"storage"},
    "security":    {"enforcement"},
    "server":      {
        "storage",
        "storage_status",
        "health",
        "atif",
        "agent_sec",
        "grader",
        "enforcement",
        "security",
        "grounding",
        "preferences",
        "reuse",
        "semantic_search",
    },
    # Same-layer peers of `server`: both are transport-agnostic contracts the
    # Linux and macOS handlers share, so they may only read downwards.
    "preferences": {"genai", "storage", "atif"},
    # May read the grounding engine, and nothing else — in particular not
    # `server`. The labelling rules themselves take grounding's conclusions as an
    # argument; only the adapter that folds them reaches for the engine. Keeping
    # the set this narrow is what stops an import of `server` from being quietly
    # reintroduced.
    "reuse":       {"grounding"},
    "semantic_search": {"storage"},
    "agent_sec":   set(),
    "health":      {"storage"},
    "unified":     "*",
    "config":      set(),
    "ffi":         "*",
    "bin":         "*",
}

# Cross-cutting modules — any module may import these.
CROSS_CUTTING = {
    "config",          # global configuration
    "chrome_trace",    # data format helpers
    "interruption",    # cross-layer event detection
    "response_map",    # session mapping helpers
    "logging",         # logging init
    "utils",           # utility functions
    "discovery",       # process discovery (Cross in ARCHITECTURE.md)
    "skill_metrics",   # metric helpers
    "token_breakdown", # token analysis helpers
    "ecs_metadata",    # shared ECS metadata client primitives
    "private_sqlite",  # private state-file creation shared by control modules
}

# Known violations: (source_file_relative_to_src, target_module, reason)
KNOWN_VIOLATIONS = [
    ("genai/builder.rs", "storage",
     "L5->L6: PendingCallInfo/SseEnrichment types pending migration (#906)"),
]

USE_RE = re.compile(r"use\s+crate::(\w+)")
PATH_RE = re.compile(r"crate::(\w+)::")
CFG_TEST_RE = re.compile(r"#\[cfg\(test\)\]")
MOD_RE = re.compile(r"\bmod\s+\w+")


def source_module_for(rel_path: Path) -> str:
    """Derive the module name owning a source file.

    Examples:
        genai/builder.rs        -> genai
        unified.rs              -> unified
        bin/cli/token.rs        -> bin
    """
    parts = rel_path.parts
    if len(parts) >= 2:
        return parts[0]
    # top-level file like unified.rs / config.rs / lib.rs
    return rel_path.stem


def is_test_file(rel_path: Path) -> bool:
    s = str(rel_path).replace("\\", "/")
    if "/tests/" in s:
        return True
    if rel_path.name.endswith("_tests.rs"):
        return True
    return False


def extract_imports(text: str):
    """Yield (line_no, target_module) tuples for crate-internal imports.

    Skips imports inside `#[cfg(test)] mod ... { ... }` blocks using a simple
    brace-depth tracker.

    Limitations:
      - Only recognises `#[cfg(test)]` followed by `mod <name> {` with the
        opening brace on the same line as the `mod` keyword.
      - Does not handle `use super::super::` cross-module references; code
        convention must enforce `use crate::` for cross-module imports.
    """
    lines = text.splitlines()
    in_test_block = False
    test_block_depth = 0  # brace depth at which the test block was opened
    depth = 0
    pending_cfg_test = False

    for idx, line in enumerate(lines, start=1):
        # Skip comment lines (line comments and doc comments).
        stripped = line.lstrip()
        if stripped.startswith("//"):
            continue

        # Detect cfg(test) attribute on its own line — applies to the next item.
        if CFG_TEST_RE.search(line):
            pending_cfg_test = True
        elif pending_cfg_test and not MOD_RE.search(line) and stripped:
            # Reset pending if the next non-empty line is not a `mod` declaration.
            pending_cfg_test = False

        # If we have a pending cfg(test), look for `mod ... {` to enter a block.
        if pending_cfg_test and not in_test_block and MOD_RE.search(line):
            if "{" in line:
                in_test_block = True
                test_block_depth = depth  # opening brace counted below
                pending_cfg_test = False

        # Update brace depth from this line's braces.
        opens = line.count("{")
        closes = line.count("}")
        new_depth = depth + opens - closes

        # Process imports if not inside a test block.
        if not in_test_block:
            for m in USE_RE.finditer(line):
                yield idx, m.group(1)
            for m in PATH_RE.finditer(line):
                yield idx, m.group(1)

        depth = new_depth

        # Exit test block once depth falls back to its opening level.
        if in_test_block and depth <= test_block_depth:
            in_test_block = False
            test_block_depth = 0


def known_violation_match(rel_path: Path, target: str):
    rel_str = str(rel_path).replace("\\", "/")
    for src_file, tgt, reason in KNOWN_VIOLATIONS:
        if src_file == rel_str and tgt == target:
            return reason
    return None


def classify(source: str, target: str, rel_path: Path):
    """Return ("pass" | "known" | "violation", detail)."""
    if target in CROSS_CUTTING:
        return "pass", None
    if target == source:
        return "pass", None
    allowed = ALLOWED_DEPS.get(source)
    if allowed == "*":
        return "pass", None
    if allowed is None:
        # Source has no rule defined (e.g., cross-cutting helper module).
        # Treat as unconstrained — only modules listed in ALLOWED_DEPS are
        # subject to layer constraints.
        return "pass", None
    if target in allowed:
        return "pass", None
    reason = known_violation_match(rel_path, target)
    if reason is not None:
        return "known", reason
    return "violation", None


def fmt_layer(module: str) -> str:
    layer = LAYER_MAP.get(module)
    return f"L{layer}" if layer is not None else "L?"


def fmt_allowed(source: str) -> str:
    allowed = ALLOWED_DEPS.get(source)
    if allowed == "*":
        return "any"
    if allowed is None:
        return "(unknown module — no rule defined)"
    if not allowed:
        return "(none)"
    return "{" + ", ".join(sorted(allowed)) + "}"


def main() -> int:
    script_dir = Path(__file__).resolve().parent
    src_dir = (script_dir / ".." / "src").resolve()

    print("=== AgentSight Architecture Boundary Check ===\n")

    if not src_dir.is_dir():
        print(f"ERROR: src/ not found at {src_dir}", file=sys.stderr)
        return 1

    rs_files = sorted(p for p in src_dir.rglob("*.rs"))
    rs_files = [p for p in rs_files if not is_test_file(p.relative_to(src_dir))]

    print(f"Scanning: src/ ({len(rs_files)} files)\n")

    violations = []
    knowns = []

    for path in rs_files:
        rel = path.relative_to(src_dir)
        source = source_module_for(rel)
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError as exc:
            print(f"WARN: cannot read {rel}: {exc}", file=sys.stderr)
            continue

        seen = set()
        for line_no, target in extract_imports(text):
            key = (line_no, target)
            if key in seen:
                continue
            seen.add(key)
            verdict, detail = classify(source, target, rel)
            if verdict == "pass":
                continue
            entry = (rel, line_no, source, target, detail)
            if verdict == "known":
                knowns.append(entry)
            else:
                violations.append(entry)

    for rel, line_no, source, target, _ in violations:
        print(f"[VIOLATION] {rel}:{line_no}")
        print(f"  {source} ({fmt_layer(source)}) -> {target} ({fmt_layer(target)})")
        print(f"  Allowed imports for {source}: {fmt_allowed(source)}")
        print()

    for rel, line_no, source, target, detail in knowns:
        print(f"[KNOWN] {rel}:{line_no}")
        print(f"  {source} ({fmt_layer(source)}) -> {target} ({fmt_layer(target)})")
        print(f"  Tracked: {detail}")
        print()

    print("---")
    print(
        f"Summary: {len(violations)} violations, "
        f"{len(knowns)} known (allowlisted), "
        f"{len(rs_files)} files checked"
    )
    if violations:
        print("Result: FAILED (new violations found)")
        return 1
    print("Result: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
