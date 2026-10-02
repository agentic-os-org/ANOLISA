"""Status command — ledger-wide health overview.

Implements ``agent-sec-cli skill-ledger status``:

Collects infrastructure health (keys, config) and aggregate skill
integrity state into a single overview response.
"""

import logging
from typing import Any

from agent_sec_cli.skill_ledger.config import (
    config_path,
    default_skill_dir_entries,
    deprecated_skill_dir_entries,
    effective_skill_dir_entries,
    load_config,
    resolve_skill_dirs,
)
from agent_sec_cli.skill_ledger.core.checker import check_batch
from agent_sec_cli.skill_ledger.signing.base import SigningBackend
from agent_sec_cli.skill_ledger.signing.ed25519 import compute_fingerprint
from agent_sec_cli.skill_ledger.signing.key_manager import (
    key_enc_path,
    key_pub_path,
    keyring_dir,
    keys_exist,
)

logger = logging.getLogger(__name__)

# Raw Ed25519 seed is exactly 32 bytes; encrypted file is always larger.
_RAW_SEED_LEN = 32

_CRITICAL_STATUSES = frozenset({"deny", "tampered", "error"})
_ATTENTION_STATUSES = frozenset({"drifted", "warn"})

# Canonical ordering for the breakdown dict.
_STATUS_KEYS = ("pass", "none", "drifted", "warn", "deny", "tampered", "error")


def _keys_info() -> dict[str, Any]:
    """Collect signing key infrastructure status."""
    initialized = keys_exist()
    info: dict[str, Any] = {
        "initialized": initialized,
        "fingerprint": None,
        "publicKeyPath": str(key_pub_path()),
        "encrypted": None,
        "keyringSize": 0,
    }

    if initialized:
        raw_pub = key_pub_path().read_bytes()
        info["fingerprint"] = compute_fingerprint(raw_pub)
        enc_data = key_enc_path().read_bytes()
        info["encrypted"] = len(enc_data) != _RAW_SEED_LEN

    kdir = keyring_dir()
    if kdir.is_dir():
        info["keyringSize"] = len(list(kdir.glob("*.pub")))

    return info


def _config_info() -> dict[str, Any]:
    """Collect configuration summary."""
    cfg = load_config()
    cp = config_path()
    scanners = cfg.get("scanners", [])
    effective_skill_dirs = effective_skill_dir_entries(cfg)
    deprecated_skill_dirs = deprecated_skill_dir_entries(cfg)
    return {
        "configPath": str(cp),
        "customized": cp.is_file(),
        "defaultSkillDirsEnabled": bool(cfg.get("enableDefaultSkillDirs", True)),
        "defaultSkillDirPatterns": len(default_skill_dir_entries()),
        "managedSkillDirPatterns": len(cfg.get("managedSkillDirs", [])),
        "ignoredDeprecatedSkillDirPatterns": len(deprecated_skill_dirs),
        "effectiveSkillDirPatterns": len(effective_skill_dirs),
        "registeredScanners": [
            s["name"] for s in scanners if isinstance(s, dict) and "name" in s
        ],
    }


def _derive_health(breakdown: dict[str, int], total: int) -> str:
    """Derive overall health label from per-status counts.

    Priority (highest wins):

    * ``"critical"``  — at least one deny / tampered / error
    * ``"attention"``  — at least one drifted / warn
    * ``"unscanned"`` — every discovered skill has status ``none``
    * ``"healthy"``   — all skills pass
    * ``"empty"``     — no skills discovered
    """
    if total == 0:
        return "empty"

    if any(breakdown.get(s, 0) > 0 for s in _CRITICAL_STATUSES):
        return "critical"

    if any(breakdown.get(s, 0) > 0 for s in _ATTENTION_STATUSES):
        return "attention"

    if breakdown.get("none", 0) == total:
        return "unscanned"

    return "healthy"


def ledger_status(
    backend: SigningBackend,
    *,
    verbose: bool = False,
) -> dict[str, Any]:
    """Collect the full ledger-wide status overview.

    Returns a JSON-serialisable dict with keys, config, and aggregate
    skill health information.

    When *verbose* is True the per-skill ``results`` array is included.
    """
    keys = _keys_info()
    config = _config_info()

    dirs = resolve_skill_dirs()
    breakdown: dict[str, int] = {s: 0 for s in _STATUS_KEYS}

    if dirs:
        results = check_batch(dirs, backend)
        for r in results:
            s = r.get("status", "error")
            if s in breakdown:
                breakdown[s] += 1
            else:
                breakdown["error"] += 1
    else:
        results = []

    total = len(dirs)
    health = _derive_health(breakdown, total)

    data: dict[str, Any] = {
        "keys": keys,
        "config": config,
        "skills": {
            "discovered": total,
            "breakdown": breakdown,
            "health": health,
        },
    }

    if verbose:
        data["results"] = results

    return data
