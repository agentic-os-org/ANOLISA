"""Check explicit language preferences and their effect on CLI and cosh launches."""

import os
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from test_integration import load

import i18n


@pytest.fixture
def profile(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    monkeypatch.setenv("LANG", "zh_CN.UTF-8")
    monkeypatch.setenv("LC_ALL", "zh_CN.UTF-8")
    return tmp_path / "anolisa-omarchy"


def test_default_english_on_chinese_system(profile: Path) -> None:
    assert i18n.language() == "en-US"
    assert not profile.exists()
    assert i18n.tr("Installed") == "Installed"
    assert i18n.ui_state() == {"language": "en-US", "strings": {}}


def test_private_persistence_preserves_accounts(profile: Path) -> None:
    profile.mkdir(mode=0o700)
    account = profile / "qoder-auth.json"
    account.write_bytes(b"account-sentinel")
    account.chmod(0o600)
    i18n.set_language("zh-CN")
    assert i18n.language() == "zh-CN"
    assert i18n.tr("Installed") == "已安装"
    assert i18n.ui_state()["strings"]["Settings"] == "设置"
    assert i18n.preference_path().stat().st_mode & 0o777 == 0o600
    assert account.read_bytes() == b"account-sentinel"
    result = subprocess.check_output([sys.executable, ROOT / "runtime.py", "language"], text=True)
    assert "zh-CN" in result and "界面语言" in result
    i18n.set_language("en-US")
    assert i18n.tr("Installed") == "Installed"


def test_invalid_language_does_not_write(profile: Path) -> None:
    with pytest.raises(ValueError):
        i18n.set_language("unexpected")
    assert not profile.exists()


def test_language_preference_symlink_is_not_read(profile: Path, tmp_path: Path) -> None:
    profile.mkdir()
    outside = tmp_path / "outside.json"
    outside.write_text('{"language":"zh-CN"}')
    i18n.preference_path().symlink_to(outside)
    assert i18n.language() == "en-US"
    assert outside.read_text() == '{"language":"zh-CN"}'


def test_invalid_preferences_can_be_repaired(profile: Path) -> None:
    profile.mkdir()
    preferences = i18n.preference_path()
    preferences.write_bytes(b"invalid-json")
    preferences.chmod(0o600)
    assert i18n.language() == "en-US"
    subprocess.run([sys.executable, ROOT / "runtime.py", "language", "zh-CN"], check=True)
    assert i18n.language() == "zh-CN"


@pytest.mark.parametrize("selected", ["en-US", "zh-CN"])
def test_cosh_language_is_child_only(
    profile: Path, monkeypatch: pytest.MonkeyPatch, selected: str
) -> None:
    runtime = load("runtime")
    i18n.set_language(selected)
    before = dict(os.environ)
    calls = []
    monkeypatch.setattr(
        runtime.subprocess, "call", lambda args, *, env: calls.append((args, env)) or 0
    )
    paths = {"prefix": ROOT, "state": profile / "state"}
    assert runtime.run_cosh(paths, []) == 0
    assert calls[0][1]["COSH_SHELL_LANG"] == selected
    assert calls[0][1]["MISE_NOT_FOUND_AUTO_INSTALL"] == "0"
    assert dict(os.environ) == before


def test_catalog_preserves_format_arguments(profile: Path) -> None:
    import string

    formatter = string.Formatter()
    for messages in i18n.catalog().values():
        for english, chinese in messages.items():

            def arguments(text: str) -> set[str]:
                return {field for _, field, _, _ in formatter.parse(text) if field is not None}

            assert arguments(english) == arguments(chinese), english
    i18n.set_language("zh-CN")
    assert "1.2.3" in i18n.tr("Installing Qoder CLI {0}…", "1.2.3")


def test_english_help_and_chinese_help(profile: Path) -> None:
    english = subprocess.check_output([sys.executable, ROOT / "runtime.py", "help"], text=True)
    assert "Component help" in english and "组件帮助" not in english
    subprocess.run([sys.executable, ROOT / "runtime.py", "language", "zh-CN"], check=True)
    chinese = subprocess.check_output([sys.executable, ROOT / "runtime.py", "help"], text=True)
    assert "组件帮助" in chinese and "Component help" not in chinese
