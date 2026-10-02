"""CodeScanError hierarchy for the code_scanner module.

Error code ranges (prefix ``1`` identifies the code_scanner module):
- 100: base / fallback
- 110-119: input layer
- 120-129: rule layer
- 130-139: engine layer
- 140-149: LLM engine layer
"""


class CodeScanError(Exception):
    """Base exception for code scanning errors."""

    code = 100
    message = "internal error"

    def __init__(self, message: str = "") -> None:
        self.message = message or self.__class__.message
        super().__init__(self.message)


# -- Input layer (110-119) --


class ErrInputEmpty(CodeScanError):
    code = 110
    message = "empty input code"


class ErrUnsupportedLang(CodeScanError):
    code = 111

    def __init__(self, lang: str = "") -> None:
        super().__init__(
            f"unsupported language: {lang}" if lang else "unsupported language"
        )


class ErrInputEncoding(CodeScanError):
    code = 112
    message = "input encoding error"


# -- Rule layer (120-129) --


class ErrRuleFileNotFound(CodeScanError):
    code = 120

    def __init__(self, path: str = "") -> None:
        super().__init__(
            f"rule file not found: {path}" if path else "rule file not found"
        )


class ErrRuleYamlParse(CodeScanError):
    code = 121

    def __init__(self, rule_name: str = "") -> None:
        super().__init__(
            f"rule file YAML parse error: {rule_name}"
            if rule_name
            else "rule file YAML parse error"
        )


class ErrRuleValidation(CodeScanError):
    code = 122

    def __init__(self, rule_name: str = "") -> None:
        super().__init__(
            f"rule validation failed: {rule_name}"
            if rule_name
            else "rule validation failed"
        )


class ErrRuleRefResolve(CodeScanError):
    code = 123

    def __init__(self, rule_name: str = "") -> None:
        super().__init__(
            f"rule reference resolve failed: {rule_name}"
            if rule_name
            else "rule reference resolve failed"
        )


class ErrRegexCompile(CodeScanError):
    code = 124

    def __init__(self, rule_name: str = "") -> None:
        super().__init__(
            f"regex compile failed: {rule_name}"
            if rule_name
            else "regex compile failed"
        )


# -- Engine layer (130-139) --


class ErrEngineTimeout(CodeScanError):
    code = 130
    message = "scan timeout"


class ErrEngineResource(CodeScanError):
    code = 131
    message = "engine resource exhausted"


# -- LLM engine layer (140-149) --


class ErrLlmUnavailable(CodeScanError):
    code = 140
    message = "LLM model not available"


class ErrLlmUnparsable(CodeScanError):
    code = 141
    message = "LLM response unparsable"
