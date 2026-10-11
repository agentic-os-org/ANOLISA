"""Unit tests for cosh-extension/hooks/sandbox-failure-handler.py."""

from itertools import product
from pathlib import Path

from standalone_hook_test_loader import load_standalone_hook

_COSH_EXTENSION_DIR = Path(__file__).resolve().parents[2] / ".." / "cosh-extension"
_HOOKS_DIR = _COSH_EXTENSION_DIR / "hooks"

sandbox_guard = load_standalone_hook(
    "cosh_sandbox_guard_hook",
    _HOOKS_DIR / "sandbox-guard.py",
)
sandbox_failure_handler = load_standalone_hook(
    "cosh_sandbox_failure_handler_hook",
    _HOOKS_DIR / "sandbox-failure-handler.py",
)


def _roundtrip(command: str, restore_command: str = "") -> str | None:
    sandboxed = sandbox_guard.build_sandbox_command(
        command, "/tmp", "restricted", restore_command=restore_command
    )
    return sandbox_failure_handler.extract_original_command(sandboxed)


class TestExtractOriginalCommandRoundtrip:
    def test_plain_command(self):
        assert _roundtrip("rm -rf build") == "rm -rf build"

    def test_quoted_path(self):
        assert _roundtrip("rm -rf '/tmp/my dir'") == "rm -rf '/tmp/my dir'"

    def test_trailing_backslash(self):
        # 引号内反斜杠是字面字符，不能与收尾引号组成转义对吞掉引号
        assert _roundtrip("rm -rf a\\") == "rm -rf a\\"

    def test_backslash_before_quote(self):
        # 反斜杠后的引号属于 '\'' 边界，不能被当作 \<char> 转义对消费
        assert _roundtrip("echo a\\'b") == "echo a\\'b"

    def test_printf_style_escape(self):
        command = "printf '%s\\n' x"
        assert _roundtrip(command) == command

    def test_nested_bash_c_payload(self):
        command = "bash -c 'true'"
        assert _roundtrip(command) == command

    def test_sudo_restore_command_uses_cosh_rc(self):
        # COSH_RC 优先路径：完整还原命令（含 sudo 前缀）经 base64 嵌入
        assert (
            _roundtrip("rm -rf build", restore_command="sudo rm -rf build")
            == "sudo rm -rf build"
        )

    def test_sudo_restore_command_with_quotes(self):
        restore = "sudo rm -rf '/tmp/my dir'"
        assert _roundtrip("rm -rf '/tmp/my dir'", restore_command=restore) == restore

    def test_non_sandbox_command_returns_none(self):
        assert sandbox_failure_handler.extract_original_command("ls -la") is None

    def test_exhaustive_quote_backslash_alphabet(self):
        # 对引号/反斜杠字母表穷举：bypass 还原必须逐字符等于原始命令
        alphabet = ["a", "'", "\\", " "]
        for n in range(1, 5):
            for combo in product(alphabet, repeat=n):
                command = "rm -rf " + "".join(combo)
                extracted = _roundtrip(command)
                assert extracted == command, (command, extracted)
