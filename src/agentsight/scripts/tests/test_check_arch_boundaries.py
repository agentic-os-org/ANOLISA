"""Contract tests for the architecture boundary checker.

check-arch-boundaries.py enforces the L0-L8 layer constraints of
docs/ARCHITECTURE.md on `use crate::` imports. A drift in classify() or in
the cfg(test)-skipping state machine either hides real boundary violations
or blocks legitimate imports — both fail the check's one job.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / "check-arch-boundaries.py"

spec = importlib.util.spec_from_file_location("check_arch_boundaries", SCRIPT)
checker = importlib.util.module_from_spec(spec)
sys.modules["check_arch_boundaries"] = checker
spec.loader.exec_module(checker)


def imports_of(text: str) -> list[tuple[int, str]]:
    return list(checker.extract_imports(text))


# ─── module ownership and test-file detection ─────────────────────────────────


def test_source_module_ownership_rules() -> None:
    assert checker.source_module_for(Path("genai/builder.rs")) == "genai"
    assert checker.source_module_for(Path("unified.rs")) == "unified"
    assert checker.source_module_for(Path("bin/cli/token.rs")) == "bin"


def test_test_files_are_recognized() -> None:
    assert checker.is_test_file(Path("parser/tests/helper.rs"))
    assert checker.is_test_file(Path("storage/sqlite/token_tests.rs"))
    assert not checker.is_test_file(Path("storage/sqlite/token.rs"))
    # windows separators normalize too
    assert checker.is_test_file(Path("parser\\tests\\helper.rs"))


# ─── extract_imports ──────────────────────────────────────────────────────────


def test_both_import_shapes_are_captured() -> None:
    text = (
        "use crate::parser::lines;\n"
        "fn f() { let x = crate::storage::entry(); }\n"
    )
    # a use-line matches BOTH regexes (use crate::X and crate::X::), so the
    # same (line, module) pair is yielded twice; main() dedups on the pair
    assert imports_of(text) == [(1, "parser"), (1, "parser"), (2, "storage")]


def test_comment_lines_are_never_scanned() -> None:
    text = "// use crate::bypassed;\n/// doc: crate::also_bypassed::path\nfn f() {}\n"
    assert imports_of(text) == []


def test_imports_inside_a_cfg_test_mod_block_are_skipped() -> None:
    text = (
        "#[cfg(test)]\n"
        "mod tests {\n"
        "    use crate::anything::goes;\n"
        "    fn t() { let _ = crate::also::fine(); }\n"
        "}\n"
        "fn prod() { crate::counted::here(); }\n"
    )
    assert imports_of(text) == [(6, "counted")]


def test_a_cfg_test_annotation_without_a_mod_block_hides_nothing() -> None:
    # a plain fn (or anything else) after #[cfg(test)] is still scanned
    text = "#[cfg(test)]\nfn marked() { crate::still::counted(); }\n"
    assert imports_of(text) == [(2, "still")]


# ─── classify: the layer matrix ───────────────────────────────────────────────


def test_cross_cutting_targets_are_always_allowed() -> None:
    verdict, detail = checker.classify("parser", "utils", Path("parser/mod.rs"))
    assert verdict == "pass"


def test_same_module_imports_pass() -> None:
    assert checker.classify("parser", "parser", Path("parser/mod.rs"))[0] == "pass"


def test_wildcard_modules_import_anything() -> None:
    for source in ("unified", "bin", "ffi"):
        verdict, _ = checker.classify(source, "storage", Path(f"{source}.rs"))
        assert verdict == "pass", source


def test_modules_without_a_rule_are_unconstrained() -> None:
    # discovery is cross-cutting: not in ALLOWED_DEPS as a source
    verdict, _ = checker.classify("discovery", "storage", Path("discovery/mod.rs"))
    assert verdict == "pass"


def test_allowed_edges_pass_and_disallowed_edges_violate() -> None:
    # atif -> genai is allowed
    assert checker.classify("atif", "genai", Path("atif/mod.rs"))[0] == "pass"
    # atif -> probes is not
    verdict, detail = checker.classify("atif", "probes", Path("atif/mod.rs"))
    assert verdict == "violation"
    assert detail is None


def test_the_allowlist_matches_on_file_and_target() -> None:
    # the one known violation: genai/builder.rs importing storage
    verdict, detail = checker.classify(
        "genai", "storage", Path("genai/builder.rs")
    )
    assert verdict == "known"
    assert "906" in detail

    # the same edge from a DIFFERENT genai file is a fresh violation
    verdict, _ = checker.classify("genai", "storage", Path("genai/other.rs"))
    assert verdict == "violation"

    # and builder.rs importing a different disallowed module is fresh too
    verdict, _ = checker.classify("genai", "probes", Path("genai/builder.rs"))
    assert verdict == "violation"


# ─── diagnostics formatting ───────────────────────────────────────────────────


def test_layer_and_allowed_formatting() -> None:
    assert checker.fmt_layer("bpf") == "L0"
    assert checker.fmt_layer("server") == "L7"
    assert checker.fmt_layer("unlisted") == "L?"

    assert checker.fmt_allowed("unified") == "any"
    assert checker.fmt_allowed("not_a_module") == "(unknown module — no rule defined)"
    assert checker.fmt_allowed("tokenizer") == "(none)"
    assert checker.fmt_allowed("event") == "{probes}"


# ─── main: end to end against a synthetic tree ────────────────────────────────


def build_tree(tmp_path: Path) -> Path:
    src = tmp_path / "src"
    (src / "atif").mkdir(parents=True)
    (src / "genai").mkdir()
    (src / "parser").mkdir()
    (src / "parser" / "tests").mkdir()
    (src / "parser" / "tests" / "helper.rs").write_text(
        "use crate::server::whatever;\n", encoding="utf-8"
    )
    # allowed edge
    (src / "atif" / "converter.rs").write_text(
        "use crate::genai::model;\n", encoding="utf-8"
    )
    # the allowlisted violation
    (src / "genai" / "builder.rs").write_text(
        "use crate::storage::PendingCallInfo;\n", encoding="utf-8"
    )
    # cross-cutting import from a constrained module
    (src / "parser" / "mod.rs").write_text(
        "use crate::utils::helper;\n", encoding="utf-8"
    )
    return tmp_path


def run_main(monkeypatch, tmp_path: Path, extra_violation: bool, capsys):
    root = build_tree(tmp_path)
    if extra_violation:
        (root / "src" / "atif" / "rogue.rs").write_text(
            "use crate::probes::raw;\n", encoding="utf-8"
        )
    scripts = root / "scripts"
    scripts.mkdir(exist_ok=True)
    monkeypatch.setattr(checker, "__file__", str(scripts / "check-arch-boundaries.py"))
    monkeypatch.setattr(sys, "argv", ["check-arch-boundaries.py"])
    code = checker.main()
    out = capsys.readouterr().out
    return code, out


def test_main_passes_with_only_the_allowlisted_violation(
    monkeypatch, tmp_path, capsys
) -> None:
    code, out = run_main(monkeypatch, tmp_path, extra_violation=False, capsys=capsys)
    assert code == 0
    assert "Result: PASS" in out
    assert "[KNOWN] genai" in out or "[KNOWN] genai\\builder.rs" in out or "genai" in out
    assert "1 violations" not in out
    assert "0 violations" in out
    assert "1 known" in out


def test_main_fails_on_a_new_violation(monkeypatch, tmp_path, capsys) -> None:
    code, out = run_main(monkeypatch, tmp_path, extra_violation=True, capsys=capsys)
    assert code == 1
    assert "Result: FAILED" in out
    assert "1 violations" in out
    assert "[VIOLATION]" in out
    assert "atif (L5) -> probes (L1)" in out


def test_main_reports_the_allowlist_reason(monkeypatch, tmp_path, capsys) -> None:
    code, out = run_main(monkeypatch, tmp_path, extra_violation=False, capsys=capsys)
    assert code == 0
    assert "906" in out
