"""Contract tests for the SQLite entry-point checker.

check-sqlite-entrypoints.py is the guard that keeps production code from
opening SQLite outside the approved lifecycle and DatabaseManager entry
points. Its subtle part is production_lines(): a brace-depth state
machine that excludes #[cfg(test)] mod blocks from the scan without
excluding anything else — a regression there either hides real violations
or flags test fixtures as production.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / "check-sqlite-entrypoints.py"

spec = importlib.util.spec_from_file_location("check_sqlite_entrypoints", SCRIPT)
checker = importlib.util.module_from_spec(spec)
sys.modules["check_sqlite_entrypoints"] = checker
spec.loader.exec_module(checker)


def production_text(text: str) -> list[tuple[int, str]]:
    return list(checker.production_lines(text))


def make_tree(tmp_path: Path, files: dict[str, str]) -> Path:
    for rel, text in files.items():
        target = tmp_path / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")
    return tmp_path


# ─── is_test_file ─────────────────────────────────────────────────────────────


def test_test_files_are_recognized_by_every_convention() -> None:
    for rel in (
        "src/tests/helper.rs",
        "tests/integration.rs",
        "src/storage/sqlite/tests.rs",
        "src/storage/sqlite/token_tests.rs",
    ):
        assert checker.is_test_file(Path(rel)), rel
    assert not checker.is_test_file(Path("src/storage/sqlite/token.rs"))


# ─── production_lines: the cfg(test) state machine ────────────────────────────


def test_a_cfg_test_mod_block_is_excluded() -> None:
    text = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() { Connection::open(x); }\n}\nfn b() {}\n"
    lines = production_text(text)
    numbers = [n for n, _ in lines]
    # the attribute line and everything after the block stay production;
    # the mod declaration line and the block body are excluded
    assert numbers == [1, 2, 6]


def test_code_after_the_test_block_is_still_production() -> None:
    text = "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\nfn after() { evil(); }\n"
    lines = production_text(text)
    assert any("evil" in line for _, line in lines)


def test_nested_braces_inside_the_test_block_do_not_confuse_the_depth() -> None:
    text = (
        "#[cfg(test)]\nmod tests {\n"
        "    fn t() { if x { if y { } } }\n"
        "    fn u() { let v = vec![{ 1 }, { 2 }]; }\n"
        "}\nfn after() { good(); }\n"
    )
    lines = production_text(text)
    # line 1 (attribute) and line 6 (after the block) stay production
    assert [n for n, _ in lines] == [1, 6]


def test_a_cfg_test_annotation_without_a_mod_block_hides_nothing() -> None:
    # #[cfg(test)] on a plain fn (unusual but legal) must not exclude it
    text = "#[cfg(test)]\nfn marked() { direct(); }\n"
    lines = production_text(text)
    assert any("direct" in line for _, line in lines)


# ─── scan: the three violation classes and their exemptions ──────────────────


def test_direct_opens_are_caught_with_and_without_the_rusqlite_prefix(tmp_path) -> None:
    root = make_tree(tmp_path, {
        "src/plain.rs": 'fn a() { let _ = Connection::open("a.db"); }\n',
        "src/prefixed.rs": 'fn a() { let _ = rusqlite::Connection::open("b.db"); }\n',
        "src/flags.rs": 'fn a() { let _ = Connection::open_with_flags("c.db", f); }\n',
    })
    violations = checker.scan(root)
    reasons = {(str(p), r) for p, _, r in violations}
    assert ("src\\plain.rs", "direct SQLite open") in reasons or ("src/plain.rs", "direct SQLite open") in reasons
    assert any("prefixed.rs" in str(p) and r == "direct SQLite open" for p, _, r in violations)
    assert any("flags.rs" in str(p) and r == "direct SQLite open" for p, _, r in violations)


def test_the_allowed_lifecycle_openers_are_exempt(tmp_path) -> None:
    root = make_tree(tmp_path, {
        "src/private_sqlite.rs": 'fn a() { let _ = Connection::open("ok.db"); }\n',
    })
    assert checker.scan(root) == []


def test_typed_stores_are_only_for_the_approved_entrypoints(tmp_path) -> None:
    files = {
        "src/unified.rs": "fn a() { GenAISqliteStore::new(p); }\n",  # allowed entrypoint
        "src/rogue.rs": "fn a() { GenAISqliteStore::new(p); }\n",   # not allowed
    }
    root = make_tree(tmp_path, files)
    violations = checker.scan(root)
    assert len(violations) == 1
    path, line, reason = violations[0]
    assert "rogue.rs" in str(path)
    assert line == 1
    assert reason == "typed Store opened outside DatabaseManager"


def test_every_store_entrypoint_shape_is_recognized(tmp_path) -> None:
    shapes = [
        "GenAISqliteStore::new(p)",
        "GenAISqliteStore::new_with_path(p)",
        "GenAISqliteStore::new_with_path_and_batch(p, b)",
        "InterruptionStore::new_with_path(p)",
        "TrajectoryStore::new_with_path(p)",
        "OptimizationStore::new_with_path(p)",
        "EvaluationStore::new_with_path(p)",
        "ReuseStore::open_private(p)",
        "CausalCaseStore::open_private(p)",
        "EnforcementStore::open_private(p)",
        "security::open_private_store(p)",
    ]
    files = {
        f"src/shape{i}.rs": f"fn a() {{ let _ = {call}; }}\n"
        for i, call in enumerate(shapes)
    }
    root = make_tree(tmp_path, files)
    violations = checker.scan(root)
    assert len(violations) == len(shapes)
    assert all(r == "typed Store opened outside DatabaseManager" for _, _, r in violations)


def test_a_vacuum_call_is_a_violation_even_in_an_allowed_opener_file(tmp_path) -> None:
    root = make_tree(tmp_path, {
        "src/private_sqlite.rs": "fn a() { conn.vacuum(); }\n",
    })
    violations = checker.scan(root)
    assert violations == [
        (Path("src/private_sqlite.rs"), 1, "automatic VACUUM call"),
    ]


def test_opens_inside_test_blocks_of_production_files_are_ignored(tmp_path) -> None:
    root = make_tree(tmp_path, {
        "src/prod.rs": (
            "#[cfg(test)]\nmod tests {\n"
            "    fn t() { let _ = Connection::open(\"fixture.db\"); }\n"
            "    fn u() { let _ = GenAISqliteStore::new(p); }\n"
            "}\n"
        ),
    })
    assert checker.scan(root) == []


def test_test_files_are_skipped_entirely(tmp_path) -> None:
    root = make_tree(tmp_path, {
        "src/storage/sqlite/token_tests.rs": 'fn a() { let _ = Connection::open("x.db"); }\n',
        "src/tests/fixture.rs": 'fn a() { let _ = Connection::open("x.db"); }\n',
    })
    assert checker.scan(root) == []


def test_the_built_in_self_test_scenario_still_holds() -> None:
    checker.self_test()  # raises AssertionError on drift


# ─── main ─────────────────────────────────────────────────────────────────────


def test_main_self_test_flag_reports_success(capsys, monkeypatch) -> None:
    monkeypatch.setattr(sys, "argv", ["check-sqlite-entrypoints.py", "--self-test"])
    code = checker.main()
    out = capsys.readouterr().out
    assert code == 0
    assert "self-test passed" in out


def test_main_reports_violations_and_fails(tmp_path, capsys, monkeypatch) -> None:
    root = make_tree(tmp_path, {"src/rogue.rs": 'fn a() { let _ = Connection::open("x.db"); }\n'})
    monkeypatch.setattr(sys, "argv", ["check-sqlite-entrypoints.py"])
    monkeypatch.setattr(
        checker, "scan", lambda r: [(root / "src/rogue.rs", 1, "direct SQLite open")]
    )
    code = checker.main()
    out = capsys.readouterr().out
    assert code == 1
    assert "SQLite entry-point violations:" in out
    assert "rogue.rs:1: direct SQLite open" in out


def test_main_passes_when_the_tree_is_clean(tmp_path, capsys, monkeypatch) -> None:
    monkeypatch.setattr(sys, "argv", ["check-sqlite-entrypoints.py"])
    monkeypatch.setattr(checker, "scan", lambda r: [])
    code = checker.main()
    out = capsys.readouterr().out
    assert code == 0
    assert "SQLite entry-point check passed" in out
