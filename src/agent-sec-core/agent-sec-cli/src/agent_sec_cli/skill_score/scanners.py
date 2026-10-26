"""Use the installed scanners directly; their signals never deduct points."""

from pathlib import Path
from typing import Any

import yaml

from agent_sec_cli.code_scanner.models import Language
from agent_sec_cli.code_scanner.scanner import scan as code_scan
from agent_sec_cli.pii_checker.scanner import PiiScanner
from agent_sec_cli.security_middleware.backends.prompt_scan import (
    PromptScanBackend,
)
from agent_sec_cli.security_middleware.context import RequestContext
from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_cli.skill_score.evidence.redact import redact_value
from agent_sec_cli.skill_score.vendor.masb.rules_factory import RulesFactory
from agent_sec_cli.skill_score.vendor.masb.scanner.detector import (
    SecurityDetector,
)


def redact_source(source: dict[str, Any]) -> dict[str, Any]:
    """Reconstruct source references without repeating the risk scanners."""
    pii = PiiScanner()
    source["files"] = {
        path: pii.scan(text, redact_output=True).redacted_text if text.strip() else text
        for path, text in source["files"].items()
    }
    return source


def scan_content(items: list[dict[str, str]], config: ScoreConfig) -> dict[str, Any]:
    prompt, pii = PromptScanBackend(), PiiScanner()
    signals, cleaned = [], {}
    for item in items:
        text, ref = item["text"], item["ref"]
        if not text.strip():
            cleaned[ref] = text
            continue
        result = pii.scan(text, redact_output=True)
        cleaned[ref] = result.redacted_text
        for hit in result.to_dict()["findings"]:
            signals.append({"scanner": "pii", "ref": ref, "detail": hit})
        # Direct backend execution avoids writing evaluated content to host audit storage.
        prompt_result = prompt.execute(RequestContext(action="prompt_scan"), text=text, mode="fast")
        data = prompt_result.data or {}
        if (
            not prompt_result.success
            or data.get("verdict") not in {"pass", "warn", "deny"}
            or data.get("degraded") is not False
            or data.get("input_truncated") is not False
            or data.get("layers_failed")
            or not isinstance(data.get("findings"), list)
        ):
            raise ContractError("prompt scanner failed or returned incomplete coverage")
        for hit in data["findings"]:
            signals.append({"scanner": "prompt", "ref": ref, "detail": hit})
        if item.get("language"):
            code = code_scan(text, Language(item["language"]), mode="regex")
            if not code.ok:
                raise ContractError(code.summary)
            for hit in code.model_dump(mode="json")["findings"]:
                signals.append({"scanner": "code", "ref": ref, "detail": hit})
    return redact_value(
        {
            "signals": signals,
            "cleaned": cleaned,
            "scanners": ["prompt:fast", "code:regex", "pii:local"],
        }
    )


def scan_skill(skill: Path, source: dict[str, Any], config: ScoreConfig) -> dict[str, Any]:
    items = [
        {
            "ref": path,
            "text": text,
            "language": (
                "python" if path.endswith(".py") else "bash" if path.endswith(".sh") else ""
            ),
        }
        for path, text in source["files"].items()
    ]
    result = scan_content(items, config)
    assets = Path(__file__).parent / "vendor/masb/config"
    rules = yaml.safe_load((assets / "rules.yaml").read_text())
    whitelist = yaml.safe_load((assets / "whitelist.yaml").read_text())
    detector = SecurityDetector(
        RulesFactory.create_rules([rule for group in rules.values() for rule in group]),
        whitelist.get("whitelist", []),
    )
    result["masb"] = redact_value(detector.scan(str(skill)))
    return result
