"""Result parsers — normalise raw scanner output to NormalizedFinding[].

In v1 only ``findings-array`` is implemented (identity transform).
Future parser types (``sarif``, ``field-mapping``, ``custom``) plug in
via :func:`parse_findings`.
"""

import logging
from typing import Any

from agent_sec_cli.skill_ledger.models.finding import (
    VALID_LEVELS,
    NormalizedFinding,
)
from agent_sec_cli.skill_ledger.scanner.registry import ParserInfo

logger = logging.getLogger(__name__)


# ------------------------------------------------------------------
# Parser dispatch
# ------------------------------------------------------------------


def parse_findings(
    raw_findings: list[dict[str, Any]],
    parser_info: ParserInfo | None,
) -> list[NormalizedFinding]:
    """Normalise *raw_findings* using the given parser.

    Parameters:
        raw_findings: The raw list of finding dicts from the scanner output.
        parser_info:  Registry metadata for the parser.  If ``None``, fall
                      back to ``findings-array`` (backward-compatible default).

    Returns:
        A list of :class:`NormalizedFinding` instances.
    """
    parser_type = parser_info.type if parser_info is not None else "findings-array"

    if parser_type == "findings-array":
        return _parse_findings_array(raw_findings)

    # Future parser types — not implemented in this version
    # "sarif", "field-mapping", "custom"
    logger.warning(
        "Parser type %r is not implemented; falling back to findings-array",
        parser_type,
    )
    return _parse_findings_array(raw_findings)


# ------------------------------------------------------------------
# findings-array (identity parser)
# ------------------------------------------------------------------


def _parse_findings_array(
    raw_findings: list[dict[str, Any]],
) -> list[NormalizedFinding]:
    """Identity parser — input is already ``[{rule, level, message, …}]``.

    Each dict is mapped directly to :class:`NormalizedFinding`.
    Fields not in the model are placed into ``metadata``.
    Invalid entries (missing ``rule`` or ``level``) are skipped with a warning.
    """
    result: list[NormalizedFinding] = []
    known_keys = {"rule", "level", "message", "file", "line", "metadata"}

    for idx, item in enumerate(raw_findings):
        if not isinstance(item, dict):
            logger.warning("Skipping non-dict finding at index %d", idx)
            continue

        rule = item.get("rule")
        level = item.get("level")
        message = item.get("message", "")

        if not rule or not level:
            logger.warning(
                "Skipping finding at index %d: missing 'rule' or 'level'", idx
            )
            continue

        # Normalise level
        level_lower = str(level).lower()
        if level_lower not in VALID_LEVELS:
            logger.warning(
                "Unknown level %r at index %d; treating as 'warn'",
                level,
                idx,
            )
            level_lower = "warn"

        # Collect extra keys into metadata
        extra = {k: v for k, v in item.items() if k not in known_keys}
        metadata = item.get("metadata", {})
        if extra:
            metadata = {**metadata, **extra}

        result.append(
            NormalizedFinding(
                rule=str(rule),
                level=level_lower,
                message=str(message),
                file=item.get("file"),
                line=item.get("line"),
                metadata=metadata,
            )
        )

    return result
