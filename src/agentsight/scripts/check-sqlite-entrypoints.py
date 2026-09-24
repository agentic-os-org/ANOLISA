#!/usr/bin/env python3
"""Reject production SQLite opens that bypass approved AgentSight entry points."""

import re
import sys
import tempfile
from pathlib import Path

OPEN_RE = re.compile(r"\b(?:rusqlite::)?Connection::open(?:_with_flags)?\s*\(")
STORE_OPEN_RE = re.compile(
    r"\b(?:GenAISqliteStore::new(?:_with_path(?:_and_batch)?)?"
    r"|InterruptionStore::new_with_path"
    r"|TrajectoryStore::new_with_path"
    r"|OptimizationStore::new_with_path"
    r"|EvaluationStore::new_with_path"
    r"|ReuseStore::open_private"
    r"|CausalCaseStore::open_private"
    r"|EnforcementStore::open_private"
    r"|security::open_private_store)\s*\("
)
VACUUM_CALL_RE = re.compile(r"\.vacuum\s*\(")
CFG_TEST_RE = re.compile(r"#\[cfg\(test\)\]")
MOD_RE = re.compile(r"\bmod\s+\w+")

ALLOWED_OPENERS = {
    "crates/agentsight-sqlite-lifecycle/src/connection.rs",
    "src/private_sqlite.rs",
    "src/storage/sqlite/tokenless.rs",
}

ALLOWED_STORE_ENTRYPOINTS = {
    "src/bin/cli/trace.rs",
    "src/local/server.rs",
    "src/local/trace.rs",
    "src/server/mod.rs",
    "src/unified.rs",
}


def is_test_file(relative: Path) -> bool:
    value = relative.as_posix()
    return (
        "/tests/" in value
        or value.startswith("tests/")
        or relative.name == "tests.rs"
        or relative.name.endswith("_tests.rs")
    )


def production_lines(text: str):
    in_test_block = False
    test_depth = 0
    depth = 0
    pending_test = False

    for line_number, line in enumerate(text.splitlines(), start=1):
        stripped = line.lstrip()
        if CFG_TEST_RE.search(line):
            pending_test = True
        elif pending_test and stripped and not stripped.startswith("//") and not MOD_RE.search(line):
            pending_test = False

        if pending_test and not in_test_block and MOD_RE.search(line) and "{" in line:
            in_test_block = True
            test_depth = depth
            pending_test = False

        next_depth = depth + line.count("{") - line.count("}")
        if not in_test_block:
            yield line_number, line
        depth = next_depth

        if in_test_block and depth <= test_depth:
            in_test_block = False
            test_depth = 0


def scan(root: Path):
    violations = []
    for path in sorted(root.glob("src/**/*.rs")) + sorted(root.glob("crates/*/src/**/*.rs")):
        relative = path.relative_to(root)
        if is_test_file(relative):
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for line_number, line in production_lines(text):
            if OPEN_RE.search(line) and relative.as_posix() not in ALLOWED_OPENERS:
                violations.append((relative, line_number, "direct SQLite open"))
            if (
                STORE_OPEN_RE.search(line)
                and relative.as_posix() not in ALLOWED_STORE_ENTRYPOINTS
            ):
                violations.append(
                    (relative, line_number, "typed Store opened outside DatabaseManager")
                )
            if VACUUM_CALL_RE.search(line):
                violations.append((relative, line_number, "automatic VACUUM call"))
    return violations


def self_test() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "src").mkdir()
        (root / "crates" / "sample" / "src").mkdir(parents=True)
        (root / "src" / "allowed.rs").write_text(
            "#[cfg(test)]\nmod tests {\n"
            "    fn opens() { let _ = Connection::open(\"fixture.db\"); }\n}\n",
            encoding="utf-8",
        )
        violation = root / "crates" / "sample" / "src" / "lib.rs"
        violation.write_text(
            "fn open() { let _ = rusqlite::Connection::open(\"unsafe.db\"); }\n"
            "fn store() { let _ = GenAISqliteStore::new_with_path(path, policy); }\n",
            encoding="utf-8",
        )
        assert scan(root) == [
            (violation.relative_to(root), 1, "direct SQLite open"),
            (
                violation.relative_to(root),
                2,
                "typed Store opened outside DatabaseManager",
            ),
        ]


def main() -> int:
    if "--self-test" in sys.argv:
        self_test()
        print("SQLite entry-point checker self-test passed")
        return 0

    root = Path(__file__).resolve().parent.parent
    violations = scan(root)
    if violations:
        print("SQLite entry-point violations:")
        for path, line_number, reason in violations:
            print(f"  {path}:{line_number}: {reason}")
        print("Use DatabaseManager or an explicitly allowed lifecycle/private/external entry point.")
        return 1
    print("SQLite entry-point check passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
