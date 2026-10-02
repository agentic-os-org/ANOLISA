"""Unit tests for the Cosh Skill Ledger hook path defaults."""

from pathlib import Path

from standalone_hook_test_loader import load_standalone_hook

_ROOT = Path(__file__).resolve().parents[2]
_HOOK_PATH = _ROOT / "cosh-extension" / "hooks" / "skill_ledger_hook.py"

skill_ledger_hook = load_standalone_hook("cosh_skill_ledger_hook", _HOOK_PATH)


def test_supported_skill_bases_include_rpm_and_raw_system_roots() -> None:
    bases = skill_ledger_hook._supported_skill_bases("/workspace")

    assert Path("/usr/share/anolisa/skills") in bases
    assert Path("/usr/local/share/anolisa/skills") in bases
