""".skill-meta/ directory management, version numbering, and snapshot creation.

.. warning::

   This module does **not** provide file-level locking.  If multiple
   processes call :func:`save_manifest` concurrently on the same skill
   directory, the writes may conflict.  Callers in concurrent
   environments should serialise access externally (e.g. ``flock``).
"""

import os
import re
import shutil
import stat
from pathlib import Path

from agent_sec_cli.skill_ledger.errors import SkillLedgerError
from agent_sec_cli.skill_ledger.models.manifest import SignedManifest

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

SKILL_META_DIR = ".skill-meta"
VERSIONS_DIR = "versions"
LATEST_JSON = "latest.json"

_VERSION_RE = re.compile(r"^v(\d{6})\.json$")
_SNAPSHOT_RE = re.compile(r"^v(\d{6})\.snapshot$")
_VERSION_ID_RE = re.compile(r"^v\d{6}$")

# Directories excluded when creating a snapshot of the skill directory.
_SNAPSHOT_EXCLUDED = frozenset({".skill-meta", ".git"})


# ---------------------------------------------------------------------------
# Path helpers
# ---------------------------------------------------------------------------


def skill_meta_path(skill_dir: str | Path) -> Path:
    return Path(skill_dir) / SKILL_META_DIR


def latest_json_path(skill_dir: str | Path) -> Path:
    return skill_meta_path(skill_dir) / LATEST_JSON


def versions_dir_path(skill_dir: str | Path) -> Path:
    return skill_meta_path(skill_dir) / VERSIONS_DIR


def version_json_path(skill_dir: str | Path, version_id: str) -> Path:
    return versions_dir_path(skill_dir) / f"{version_id}.json"


def snapshot_dir_path(skill_dir: str | Path, version_id: str) -> Path:
    return versions_dir_path(skill_dir) / f"{version_id}.snapshot"


# ---------------------------------------------------------------------------
# Directory initialisation
# ---------------------------------------------------------------------------


def ensure_skill_meta(skill_dir: str | Path) -> Path:
    """Create ``.skill-meta/versions/`` if it does not exist.  Returns the meta path."""
    meta = skill_meta_path(skill_dir)
    meta.mkdir(parents=True, exist_ok=True)
    versions = versions_dir_path(skill_dir)
    versions.mkdir(parents=True, exist_ok=True)
    return meta


# ---------------------------------------------------------------------------
# Version ID management
# ---------------------------------------------------------------------------


def list_version_ids(skill_dir: str | Path) -> list[str]:
    """Return sorted list of existing version IDs (e.g. ``["v000001", "v000002"]``)."""
    vdir = versions_dir_path(skill_dir)
    try:
        mode = vdir.stat().st_mode
    except FileNotFoundError:
        return []
    if not stat.S_ISDIR(mode):
        return []
    ids: list[str] = []
    with os.scandir(vdir) as entries:
        for entry in entries:
            match = _VERSION_RE.match(entry.name)
            if match:
                ids.append(f"v{match.group(1)}")
    ids.sort()
    return ids


def list_version_artifact_ids(skill_dir: str | Path) -> list[str]:
    """Return IDs reserved by a version JSON or snapshot path.

    Snapshot-only slots still represent historical evidence and must never be
    reused after a partial write or manual damage removes the matching JSON.
    """
    vdir = versions_dir_path(skill_dir)
    try:
        mode = vdir.stat().st_mode
    except FileNotFoundError:
        return []
    if not stat.S_ISDIR(mode):
        return []

    ids: set[str] = set()
    with os.scandir(vdir) as entries:
        for entry in entries:
            match = _VERSION_RE.match(entry.name) or _SNAPSHOT_RE.match(entry.name)
            if match:
                ids.add(f"v{match.group(1)}")
    return sorted(ids)


def is_version_id(value: str) -> bool:
    """Return whether *value* is a canonical six-digit version ID."""
    return _VERSION_ID_RE.fullmatch(value) is not None


def next_version_id(
    skill_dir: str | Path,
    *,
    after_version_id: str | None = None,
) -> str:
    """Return the first unused ID after the newest verified predecessor.

    Version JSON and snapshot names reserve their own slots, but an unverified
    high-numbered artifact cannot force recovery to skip lower free slots.

    Raises :class:`SkillLedgerError` if the maximum version (999999) is reached.
    """
    existing = set(list_version_artifact_ids(skill_dir))
    start = int(after_version_id[1:]) + 1 if after_version_id is not None else 1
    for number in range(start, 1_000_000):
        candidate = f"v{number:06d}"
        if candidate not in existing:
            return candidate
    raise SkillLedgerError(
        "Version ID overflow — maximum 999999 versions reached for "
        f"{Path(skill_dir).name}"
    )


# ---------------------------------------------------------------------------
# Manifest I/O
# ---------------------------------------------------------------------------


def load_latest_manifest(skill_dir: str | Path) -> SignedManifest | None:
    """Load ``latest.json`` if it exists, else return ``None``."""
    path = latest_json_path(skill_dir)
    try:
        mode = path.stat().st_mode
    except FileNotFoundError:
        return None
    if not stat.S_ISREG(mode):
        return None
    return SignedManifest.from_file(str(path))


def save_manifest(
    skill_dir: str | Path,
    manifest: SignedManifest,
    *,
    write_version: bool = True,
) -> None:
    """Write *manifest* to ``versions/<versionId>.json`` and ``latest.json``.

    Both writes are atomic (write-tmp + rename).
    """
    ensure_skill_meta(skill_dir)
    if write_version:
        vpath = version_json_path(skill_dir, manifest.versionId)
        manifest.write_to_file(str(vpath))
    # Always update latest.json
    manifest.write_to_file(str(latest_json_path(skill_dir)))


def load_version_manifest(
    skill_dir: str | Path, version_id: str
) -> SignedManifest | None:
    """Load a specific version manifest, or ``None`` if it does not exist."""
    path = version_json_path(skill_dir, version_id)
    try:
        mode = path.stat().st_mode
    except FileNotFoundError:
        return None
    if not stat.S_ISREG(mode):
        return None
    return SignedManifest.from_file(str(path))


# ---------------------------------------------------------------------------
# Snapshot
# ---------------------------------------------------------------------------


def create_snapshot(skill_dir: str | Path, version_id: str) -> Path:
    """Copy the skill directory (excluding ``.skill-meta/`` and ``.git/``) into a snapshot.

    Symbolic links are skipped to stay consistent with :func:`compute_file_hashes`
    and to prevent directory-escape attacks.

    Returns the snapshot directory path.
    """
    src = Path(skill_dir).resolve()
    dst = snapshot_dir_path(skill_dir, version_id)
    if dst.exists():
        raise SkillLedgerError(f"snapshot already exists for version {version_id}")
    dst.mkdir(parents=True)

    for entry in sorted(src.rglob("*")):
        if entry.is_symlink():
            continue
        rel = entry.relative_to(src)
        if any(part in _SNAPSHOT_EXCLUDED for part in rel.parts):
            continue
        target = dst / rel
        if entry.is_dir():
            target.mkdir(parents=True, exist_ok=True)
        elif entry.is_file():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(entry, target)

    return dst
