#!/usr/bin/env python3
"""Install a verified, prebuilt candidate into the current Omarchy user's home."""

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid
from collections.abc import Callable
from pathlib import Path
from types import ModuleType

MARKER = ".anolisa-omarchy-owned"
PLUGIN_ID = "community.anolisa"
# Assembly replaces this value with hashes from the verified build inputs.
# Never take privileged verification values from the mutable bundle manifest.
SYSTEM_INPUT_DIGESTS = {}
INSTALL_MODULE_DIGESTS = {}

# sudo receives this code directly, not a script path in the user's home.
# Only the checked, root-owned helper is executed, with user imports disabled.
SYSTEM_BOOTSTRAP = r"""
import hashlib, os, re, stat, sys, tempfile
from pathlib import Path

expected = sys.argv[1]
if os.geteuid() != 0 or not re.fullmatch(r"[a-f0-9]{64}", expected):
    raise ValueError("Invalid privileged helper request")
body = sys.stdin.buffer.read(1024 * 1024 + 1)
if len(body) > 1024 * 1024 or hashlib.sha256(body).hexdigest() != expected:
    raise ValueError("Privileged helper checksum mismatch")
parent = Path("/opt/anolisa-omarchy/sec-core/installers")
for directory in [*reversed(parent.parents), parent]:
    directory.mkdir(mode=0o755, exist_ok=True)
    info = directory.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
        raise ValueError("Unsafe privileged helper directory: " + str(directory))
target = parent / (expected + ".py")
if not target.exists() and not target.is_symlink():
    fd, name = tempfile.mkstemp(prefix=".helper-", dir=parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(body)
            stream.flush()
            os.fsync(stream.fileno())
            os.fchmod(stream.fileno(), 0o444)
        os.replace(name, target)
    finally:
        Path(name).unlink(missing_ok=True)
fd = os.open(target, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
with os.fdopen(fd, "rb") as stream:
    info = os.fstat(stream.fileno())
    if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
        raise ValueError("Unsafe installed privileged helper")
    if hashlib.file_digest(stream, "sha256").hexdigest() != expected:
        raise ValueError("Installed privileged helper was modified")
os.execv("/usr/bin/python3", ["/usr/bin/python3", "-I", "-B", str(target), *sys.argv[2:]])
"""


def digest(filename: Path) -> str:
    with filename.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def verify(bundle: Path) -> dict:
    """Check the complete payload before any installed file is changed."""
    manifest = json.loads((bundle / "bundle.json").read_text())
    if manifest.get("schema") != 1 or manifest.get("platform") != "linux-x86_64":
        raise ValueError("Unsupported candidate format or architecture")
    expected = manifest["files"]
    actual = set()
    for filename in (bundle / "payload").rglob("*"):
        if filename.is_symlink():
            raise ValueError("Candidate payload must not contain symlinks")
        if filename.is_file():
            relative = filename.relative_to(bundle / "payload").as_posix()
            actual.add(relative)
            if relative not in expected or digest(filename) != expected[relative]:
                raise ValueError(f"Payload checksum mismatch: {relative}")
    if actual != set(expected):
        raise ValueError("Candidate payload is incomplete")
    return manifest


def require_owned(directory: Path) -> None:
    if directory.is_symlink() or (directory.exists() and not (directory / MARKER).is_file()):
        raise ValueError(f"Refusing to overwrite an unowned directory: {directory}")


def verified_runtime(bundle: Path) -> ModuleType:
    """Verify and compile every caller module before executing any of its code.

    Root isolation is insufficient if a replaced module can alter the caller
    before sudo. Build-time pins also cover this import seam.
    """
    names = ("i18n", "setup", "agent_installation", "runtime")
    if set(INSTALL_MODULE_DIGESTS) != {name + ".py" for name in names}:
        raise ValueError("Use a prebuilt installer with pinned Python modules")
    sources = {}
    for name in names:
        path = bundle / "payload" / (name + ".py")
        body = path.read_bytes()
        if hashlib.sha256(body).hexdigest() != INSTALL_MODULE_DIGESTS[name + ".py"]:
            raise ValueError(f"Installer module checksum mismatch: {name}")
        sources[name] = compile(body, str(path), "exec")
    # Dependencies load in order using only the already verified code objects.
    for name in names:
        module = ModuleType(name)
        module.__file__ = str(bundle / "payload" / (name + ".py"))
        sys.modules[name] = module
        exec(sources[name], module.__dict__)
    return sys.modules["runtime"]


