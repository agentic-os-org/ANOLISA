"""Scanner Registry — load scanner/parser definitions from config.json.

The registry provides lookup-by-name for scanners and parsers.  Defaults
include ``skill-vetter`` and built-in Skill scanners.

Usage::

    from agent_sec_cli.skill_ledger.scanner.registry import ScannerRegistry

    reg = ScannerRegistry.from_config()      # loads config.json
    scanner = reg.get_scanner("skill-vetter")  # ScannerInfo | None
    parser  = reg.get_parser("findings-array") # ParserInfo | None
"""

from dataclasses import dataclass, field
from typing import Any, Optional

from agent_sec_cli.skill_ledger.config import load_config
from agent_sec_cli.skill_ledger.scanner.names import validate_scanner_name


@dataclass(frozen=True)
class ScannerInfo:
    """Metadata for a registered scanner.

    Attributes:
        name:        Unique scanner identifier.
        type:        Invocation type: ``builtin`` | ``cli`` | ``skill`` | ``api``.
        parser:      Name of the associated result parser.
        description: Human-readable description (optional).
        enabled:     Whether the scanner is active (default ``True``).
        extra:       All remaining config fields (``command``, ``endpoint``, …).
    """

    name: str
    type: str  # "builtin" | "cli" | "skill" | "api"
    parser: str = "findings-array"
    description: str = ""
    enabled: bool = True
    extra: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "ScannerInfo":
        """Construct from a raw config dict entry."""
        known = {"name", "type", "parser", "description", "enabled"}
        canonical_name = validate_scanner_name(str(d["name"]))
        return cls(
            name=canonical_name,
            type=d.get("type", "skill"),
            parser=d.get("parser", "findings-array"),
            description=d.get("description", ""),
            enabled=d.get("enabled", True),
            extra={k: v for k, v in d.items() if k not in known},
        )


@dataclass(frozen=True)
class ParserInfo:
    """Metadata for a registered result parser.

    Attributes:
        name:  Unique parser identifier (also the key in ``parsers{}``).
        type:  Parser type: ``findings-array`` | ``sarif`` | ``field-mapping`` | ``custom``.
        extra: All remaining config fields (``rootPath``, ``mappings``, …).
    """

    name: str
    type: str  # "findings-array" | "sarif" | "field-mapping" | "custom"
    extra: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_dict(cls, name: str, d: dict[str, Any]) -> "ParserInfo":
        """Construct from a raw config dict entry."""
        return cls(
            name=name,
            type=d.get("type", "findings-array"),
            extra={k: v for k, v in d.items() if k != "type"},
        )


class ScannerRegistry:
    """In-memory registry of scanners and parsers loaded from config.

    Provides O(1) lookup by name.
    """

    def __init__(
        self,
        scanners: dict[str, ScannerInfo],
        parsers: dict[str, ParserInfo],
    ) -> None:
        self._scanners = scanners
        self._parsers = parsers

    # ------------------------------------------------------------------
    # Factory
    # ------------------------------------------------------------------

    @classmethod
    def from_config(cls, config: dict[str, Any] | None = None) -> "ScannerRegistry":
        """Build a registry from the loaded config (or load it fresh)."""
        if config is None:
            config = load_config()

        scanners: dict[str, ScannerInfo] = {}
        for entry in config.get("scanners", []):
            if isinstance(entry, dict) and "name" in entry:
                info = ScannerInfo.from_dict(entry)
                scanners[info.name] = info

        parsers: dict[str, ParserInfo] = {}
        for name, entry in config.get("parsers", {}).items():
            if isinstance(entry, dict):
                parsers[name] = ParserInfo.from_dict(name, entry)

        return cls(scanners=scanners, parsers=parsers)

    # ------------------------------------------------------------------
    # Lookups
    # ------------------------------------------------------------------

    def get_scanner(self, name: str) -> Optional[ScannerInfo]:
        """Return the scanner with *name*, or ``None``."""
        return self._scanners.get(validate_scanner_name(name))

    def get_parser(self, name: str) -> Optional[ParserInfo]:
        """Return the parser with *name*, or ``None``."""
        return self._parsers.get(name)

    def get_parser_for_scanner(self, scanner_name: str) -> Optional[ParserInfo]:
        """Resolve scanner → parser name → parser info.  Returns ``None`` if not found."""
        scanner = self.get_scanner(scanner_name)
        if scanner is None:
            return None
        return self.get_parser(scanner.parser)

    def list_scanners(self, *, enabled_only: bool = True) -> list[ScannerInfo]:
        """Return all registered scanners, optionally filtered by ``enabled``."""
        return [s for s in self._scanners.values() if not enabled_only or s.enabled]

    def list_invocable_scanners(
        self,
        *,
        names: list[str] | None = None,
    ) -> list[ScannerInfo]:
        """Return scanners that the CLI can currently invoke.

        If *names* is given, only return scanners whose name is in the list.
        """
        scanners = self.list_scanners(enabled_only=True)
        # cli/api adapters are reserved extension points; only builtin is
        # implemented today.
        scanners = [s for s in scanners if s.type == "builtin"]
        if names is not None:
            name_set = {validate_scanner_name(name) for name in names}
            scanners = [s for s in scanners if s.name in name_set]
        return scanners
