"""Select and recheck the Qoder executable before running it with enhancements."""

import hashlib
import json
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
from pathlib import Path
from urllib.parse import urlparse

from i18n import tr
from setup import atomic_write, json_bytes, private_directory, read_json

VERSION = "1.1.84"
URL = (
    "https://qoder-ide.oss-accelerate.aliyuncs.com/qodercli/releases/"
    f"{VERSION}/qodercli-linux-x64-baseline.tar.gz"
)
ARCHIVE_SHA256 = "e85d82850c700b3e8691b7be35bf3c192223f8523ff22a96c011039a872f27a9"
BINARY_SHA256 = "c645b7aeceacee1ba118458811fc4bcf923920e5e13f9a8aad3404d143d46d31"
TESTED_BUILDS = {"3d8d39cfb800c7287bd319e7c9aaa6275714740e013bdaa6b08ea9157a72343f": "1.1.91"}
PAT = "QODER_PERSONAL_ACCESS_TOKEN"
BASE_URL = "https://qoder-ide.oss-accelerate.aliyuncs.com/qodercli"


def data_directory() -> Path:
    return (
        Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share"))
        / "anolisa-omarchy/qoder"
    )


def release(channel: str) -> dict:
    """Resolve latest once; the recommended build stays pinned and reproducible."""
    if channel == "recommended":
        return {"version": VERSION, "url": URL, "sha256": ARCHIVE_SHA256, "channel": channel}
    if channel != "latest":
        raise ValueError("Unknown Qoder release channel")
    print(tr("Checking the latest Qoder CLI release…"), flush=True)
    with urllib.request.urlopen(BASE_URL + "/channels/manifest.json", timeout=45) as response:
        manifest = json.load(response)
    if not isinstance(manifest, dict) or not isinstance(manifest.get("files"), list):
        raise ValueError(tr("The Qoder release manifest is invalid."))
    version = manifest.get("latest")
    if not re.fullmatch(r"\d+\.\d+\.\d+", str(version)):
        raise ValueError(tr("The Qoder release manifest is invalid."))
    # Baseline builds work on older laptop CPUs. Newer releases use one x64 build.
    entries = {
        entry.get("arch"): entry
        for entry in manifest["files"]
        if isinstance(entry, dict) and entry.get("os") == "linux"
    }
    entry = entries.get("amd64-baseline") or entries.get("amd64") or {}
    url = entry.get("url", "")
    parsed = urlparse(url)
    if (
        parsed.scheme != "https"
        or parsed.netloc != "qoder-ide.oss-accelerate.aliyuncs.com"
        or not parsed.path.startswith(f"/qodercli/releases/{version}/")
        or not parsed.path.endswith(".tar.gz")
        or not re.fullmatch(r"[0-9a-f]{64}", str(entry.get("sha256", "")))
    ):
        raise ValueError(tr("The Qoder release manifest is invalid."))
    return {"version": version, "url": url, "sha256": entry["sha256"], "channel": channel}


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def stage_qoder(cache: Path, destination: Path, resolved: dict | None = None) -> str:
    """Publish only the verified native executable, including when repairing a release."""
    cache.mkdir(parents=True, exist_ok=True)
    resolved = resolved or release("recommended")
    version, checksum = resolved["version"], resolved["sha256"]
    archive = cache / f"qodercli-{version}-{checksum[:12]}.tar.gz"
    if not archive.is_file() or digest(archive) != checksum:
        with tempfile.TemporaryDirectory(dir=cache) as directory:
            temporary = Path(directory) / "qoder.tar.gz"
            print(tr("Downloading Qoder CLI {0} from the official source…", version), flush=True)
            with (
                urllib.request.urlopen(resolved["url"], timeout=45) as response,
                temporary.open("wb") as out,
            ):
                downloaded = 0
                next_update = 8 * 1024 * 1024
                while block := response.read(1024 * 1024):
                    out.write(block)
                    downloaded += len(block)
                    if downloaded >= next_update:
                        print(tr("Downloaded {0} MiB…", downloaded // (1024 * 1024)), flush=True)
                        next_update = downloaded + 8 * 1024 * 1024
            if digest(temporary) != checksum:
                raise ValueError(tr("Qoder CLI download verification failed."))
            temporary.replace(archive)

    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as directory:
        temporary = Path(directory) / "qodercli"
        with tarfile.open(archive) as tar:
            members = [
                entry
                for entry in tar.getmembers()
                if Path(entry.name).name == "qodercli" and entry.isfile()
            ]
            if len(members) != 1:
                raise ValueError(tr("The Qoder CLI archive contains an invalid executable."))
            with tar.extractfile(members[0]) as source, temporary.open("wb") as output:
                shutil.copyfileobj(source, output)
        binary_checksum = digest(temporary)
        if (
            version == VERSION
            and resolved["channel"] == "recommended"
            and binary_checksum != BINARY_SHA256
        ):
            raise ValueError(tr("Qoder CLI executable verification failed."))
        temporary.chmod(0o755)
        # Check ABI and reported version before replacing any working executable.
        result = subprocess.run(
            [str(temporary), "--version"], capture_output=True, text=True, timeout=20
        )
        if result.returncode or version not in re.findall(r"\d+\.\d+\.\d+", result.stdout):
            raise ValueError(tr("The installed Qoder CLI did not pass verification."))
        temporary.replace(destination)
        return binary_checksum


def install(channel: str) -> dict:
    """Install in the user's data directory; callers hold the setup lock."""
    resolved = release(channel)
    root = data_directory()
    private_directory(root)
    destination = root / (resolved["version"] + "-" + resolved["sha256"][:12]) / "qodercli"
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "anolisa-omarchy/qoder"
    checksum = stage_qoder(cache, destination, resolved)
    selected = {
        "schema": 1,
        "source": "downloaded",
        "path": str(destination),
        "version": resolved["version"],
        "sha256": checksum,
        "channel": channel,
    }
    atomic_write(root / "current.json", json_bytes(selected))
    return inspect(destination, "downloaded", selected)


def display_digest(path: Path) -> str:
    """Cache display-only hashes across short-lived panel status processes.

    Launch and configuration checks always hash the full file. This receipt only
    avoids repeated polling work; it must never authorize execution.
    """
    info = path.stat()
    identity = [
        info.st_dev,
        info.st_ino,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
        info.st_mode,
    ]
    target = data_directory() / "display-inspection.json"
    # A display cache is optional; its state must not hide a usable executable.
    try:
        cached = read_json(target) or {}
    except (OSError, ValueError):
        cached = {}
    if (
        cached.get("path") == str(path)
        and cached.get("identity") == identity
        and re.fullmatch(r"[a-f0-9]{64}", str(cached.get("sha256", "")))
    ):
        return cached["sha256"]
    checksum = digest(path)
    try:
        private_directory(target.parent)
        atomic_write(
            target, json_bytes({"path": str(path), "identity": identity, "sha256": checksum})
        )
    except (OSError, ValueError):
        pass
    return checksum


def inspect(path: Path, source: str, expected: dict | None = None, *, full: bool = True) -> dict:
    report = {"source": source, "path": str(path), "version": None, "installation": "missing"}
    try:
        if not path.is_file() or not os.access(path, os.X_OK):
            return report
        path = path.resolve(strict=True)
        report.update(path=str(path), installation="unknown")
        checksum = digest(path) if full else display_digest(path)
        # Known digests identify tested builds without starting Qoder during status polling.
        known_version = VERSION if checksum == BINARY_SHA256 else TESTED_BUILDS.get(checksum)
        known = known_version is not None
        if not known and not (expected and checksum == expected.get("sha256")):
            return dict(report, installation="incompatible")
        report.update(
            version=known_version if known else expected["version"],
            installation="compatible",
            sha256=checksum,
            channel=(expected or {}).get("channel", "recommended"),
            build_validation="tested" if known else "not_verified",
        )
    except OSError:
        pass
    return report


def candidates(prefix: Path) -> list[dict]:
    bundled = prefix / "bin/qodercli"
    reports = []
    managed = prefix.resolve().parent.parent
    for directory in os.get_exec_path():
        executable = shutil.which("qodercli", path=directory or ".")
        if executable:
            path = Path(executable).resolve()
            if (
                path == bundled.resolve()
                or path == (Path.home() / ".local/bin/anolisa-omarchy").resolve()
                or path.is_relative_to(managed / "releases")
            ):
                continue
            reports.append(inspect(path, "existing"))
            break
    receipt = read_json(data_directory() / "current.json")
    if receipt:
        receipt = checked_selection(receipt)
        if receipt["source"] != "downloaded":
            raise ValueError(tr("Invalid Qoder source record. Select the program again."))
        reports.insert(0, inspect(Path(receipt["path"]), "downloaded", receipt))
    return [*reports, inspect(bundled, "bundled")]


def selection(report: dict) -> dict:
    if report["installation"] != "compatible":
        raise ValueError(
            tr("Qoder CLI has not passed compatibility checks. Reinstall it from Settings.")
        )
    path = report["path"] if report["source"] != "bundled" else None
    return {
        "schema": 1,
        "source": report["source"],
        "path": path,
        "version": report["version"],
        "sha256": report["sha256"],
        "channel": report.get("channel", "recommended"),
    }


def checked_selection(selected: dict) -> dict:
    if selected.get("schema") != 1 or selected.get("source") not in (
        "existing",
        "bundled",
        "downloaded",
    ):
        raise ValueError(tr("Invalid Qoder source record. Select the program again."))
    path = selected.get("path")
    if selected["source"] != "bundled" and (
        not isinstance(path, str) or not Path(path).is_absolute()
    ):
        raise ValueError(tr("Invalid Qoder program path. Select the program again."))
    if selected["source"] == "downloaded" and not Path(path).is_relative_to(data_directory()):
        raise ValueError(tr("Invalid Qoder program path. Select the program again."))
    if selected.get("channel", "recommended") not in ("recommended", "latest"):
        raise ValueError(tr("Invalid Qoder source record. Select the program again."))
    if not re.fullmatch(r"\d+\.\d+\.\d+", str(selected.get("version", ""))) or not re.fullmatch(
        r"[0-9a-f]{64}", str(selected.get("sha256", ""))
    ):
        raise ValueError(tr("Invalid Qoder compatibility record. Select the program again."))
    return selected


def read_selection(config: Path) -> dict | None:
    selected = read_json(config / "agent.json")
    return checked_selection(selected) if selected is not None else None


def executable(prefix: Path, selected: dict | None) -> Path:
    if selected and selected["source"] != "bundled":
        return Path(selected["path"])
    return prefix / "bin/qodercli"


def validate(prefix: Path, selected: dict | None, *, full: bool = True) -> dict:
    source = selected["source"] if selected else "bundled"
    path = executable(prefix, selected)
    report = inspect(path, source, selected if source == "downloaded" else None, full=full)
    if (
        selected
        and report["installation"] == "compatible"
        and (report["version"] != selected["version"] or report["sha256"] != selected["sha256"])
    ):
        report["installation"] = "incompatible"
    return report