def stage_release(bundle: Path, data: Path, metadata: dict) -> tuple[Path, str]:
    fingerprint = digest(bundle / "bundle.json")[:16]
    release = data / "releases" / fingerprint
    release.parent.mkdir(exist_ok=True)
    if not release.exists():
        with tempfile.TemporaryDirectory(prefix=".stage-", dir=release.parent) as temporary:
            stage = Path(temporary) / "payload"
            shutil.copytree(bundle / "payload", stage)
            shutil.copy2(bundle / "bundle.json", stage / "bundle.json")
            (stage / MARKER).write_text(fingerprint + "\n")
            for filename in stage.rglob("*"):
                if filename.is_file():
                    executable = filename.parent.name == "bin" or filename.name in (
                        "cosh-shell",
                        "cosh-core",
                        "cosh-gateway",
                    )
                    filename.chmod(0o755 if executable else 0o644)
            stage.rename(release)
    else:
        require_owned(release)
        for relative, checksum in metadata["files"].items():
            filename = release / relative
            if not filename.is_file() or filename.is_symlink() or digest(filename) != checksum:
                raise ValueError(f"Installed payload was modified: {relative}")
    return release, fingerprint


def required_libraries(release: Path) -> set[str]:
    missing = set()
    for candidate in [
        *(release / "bin").iterdir(),
        *(release / "libexec/anolisa/cosh-ng").iterdir(),
    ]:
        if not candidate.is_file():
            continue
        with candidate.open("rb") as stream:
            if stream.read(4) != b"\x7fELF":
                continue
        result = subprocess.run(
            ["ldd", str(candidate)],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
            env=dict(os.environ, LC_ALL="C"),
        )
        output = result.stdout + result.stderr
        missing.update(re.findall(r"(?m)^\s*(\S+)\s+=>\s+not found", output))
        if "version " in output and "not found" in output:
            raise ValueError(f"ABI mismatch for {candidate.name}: {output.strip()}")
        if result.returncode and "statically linked" not in output:
            raise ValueError(f"Could not inspect {candidate.name}: {output.strip()}")
    return missing


