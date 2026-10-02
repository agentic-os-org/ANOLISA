import re
from typing import Optional, Tuple

from agent_sec_cli.code_scanner.models import Language

# Interpreter -> Language mapping
_SHELL_INTERPRETERS = {"bash", "sh", "zsh"}
_PYTHON_INTERPRETERS = {"python", "python3"}

# Regex: match `[uv run [options]] <interpreter> -c '<code>'`
# Group 1 = interpreter name, Group 2 = quote char, Group 3 = code content
_INLINE_RE = re.compile(
    r"""(?:^|\s)"""
    r"""(?:uv\s+run\s+(?:--\w[\w-]*(?:\s+\S+)?\s+)*)?"""  # optional uv run prefix
    r"""(bash|sh|zsh|python3?)\s+"""  # interpreter
    r"""-c\s+"""  # -c flag
    r"""(["'])((?:\\.|(?!\2).)*)\2""",  # quoted code (escape-aware)
    re.DOTALL,
)


def extract_inline_code(command: str) -> Optional[Tuple[str, Language]]:
    """Extract inline code from a shell command string.

    Supports:
      Shell (returned as Language.BASH):
        'bash -c "rm -rf /"'
        'sh -c "curl ... | sh"'
        'zsh -c "rm -rf /"'

      Python (returned as Language.PYTHON):
        'python -c "import os; ..."'
        'python3 -c "print(1)"'
        'uv run python -c "os.system(...)"'
        'uv run --with pkg python3 -c "..."'

    Returns ``None`` when the command does not match any known pattern.
    """
    return _try_extract(command)


def _try_extract(command: str) -> Optional[Tuple[str, Language]]:
    """Attempt to extract inline code from *command*."""
    m = _INLINE_RE.search(command)
    if m is None:
        return None

    interpreter = m.group(1)
    code = m.group(3)

    if interpreter in _SHELL_INTERPRETERS:
        return (code, Language.BASH)
    if interpreter in _PYTHON_INTERPRETERS:
        return (code, Language.PYTHON)
    return None
