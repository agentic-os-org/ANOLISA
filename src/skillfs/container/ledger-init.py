#!/usr/bin/env python3
"""Seed a private source from an immutable flat skill bundle, then run Ledger."""

import argparse
import hashlib
import json
import os
import shutil
import stat
import tempfile
from pathlib import Path


def seed(package: Path, source: Path) -> list[Path]:
    """Publish the complete seed once; never overwrite an existing Ledger tree."""
    package = package.resolve(strict=True)
    if source.is_symlink():
        raise ValueError("source must not be a symlink")
    source = source.resolve()
    if source == package or source.is_relative_to(package) or package.is_relative_to(source):
        raise ValueError("package and source must be separate trees")
    skills = sorted(p for p in package.iterdir() if not p.name.startswith("."))
    skills = [p for p in skills if p.is_dir() and (p / "SKILL.md").is_file()]
    if not skills:
        raise ValueError(f"no flat skills with SKILL.md found in {package}")
    inventory = {}
    for skill in skills:
        for path in [skill, *sorted(skill.rglob("*"))]:
            mode = path.lstat().st_mode
            if path.is_symlink() or not (stat.S_ISDIR(mode) or stat.S_ISREG(mode)):
                raise ValueError(f"bundle contains a link or special file: {path}")
            if ".skill-meta" in path.relative_to(package).parts:
                raise ValueError(f"bundle must not supply Ledger state: {path}")
            digest = None
            if path.is_file():
                with path.open("rb") as stream:
                    digest = hashlib.file_digest(stream, "sha256").hexdigest()
            inventory[str(path.relative_to(package))] = [digest, mode & 0o111]

    marker = source / ".skillfs-seed.json"
    if source.exists() and any(source.iterdir()):
        # ponytail: upgrades use a new volume; add migration only with a version policy.
        if not marker.is_file() or json.loads(marker.read_text()) != inventory:
            raise ValueError("seed differs or is unrecognized; use a new source volume and rescan")
        if source.stat().st_uid != os.geteuid() or source.stat().st_mode & 0o077:
            raise ValueError("existing source must be owned by this UID with mode 0700")
    else:
        source.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix=".skillfs-seed-", dir=source.parent) as tmp:
            staged = Path(tmp) / "source"
            staged.mkdir(mode=0o700)
            for relative, (digest, executable) in inventory.items():
                target = staged / relative
                if digest is None:
                    target.mkdir(mode=0o755)
                    target.chmod(0o755)
                else:
                    # Omit package xattrs, including stale activation on directories.
                    shutil.copyfile(package / relative, target)
                    target.chmod(0o644 | executable)
            (staged / marker.name).write_text(json.dumps(inventory, sort_keys=True) + "\n")
            if source.exists():
                source.rmdir()
            staged.rename(source)
    return [source / p.name for p in skills]


def activate(skills: list[Path]) -> None:
    """Use the daemon's real scanner and activation policy without auto-approval."""
    from agent_sec_cli.daemon.jobs.skill_ledger.processor import process_skill_change
    from agent_sec_cli.daemon.jobs.skill_ledger.protocol import SkillFsChange
    from agent_sec_cli.skill_ledger.config import load_config, save_config

    config = load_config()
    config["enableDefaultSkillDirs"] = False
    config["managedSkillDirs"] = [str(skill) for skill in skills]
    save_config(config)
    for skill in skills:
        result = process_skill_change(
            SkillFsChange(canonical_skill_dir=skill, event_kinds={"reconcile"})
        )
        print(json.dumps(result), flush=True)
        if result["status"] != "processed":
            raise RuntimeError(f"Ledger initialization failed for {skill}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    activate(seed(args.package, args.source))


if __name__ == "__main__":
    main()
