"""Public scanner names and historical ledger identifiers."""

CODE_SCANNER_NAME = "code-scanner"
STATIC_SCANNER_NAME = "static-scanner"
SKILL_VETTER_NAME = "skill-vetter"

LEGACY_CODE_SCANNER_NAME = "skill-code-scanner"
LEGACY_STATIC_SCANNER_NAME = "cisco-static-scanner"

DEFAULT_BUILTIN_SCANNERS = [CODE_SCANNER_NAME, STATIC_SCANNER_NAME]

_ALIASES = {
    LEGACY_CODE_SCANNER_NAME: CODE_SCANNER_NAME,
    LEGACY_STATIC_SCANNER_NAME: STATIC_SCANNER_NAME,
}


def canonicalize_scanner_name(name: str) -> str:
    """Identify a historical scan without changing its signed representation."""
    return _ALIASES.get(name, name)


def validate_scanner_name(name: str) -> str:
    """Reject retired names in new requests while allowing custom scanners."""
    if name in _ALIASES:
        raise ValueError(
            f"unsupported scanner name: {name}; use {_ALIASES[name]} instead"
        )
    return name