def dependencies(release: Path, sudo: list[str]) -> list[str]:
    packages = set()
    missing = required_libraries(release)
    if missing and not shutil.which("pacman"):
        raise ValueError("Missing runtime libraries: " + ", ".join(sorted(missing)))
    for library in sorted(missing):
        result = subprocess.run(
            ["pacman", "-Fq", f"usr/lib/{library}"],
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
        matches = set(result.stdout.split()) if result.returncode == 0 else set()
        # pacman -Fq returns repository-qualified names, including for one match.
        qualified = r"[a-zA-Z0-9@._+-]+/[a-zA-Z0-9@._+-]+"
        if len(matches) != 1 or not re.fullmatch(qualified, next(iter(matches))):
            raise ValueError(
                f"No unique pacman package confirmed for {library}. "
                "Refresh the file database with sudo pacman -Fy and retry."
            )
        packages.update(matches)
    if packages:
        print("Installing missing runtime dependencies: " + " ".join(sorted(packages)), flush=True)
        command = [*sudo, "pacman", "-S", "--needed"]
        if "-n" in sudo:
            command.append("--noconfirm")
        subprocess.run([*command, "--", *sorted(packages)], check=True)
        if required_libraries(release):
            raise ValueError("Some runtime libraries are still missing after installation.")
    for name in ("aw", "cosh", "tokenless", "agent-sec-cli"):
        result = subprocess.run(
            [str(release / "bin" / name), "--version"],
            timeout=10,
            capture_output=True,
            check=False,
        )
        if result.returncode:
            raise ValueError(f"Runtime check failed: {name} --version")
    return sorted(packages)


def launcher_bytes(current: Path, *, legacy: bool = False) -> bytes:
    python_flags = "" if legacy else '"-B", '
    return (
        "#!/usr/bin/env python3\n# ANOLISA Omarchy managed launcher\n"
        "import os, sys\n"
        f"runtime = {str(current / 'runtime.py')!r}\n"
        f"os.execv(sys.executable, [sys.executable, {python_flags}runtime, *sys.argv[1:]])\n"
    ).encode()


def replace_link(path: Path, destination: Path) -> None:
    with tempfile.TemporaryDirectory(prefix=".link-", dir=path.parent) as temporary:
        link = Path(temporary) / "current"
        link.symlink_to(destination)
        link.replace(path)


def plugin_checksums(directory: Path) -> dict[str, str]:
    """Compare widget contents without following links or including its owner marker."""
    if directory.is_symlink():
        raise ValueError("Plugin directory must not be a link")
    files = {}
    for path in directory.rglob("*"):
        if path.is_symlink():
            raise ValueError("Plugin must not contain links")
        if path.is_file() and path.name != MARKER:
            files[path.relative_to(directory).as_posix()] = digest(path)
    return files


def previous_plugin_checksums(plugin: Path, current: Path) -> dict[str, str]:
    """Use the widget's recorded release only for explicit interrupted-upgrade recovery."""
    marker = plugin / MARKER
    if marker.is_symlink():
        raise ValueError("Plugin owner marker must not be a link")
    fingerprint = marker.read_text().strip()
    if not re.fullmatch(r"[0-9a-f]{16}", fingerprint):
        raise ValueError("Invalid plugin release marker")
    releases = current.parent / "releases"
    previous = releases / fingerprint
    require_owned(previous)
    manifest = previous / "bundle.json"
    owner = previous / MARKER
    if (
        releases.is_symlink()
        or manifest.is_symlink()
        or owner.is_symlink()
        or owner.read_text().strip() != fingerprint
        or digest(manifest)[:16] != fingerprint
    ):
        raise ValueError("Invalid previous release receipt")
    metadata = json.loads(manifest.read_text())
    expected = {
        name.removeprefix("plugin/"): checksum
        for name, checksum in metadata["files"].items()
        if name.startswith("plugin/")
    }
    actual = plugin_checksums(previous / "plugin")
    if not expected or actual != expected:
        raise ValueError("Previous plugin payload was modified")
    return actual


def verify_installed_plugin(plugin: Path, current: Path, *, recover: bool = False) -> None:
    require_owned(plugin)
    if not plugin.exists():
        return
    try:
        actual = plugin_checksums(plugin)
        if actual == plugin_checksums(current / "plugin"):
            return
        # current may already point to B while the unchanged widget still records A.
        if recover and actual == previous_plugin_checksums(plugin, current):
            return
    except (OSError, ValueError, KeyError, TypeError):
        pass
    raise ValueError(f"Existing plugin differs; keep or back up your changes first: {plugin}")


def install(
    bundle: Path, *, enable: bool, non_interactive: bool = False, recover: bool = False
) -> None:
    if platform.system() != "Linux" or platform.machine() != "x86_64" or sys.version_info < (3, 11):
        raise ValueError("This candidate requires Linux x86_64 and Python 3.11 or newer")
    if os.getuid() == 0:
        raise ValueError("Run as the desktop user, not with sudo")
    if recover and not enable:
        raise ValueError("--recover requires system-service activation; omit --no-enable")
    metadata = verify(bundle)
    if enable:
        for executable in (
            "omarchy",
            "omarchy-shell",
            "omarchy-launch-terminal",
            "systemctl",
            "sudo",
            "ldd",
        ):
            if not shutil.which(executable):
                raise ValueError(f"Required Omarchy executable is missing: {executable}")
    if enable or INSTALL_MODULE_DIGESTS:
        runtime = verified_runtime(bundle)
    else:
        # Unbuilt source fixtures may exercise user-only deployment, never sudo.
        import runtime
    setup = runtime.setup

    home = Path.home()
    data = Path(os.environ.get("XDG_DATA_HOME", home / ".local/share")) / "anolisa-omarchy"
    require_owned(data)
    setup.private_directory(data)
    (data / MARKER).write_text("ANOLISA Omarchy integration\n")
    current = data / "current"
    command = home / ".local/bin/anolisa-omarchy"
    # Omarchy's registry and removal command use HOME, not XDG_CONFIG_HOME.
    plugin = home / ".config/omarchy/plugins" / PLUGIN_ID
    paths = dict(runtime.locations(), prefix=current)
    sudo = ["sudo", "-n"] if non_interactive else ["sudo"]
    with setup.lock(paths["run"]):
        if current.exists() or current.is_symlink():
            if not current.is_symlink() or not current.resolve().is_relative_to(data / "releases"):
                raise ValueError(f"Expected an owned release symlink: {current}")
            require_owned(current.resolve())
        verify_installed_plugin(plugin, current, recover=recover)
        launcher = launcher_bytes(current)
        if command.is_symlink() or (
            command.exists()
            and command.read_bytes() not in (launcher, launcher_bytes(current, legacy=True))
        ):
            raise ValueError(f"Refusing to replace an existing command: {command}")
        release, fingerprint = stage_release(bundle, data, metadata)
        for name in ("config", "state"):
            setup.private_directory(paths[name])
        try:
            plan = runtime.configuration_plan(paths, release, recover=recover)
        except ValueError as error:
            if "configuration differs" in str(error):
                proposal = paths["state"] / "aw.proposed.json"
                setup.atomic_write(proposal, runtime.config_bytes(paths))
                raise ValueError(f"{error} Candidate for comparison: {proposal}") from None
            raise
        command.parent.mkdir(parents=True, exist_ok=True)
        plugin.parent.mkdir(parents=True, exist_ok=True)
        activate(
            runtime,
            metadata,
            release,
            fingerprint,
            current,
            command,
            plugin,
            plan,
            paths,
            sudo,
            enable,
            recover,
        )
    print(
        f"Installed {metadata['version']}\nCommand: {command}\nSource: {metadata['source_commit']}"
    )
    print(f"ANOLISA - AI Assistant {metadata['version']} is installed.")
    print("Open ANOLISA in the top bar. Configure accounts in first-run setup or Settings.")


def activate(
    runtime: ModuleType,
    metadata: dict,
    release: Path,
    fingerprint: str,
    current: Path,
    command: Path,
    plugin: Path,
    plan: dict[Path, bytes],
    paths: dict[str, Path],
    sudo: list[str],
    enable: bool,
    recover: bool = False,
) -> None:
    setup = runtime.setup

    old_current = current.readlink() if current.is_symlink() else None
    old_command = command.read_bytes() if command.exists() else None
    old_config = {path: setup.private_read(path) for path in plan}
    transaction = uuid.uuid4().hex
    helper = b""
    if enable:
        # Capture both pins before sudo; later phases never reread a user manifest.
        pins = dict(SYSTEM_INPUT_DIGESTS)
        if set(pins) != {"system_install.py", "bin/agent-sec-daemon"} or any(
            not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value)
            for value in pins.values()
        ):
            raise ValueError("Use a prebuilt installer with pinned privileged inputs")
        helper = (release / "system_install.py").read_bytes()
        if hashlib.sha256(helper).hexdigest() != pins["system_install.py"]:
            raise ValueError("Privileged helper checksum mismatch")
        root_command = [
            *sudo,
            "/usr/bin/python3",
            "-I",
            "-B",
            "-c",
            SYSTEM_BOOTSTRAP,
            pins["system_install.py"],
        ]

    def root_action(action: str, *arguments: str) -> None:
        subprocess.run([*root_command, action, *arguments], input=helper, check=True)

    service_started = False
    app_changed = False
    plugin_published = False
    old_plugin_saved = False
    shell_changed = False
    packages = []
    shell_config = plugin.parent.parent / "shell.json"
    old_shell = None
    shell_env = dict(os.environ)
    # The desktop may spend several seconds reloading QML after publishing the plugin.
    shell_env.setdefault("OMARCHY_SHELL_IPC_TIMEOUT", "10s")
    phase = "stage"
    receipt = paths["state"] / "installation.json"
    with tempfile.TemporaryDirectory(prefix=".anolisa-transaction-", dir=plugin.parent) as temp:
        work = Path(temp)
        staged_plugin = work / PLUGIN_ID
        backup_plugin = work / "previous"
        shutil.copytree(release / "plugin", staged_plugin)
        (staged_plugin / MARKER).write_text(fingerprint + "\n")
        try:
            if enable:
                phase = "dependency_check"
                packages = dependencies(release, sudo)
                phase = "plugin_validation"
                subprocess.run(["omarchy", "plugin", "validate", str(staged_plugin)], check=True)
                phase = "security_service"
                print("Installing or checking the system safety service with sudo.", flush=True)
                if recover:
                    root_action("recover", transaction)
                root_action(
                    "prepare",
                    str(release / "bin/agent-sec-daemon"),
                    pins["bin/agent-sec-daemon"],
                    transaction,
                )
                service_started = True
                probe_paths = dict(paths, prefix=release)
                for _ in range(15):
                    if runtime.healthy(probe_paths):
                        break
                    time.sleep(0.2)
                else:
                    raise ValueError("Security service did not become ready.")
            phase = "activation"
            app_changed = True
            runtime.write_plan(plan)
            replace_link(current, release)
            setup.atomic_write(command, launcher_bytes(current), mode=0o755)
            if plugin.exists():
                plugin.rename(backup_plugin)
                old_plugin_saved = True
            staged_plugin.rename(plugin)
            plugin_published = True
            if enable:
                phase = "plugin_enable"
                if shell_config.is_symlink():
                    raise ValueError("Refusing to update a linked Omarchy shell configuration.")
                old_shell = shell_config.read_bytes() if shell_config.exists() else None
                run_checked(["omarchy-shell", "shell", "rescanPlugins"], shell_env)
                shell_changed = True
                wait_for_plugin(shell_env)
                args = [] if old_plugin_saved else ["--section", "right"]
                run_checked(["omarchy", "plugin", "enable", PLUGIN_ID, *args], shell_env)
            setup.atomic_write(
                receipt,
                setup.json_bytes(
                    {
                        "version": metadata["version"],
                        "result": "installed",
                        "previous_release": str(old_current) if old_current else None,
                        "release": str(release),
                        "packages": packages,
                        "desktop_enabled": enable,
                        "timestamp": runtime.timestamp(),
                    }
                ),
            )
            if service_started:
                root_action("commit", transaction)
        except BaseException:
            errors = []

            def restore(label: str, callback: Callable[..., object], *args: object) -> None:
                try:
                    callback(*args)
                except (OSError, ValueError, subprocess.SubprocessError) as error:
                    errors.append(f"{label}: {error}")

            def restore_file(path: Path, value: bytes | None, mode: int = 0o600) -> None:
                if value is None:
                    path.unlink(missing_ok=True)
                else:
                    setup.atomic_write(path, value, mode)

            if plugin_published:
                restore("remove new plugin", plugin.rename, work / "failed")
            if old_plugin_saved:
                restore("previous plugin", backup_plugin.rename, plugin)
            if app_changed:
                if old_current is None:
                    restore("current", restore_file, current, None)
                else:
                    restore("current", replace_link, current, old_current)
                restore("launcher", restore_file, command, old_command, 0o755)
                for path, value in old_config.items():
                    restore("configuration", restore_file, path, value)
            if shell_changed:
                restore("shell configuration", restore_file, shell_config, old_shell, 0o644)
            if enable and app_changed:
                restore(
                    "plugin registry",
                    run_checked,
                    ["omarchy-shell", "shell", "rescanPlugins"],
                    shell_env,
                )
            if service_started:
                restore("system safety service", root_action, "rollback", transaction)
            setup.atomic_write(
                receipt,
                setup.json_bytes(
                    {
                        "version": metadata["version"],
                        "result": "failed",
                        "phase": phase,
                        "packages": packages,
                        "rollback_errors": errors,
                        "transaction": transaction,
                        "timestamp": runtime.timestamp(),
                    }
                ),
            )
            if errors:
                print(f"Some rollback steps failed. See {receipt}", file=sys.stderr)
                if backup_plugin.exists():
                    backup_plugin.rename(plugin.parent / (".anolisa-previous-" + transaction))
            raise


