#!/usr/bin/env python3
"""Sideload the macOS plugin without changing desktop settings."""

from __future__ import annotations

import sys

# Direct Python invocations must be as read-only as the wrappers' python3 -B.
sys.dont_write_bytecode = True

import argparse
import hashlib
import importlib.util
import json
import os
import platform
import plistlib
import shutil
import stat
import tempfile
from pathlib import Path

PLUGIN_ROOT = Path(__file__).resolve().parents[1]
MARKER = ".tokenless-qwenwork-owned.json"
OWNER = "tokenless/qwenwork"
RUNTIME_FILES = (
    ".qoder-plugin/plugin.json",
    "hooks/hooks.json",
    "hooks/run-hook.sh",
    "hooks/shared/rewrite_hook.py",
    "hooks/shared/compress_response_hook.py",
    "hooks/shared/hook_utils.py",
    "hooks/shared/tool_categories.json",
    "scripts/lifecycle.py",
    "scripts/detect.sh",
    "scripts/install.sh",
    "scripts/uninstall.sh",
)


class LifecycleError(RuntimeError):
    """An unsafe path, incomplete plugin, or failed lifecycle operation."""


def _absolute(value: str) -> Path:
    path = Path(os.path.expanduser(value))
    if not path.is_absolute() or ".." in path.parts:
        raise LifecycleError(f"Expected an absolute path without '..': {value}")
    return path


def _check_directories(anchor: Path, destination: Path) -> None:
    for part in destination.relative_to(anchor).parts:
        anchor = anchor / part
        if anchor.is_symlink():
            raise LifecycleError(f"Refusing symlink in managed path: {anchor}")
        if anchor.exists() and not anchor.is_dir():
            raise LifecycleError(f"Expected a directory: {anchor}")


def _paths() -> tuple[Path, Path, Path]:
    raw_home = _absolute(os.environ.get("HOME", str(Path.home())))
    # HOME is the trusted anchor: /var -> /private/var on macOS is not an
    # unsafe link inside our managed boundary.
    home = raw_home.resolve()
    configured = os.environ.get("TOKENLESS_QWENWORK_HOME")
    data = _absolute(configured) if configured is not None else raw_home / ".qwenworkcn"
    for candidate in (raw_home, home):
        try:
            relative = data.relative_to(candidate)
        except ValueError:
            continue
        data = home / relative
        anchor = home
        break
    else:
        # For an explicit external data root, its parent is caller-selected;
        # the root itself and every managed child must still be real directories.
        anchor = data.parent.resolve()
        data = anchor / data.name
    target = data / "plugins-custom" / "tokenless"
    _check_directories(anchor, target)
    return home, data, target


def _application(home: Path) -> tuple[Path, str]:
    override = os.environ.get("TOKENLESS_QWENWORK_APP")
    candidates = (
        [_absolute(override)]
        if override is not None
        else [Path("/Applications/QwenWorkCN.app"), home / "Applications/QwenWorkCN.app"]
    )
    app = next((path for path in candidates if path.is_dir()), None)
    if app is None:
        raise LifecycleError("QwenWorkCN.app not found; checked: " + ", ".join(map(str, candidates)))
    info_path = app / "Contents/Info.plist"
    try:
        with info_path.open("rb") as handle:
            info = plistlib.load(handle)
    except (OSError, ValueError, plistlib.InvalidFileException) as error:
        raise LifecycleError(f"Cannot read app metadata {info_path}: {error}") from error
    if not isinstance(info, dict) or info.get("CFBundleIdentifier") != "cn.qwenwork.desktop.mac":
        raise LifecycleError(
            f"Unexpected bundle identifier in {info_path}; expected cn.qwenwork.desktop.mac"
        )
    version = info.get("CFBundleShortVersionString")
    if not isinstance(version, str) or not version.strip():
        raise LifecycleError(f"Missing CFBundleShortVersionString in {info_path}")
    return app, version


def _ignored(_directory: str, names: list[str]) -> set[str]:
    return {
        name
        for name in names
        if name == "__pycache__" or name == MARKER or name.endswith((".pyc", ".in"))
    }


