"""code_scan backend — delegates to the code_scanner package."""

from typing import Any

from agent_sec_cli.code_scanner.errors import ErrUnsupportedLang
from agent_sec_cli.code_scanner.models import Language, Verdict
from agent_sec_cli.code_scanner.scanner import scan
from agent_sec_cli.security_middleware.backends.base import BaseBackend
from agent_sec_cli.security_middleware.context import RequestContext
from agent_sec_cli.security_middleware.result import ActionResult


class CodeScanBackend(BaseBackend):
    """Scan code snippets for security issues using the code_scanner engine.

    Supports regex (default) and LLM modes, selected via the `mode` kwarg.
    """

    def execute(self, ctx: RequestContext, **kwargs: Any) -> ActionResult:
        code = kwargs.get("code", "")
        language_str = kwargs.get("language", "bash")
        mode = kwargs.get("mode", "regex")
        try:
            language = Language(language_str)
        except ValueError:
            err = ErrUnsupportedLang(language_str)
            return ActionResult(
                success=False,
                error=f"scan error: {err.message}",
                exit_code=1,
                error_type=type(err).__name__,
            )
        result = scan(code, language, mode=mode)
        return ActionResult(
            success=result.ok,
            data=result.model_dump(),
            stdout=result.model_dump_json(indent=2),
            exit_code=0 if result.verdict != Verdict.ERROR else 1,
            error_type="CodeScanError" if result.verdict == Verdict.ERROR else "",
        )