def wait_for_plugin(env: dict[str, str]) -> None:
    # The IPC rescan returns before Quickshell publishes the new registry.
    deadline = time.monotonic() + 12
    while True:
        result = subprocess.run(
            ["omarchy-shell", "shell", "listPlugins"],
            check=True,
            capture_output=True,
            text=True,
            timeout=12,
            env=env,
        )
        plugins = json.loads(result.stdout)
        if not isinstance(plugins, list):
            raise ValueError("Omarchy returned an invalid plugin registry.")
        if any(isinstance(plugin, dict) and plugin.get("id") == PLUGIN_ID for plugin in plugins):
            return
        if time.monotonic() >= deadline:
            raise ValueError(
                "Omarchy did not register the plugin after scanning. Retry in the desktop session."
            )
        time.sleep(0.25)


def run_checked(argv: list[str], env: dict[str, str] | None = None) -> None:
    subprocess.run(argv, check=True, env=env)


def interrupted(_number: int, _frame: object) -> None:
    raise InterruptedError("Installation interrupted")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--no-enable",
        action="store_true",
        help="Deploy user files without system or desktop activation",
    )
    parser.add_argument(
        "--non-interactive", action="store_true", help="Fail if sudo requires interaction"
    )
    parser.add_argument(
        "--recover",
        action="store_true",
        help="Restore an interrupted system-service transaction before installing; stop other installers first",
    )
    options = parser.parse_args()
    signal.signal(signal.SIGTERM, interrupted)
    install(
        Path(__file__).resolve().parent,
        enable=not options.no_enable,
        non_interactive=options.non_interactive,
        recover=options.recover,
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        print(f"Installation stopped: {error}", file=sys.stderr)
        raise SystemExit(1) from None