def _check_source(root: Path, relative: Path = Path()) -> None:
    directory = root / relative
    for path in directory.iterdir():
        if path.name in _ignored(str(directory), [path.name]):
            continue
        child = relative / path.name
        if path.is_symlink():
            if child != Path("hooks/shared") or os.readlink(path) != "../../common/hooks":
                raise LifecycleError(f"Refusing unexpected source symlink: {path}")
            if not path.is_dir():
                raise LifecycleError(f"Missing shared hook directory: {path}")
        if path.is_dir():
            _check_source(root, child)
        elif not stat.S_ISREG(path.lstat().st_mode):
            raise LifecycleError(f"Refusing non-regular source resource: {path}")


def _read_json(path: Path) -> dict:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise LifecycleError(f"Cannot read JSON resource {path}: {error}") from error
    if not isinstance(value, dict):
        raise LifecycleError(f"Expected a JSON object: {path}")
    return value


def _validate_runtime(root: Path) -> None:
    for relative in RUNTIME_FILES:
        path = root / relative
        if not path.is_file() or not os.access(path, os.R_OK):
            raise LifecycleError(
                f"Missing or unreadable runtime resource: {path}; "
                "build/stamp adapter templates before installing"
            )
    manifest_path = root / ".qoder-plugin/plugin.json"
    manifest = _read_json(manifest_path)
    if (
        manifest.get("name") != "tokenless"
        or not isinstance(manifest.get("version"), str)
        or not manifest["version"].strip()
        or "@" in manifest["version"]
    ):
        raise LifecycleError(f"Invalid or unstamped plugin manifest: {manifest_path}")
    for relative in ("hooks/hooks.json", "hooks/shared/tool_categories.json"):
        _read_json(root / relative)


def _dependencies(root: Path) -> None:
    path = root / "hooks/shared/hook_utils.py"
    spec = importlib.util.spec_from_file_location("qwenwork_hook_utils", path)
    if spec is None or spec.loader is None:
        raise LifecycleError(f"Cannot load shared binary resolver: {path}")
    utils = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(utils)
    tokenless = utils.resolve_binary("tokenless")
    if not tokenless:
        raise LifecycleError("tokenless binary not found; install Tokenless separately, then retry")
    print(f"Tokenless: {tokenless}")
    rtk = utils.resolve_binary("rtk")
    if rtk:
        print(f"RTK: {rtk}")
    else:
        print(
            "WARNING: rtk binary not found; command rewriting is unavailable "
            "(response hooks remain available).",
            file=sys.stderr,
        )


def _inventory(root: Path) -> dict:
    entries = {}

    def visit(path: Path) -> None:
        relative = path.relative_to(root).as_posix()
        metadata = path.lstat()
        mode = stat.S_IMODE(metadata.st_mode)
        if stat.S_ISDIR(metadata.st_mode):
            entries[relative] = {"kind": "directory", "mode": mode}
            for child in sorted(path.iterdir()):
                if child != root / MARKER:
                    visit(child)
        elif stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1:
            entries[relative] = {
                "kind": "file", "mode": mode,
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
        else:
            raise LifecycleError(f"Refusing symlink, hard link or special file in plugin: {path}")

    visit(root)
    return entries


def _owned(root: Path) -> dict | None:
    if root.is_symlink():
        raise LifecycleError(f"Refusing symlink target: {root}")
    if not root.exists():
        return None
    marker = root / MARKER
    if not root.is_dir() or not marker.is_file() or marker.is_symlink():
        raise LifecycleError(f"Foreign plugin conflict (no owned marker): {root}")
    metadata = marker.stat()
    if metadata.st_nlink != 1:
        raise LifecycleError(f"Refusing hard-linked owned marker: {marker}")
    try:
        recorded = json.loads(marker.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise LifecycleError(f"Invalid owned marker: {marker}: {error}") from error
    if (
        not isinstance(recorded, dict)
        or set(recorded) != {"owner", "schema", "entries"}
        or recorded["owner"] != OWNER
        or recorded["schema"] != 1
        or not isinstance(recorded["entries"], dict)
    ):
        raise LifecycleError(f"Foreign or invalid owned marker: {marker}")
    actual = _inventory(root)
    if actual != recorded["entries"]:
        changed = sorted(
            key for key in actual.keys() | recorded["entries"].keys()
            if actual.get(key) != recorded["entries"].get(key)
        )
        raise LifecycleError(
            f"Plugin modified; refusing to overwrite or remove {root}: {', '.join(changed[:5])}"
        )
    return actual


def _install(data: Path, target: Path, previous: dict | None) -> None:
    data.mkdir(parents=True, exist_ok=True)
    target.parent.mkdir(exist_ok=True)
    _paths()
    if data.stat().st_dev != target.parent.stat().st_dev:
        raise LifecycleError(f"Staging root {data} and scanner {target.parent} must share a filesystem")
    stage = Path(tempfile.mkdtemp(prefix=".tokenless-stage-", dir=str(data)))
    new = stage / "plugin"
    backup = stage / "previous"
    try:
        shutil.copytree(PLUGIN_ROOT, new, symlinks=False, ignore=_ignored)
        _validate_runtime(new)
        entries = _inventory(new)
        marker = new / MARKER
        marker.write_text(
            json.dumps({"owner": OWNER, "schema": 1, "entries": entries}, indent=2) + "\n",
            encoding="utf-8",
        )
        marker.chmod(0o600)
        _paths()
        if _owned(target) != previous:
            raise LifecycleError(f"Plugin changed during installation: {target}")
        if entries != previous:
            if previous is not None:
                os.replace(target, backup)
            try:
                os.replace(new, target)
            except OSError as publish_error:
                if previous is not None:
                    try:
                        if os.path.lexists(target):
                            raise LifecycleError(f"Rollback destination is occupied: {target}")
                        os.replace(backup, target)
                    except (OSError, LifecycleError) as rollback_error:
                        raise LifecycleError(
                            f"Publish failed: {publish_error}; rollback failed: {rollback_error}; "
                            f"previous plugin retained at {backup}; new plugin at {new}"
                        ) from rollback_error
                    raise LifecycleError(
                        f"Publish failed: {publish_error}; previous plugin restored to {target}"
                    ) from publish_error
                raise
        if backup.exists():
            if _owned(backup) != previous:
                raise LifecycleError(f"Backup changed; retained at {backup}")
        shutil.rmtree(stage)
    except (OSError, ValueError, LifecycleError) as error:
        raise LifecycleError(
            f"Install incomplete: {error}; recovery files retained at {stage}; target: {target}"
        ) from error


def main(argv: list[str] | None = None) -> int:
    """Run a read-only probe, explicit sideload, or ownership-checked removal."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("detect", "install", "uninstall"))
    parser.add_argument(
        "--dry-run",
        action="store_true",
        default=os.environ.get("ANOLISA_DRY_RUN") == "1",
        help="validate without writing any files (also the default under ANOLISA_DRY_RUN=1)",
    )
    args = parser.parse_args(argv)
    try:
        home, data, target = _paths()
        if args.action == "uninstall":
            if _owned(target) is None:
                print(f"Already absent: {target}")
            elif args.dry_run:
                print(f"DRY-RUN: would remove owned plugin {target}")
            else:
                shutil.rmtree(target)
                print(
                    f"Removed sideloaded plugin: {target}. "
                    "Restart QwenWorkCN and open a new session."
                )
            return 0
        if platform.system() != "Darwin":
            raise LifecycleError("QwenWorkCN sideloading supports macOS (Darwin) only")
        app, version = _application(home)
        print(f"QwenWorkCN: {app} (version {version})")
        print(
            "Compatibility baselines: QwenWorkCN 1.2.5 native Bash and 1.3.2 workspace Shell; "
            "check plugin activation in the UI."
        )
        if version not in {"1.2.5", "1.3.2"}:
            print(
                f"WARNING: version {version} is outside the 1.2.5 / 1.3.2 baselines; "
                "compatibility is unverified.",
                file=sys.stderr,
            )
        _check_source(PLUGIN_ROOT)
        _validate_runtime(PLUGIN_ROOT)
        _dependencies(PLUGIN_ROOT)
        for source in (PLUGIN_ROOT.resolve(), (PLUGIN_ROOT / "hooks/shared").resolve()):
            if data == source or source in data.parents:
                raise LifecycleError(f"Staging root must be outside plugin source: {data}")
        previous = _owned(target)
        if args.action == "detect":
            status = "sideloaded" if previous is not None else "not sideloaded"
            print(f"Resources ready; plugin {status}: {target}. Enablement is not checked.")
        elif args.dry_run:
            print(f"DRY-RUN: would sideload plugin to {target}; no files changed.")
        else:
            _install(data, target, previous)
            print(
                f"Sideloaded: {target}. Restart QwenWorkCN, open a new session, "
                "and enable the plugin in the UI if needed. Enablement is not checked."
            )
        return 0
    except (LifecycleError, OSError, ValueError, ImportError, SyntaxError) as error:
        print(f"[tokenless] ERROR: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
