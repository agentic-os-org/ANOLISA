#!/usr/bin/env python3
"""Run seed regressions; optionally exercise real Ledger, FUSE and Cosh on Linux.

Use the agent-sec Python environment. --integration requires skillfs and cosh-core
on PATH and a private container with /dev/fuse. No scanner or activation stubs.
"""

import argparse
import errno
import importlib.util
import json
import os
import shutil
import subprocess
import tempfile
import time
import traceback
from pathlib import Path
from typing import Callable

SCRIPT = Path(__file__).resolve().parents[1] / "container/ledger-init.py"
spec = importlib.util.spec_from_file_location("ledger_init", SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def rejected(call: Callable[[], object]) -> None:
    try:
        call()
    except ValueError:
        return
    raise AssertionError("unsafe seed accepted")


def check_seed(root: Path) -> None:
    package = root / "package"
    skill = package / "sample"
    skill.mkdir(parents=True)
    (skill / "SKILL.md").write_text("---\nname: sample\ndescription: Sample\n---\nHello.\n")
    (skill / "run.sh").write_text("#!/bin/sh\nprintf 'hello\\n'\n")
    (skill / "run.sh").chmod(0o555)
    (skill / "SKILL.md").chmod(0o444)
    skill.chmod(0o555)
    source = root / "source"
    copied = module.seed(package, source)[0]
    assert source.stat().st_mode & 0o777 == 0o700
    assert (copied / "run.sh").stat().st_mode & 0o777 == 0o755
    assert (copied / "SKILL.md").stat().st_mode & 0o777 == 0o644
    meta = copied / ".skill-meta"
    meta.mkdir()
    (meta / "keep").write_text("existing activation state")
    module.seed(package, source)
    assert (meta / "keep").read_text() == "existing activation state"
    skill.chmod(0o755)
    (skill / "SKILL.md").chmod(0o644)
    (skill / "SKILL.md").write_text("changed package")
    rejected(lambda: module.seed(package, source))
    assert (meta / "keep").read_text() == "existing activation state"
    (skill / "outside").symlink_to("/etc/passwd")
    rejected(lambda: module.seed(package, root / "linked"))
    (skill / "outside").unlink()
    (skill / ".skill-meta").mkdir()
    rejected(lambda: module.seed(package, root / "metadata"))
    rejected(lambda: module.seed(package, package / "nested"))
    print("PASS: read-only seed, modes, restart preservation, upgrade/link/metadata rejection")


def eventually(predicate: Callable[[], bool]) -> None:
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.2)
    raise AssertionError("timed out waiting for mount/activation")


def integration(root: Path) -> None:
    from agent_sec_cli.skill_ledger.core.resolver import write_activation_contract

    for name in ("skillfs", "cosh-core", "fusermount3", "timeout"):
        assert shutil.which(name), f"missing {name}"
    assert Path("/dev/fuse").exists()
    assert (
        os.geteuid() == 0 and Path("/.dockerenv").exists()
    ), "use a disposable privileged container"
    root.chmod(0o755)
    for variable, directory in (
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_RUNTIME_DIR", "run"),
    ):
        os.environ[variable] = str(root / directory)
        (root / directory).mkdir(mode=0o700)
    package = root / "bundle"
    for name in ("accepted", "missing", "invalid"):
        skill = package / name
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text(
            f"---\nname: {name}\ndescription: A greeting\n---\nSay hello to the user.\n"
        )
        (skill / "SKILL.md").chmod(0o444)
        skill.chmod(0o555)
    package.chmod(0o755)
    source = root / "live"
    raw = Path("/usr/share/anolisa/skills")
    raw.mkdir(parents=True, exist_ok=True)
    raw.parent.chmod(0o755)
    assert not list(raw.iterdir()), "use an image without installed skills"
    subprocess.run(["mount", "--bind", str(package), str(raw)], check=True)
    subprocess.run(["mount", "-o", "remount,bind,ro", str(raw)], check=True)
    try:
        (raw / "accepted/SKILL.md").write_text("must fail even as root")
    except OSError as error:
        assert error.errno == errno.EROFS, error
    else:
        raise AssertionError("package is not mounted read-only")
    skills = module.seed(raw, source)
    module.activate(skills)
    accepted = source / "accepted"
    record = json.loads((accepted / ".skill-meta/activation.json").read_text())
    assert record["target"] and "pending" not in record["target"], record
    assert (accepted / record["target"] / "SKILL.md").is_file()
    before = (accepted / ".skill-meta/activation.json").read_bytes()
    module.seed(package, source)
    assert (accepted / ".skill-meta/activation.json").read_bytes() == before
    # Remove both activation sources: file mode intentionally prefers xattr.
    missing = source / "missing"
    (missing / ".skill-meta/activation.json").unlink()
    for skill in (missing, source / "invalid"):
        if "user.agent_sec.skill_ledger.activation" in os.listxattr(skill):
            os.removexattr(skill, "user.agent_sec.skill_ledger.activation")
    (source / "invalid/.skill-meta/activation.json").write_text("{invalid")
    mount = root / "mount"
    mount.mkdir()
    view = mount / "skills"
    home_skills = root / "home/.copilot-shell/skills"
    home_skills.parent.mkdir()
    home_skills.symlink_to(view)
    for path in [root / "home", home_skills.parent]:
        path.chmod(0o755)
        os.chown(path, 10001, 10001)

    def discovered() -> set[str]:
        request = {
            "type": "registry_request",
            "request_id": "seed-test",
            "domain": "skills",
            "action": "list",
            "params": None,
        }
        output = subprocess.run(
            ["cosh-core", "--registry"],
            input=json.dumps(request) + "\n",
            text=True,
            capture_output=True,
            timeout=30,
            check=True,
            cwd=root,
            user=10001,
            group=10001,
            extra_groups=[],
        ).stdout
        response = next(
            json.loads(line)
            for line in output.splitlines()
            if json.loads(line).get("type") == "registry_response"
        )
        assert response["success"], response
        return {s["name"] for s in response["data"]}

    with (root / "fuse.log").open("w") as log:
        process = subprocess.Popen(
            [
                "skillfs",
                "mount",
                str(source),
                str(mount),
                "--foreground",
                "--allow-other",
                "--read-only",
                "--security",
                "--activation-mode",
                "file",
                "--activation-reload-mode",
                "poll",
                "--activation-events-log",
                str(root / "events.jsonl"),
                "--skill-discover-root",
                str(view),
            ],
            stdout=log,
            stderr=log,
        )
        try:
            eventually(lambda: (view / "accepted/SKILL.md").is_file())
            assert "Say hello" in (view / "accepted/SKILL.md").read_text()
            assert not (view / "missing").exists()
            assert not (view / "invalid").exists()
            raw_names = discovered()
            assert {"missing", "invalid"} <= raw_names, raw_names
            masked = root / "masked"
            masked.mkdir(mode=0o755)
            subprocess.run(["mount", "--bind", str(masked), str(raw)], check=True)
            names = discovered()
            assert "accepted" in names and not names & {"missing", "invalid"}, names
            # Check the actual FUSE mount, not the read-only flag of its parent bind.
            for uid in (0, 10001):
                child = os.fork()
                if child == 0:
                    try:
                        os.setgroups([])
                        os.setgid(uid)
                        os.setuid(uid)
                        skill = view / "accepted"
                        file = skill / "SKILL.md"
                        for action in (
                            lambda: os.open(file, os.O_WRONLY),
                            lambda: os.truncate(file, 0),
                            lambda: (skill / "injected").write_text("injected"),
                            lambda: (skill / "newdir").mkdir(),
                            lambda: file.unlink(),
                            lambda: file.rename(skill / "renamed"),
                            lambda: file.chmod(0o777),
                            lambda: os.setxattr(file, "user.test", b"injected"),
                        ):
                            try:
                                action()
                            except OSError as error:
                                assert error.errno == errno.EROFS, error
                            else:
                                raise AssertionError("FUSE mutation succeeded")
                    except BaseException:
                        traceback.print_exc()
                        os._exit(1)
                    os._exit(0)
                _, status = os.waitpid(child, 0)
                assert os.waitstatus_to_exitcode(status) == 0, f"mutation check failed as UID {uid}"
            assert "Say hello" in (accepted / "SKILL.md").read_text()
            denied = subprocess.run(
                ["cat", str(accepted / "SKILL.md")],
                capture_output=True,
                user=10001,
                group=10001,
                extra_groups=[],
            )
            assert denied.returncode != 0, "agent can read the private source"
            write_activation_contract(accepted, {"schemaVersion": 1, "target": None})
            eventually(lambda: not (view / "accepted").exists())
            probe = SCRIPT.parent / "mount-probe.sh"
            subprocess.run(
                [
                    "bash",
                    str(probe),
                    "--mountpoint",
                    str(mount),
                    "--file",
                    "skills/skill-discover/SKILL.md",
                ],
                check=True,
                timeout=15,
            )
            assert process.poll() is None, "normal hiding must not stop the mount"
            print(
                "PASS: real Ledger snapshot, read-only FUSE, Cosh isolation, live revocation, probe"
            )
        finally:
            log.flush()
            if process.poll() is not None:
                print((root / "fuse.log").read_text())
            subprocess.run(["fusermount3", "-u", str(mount)], check=False, timeout=10)
            process.terminate()
            process.wait(timeout=10)
            subprocess.run(["umount", str(raw)], check=False, timeout=10)
            subprocess.run(["umount", str(raw)], check=False, timeout=10)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--integration", action="store_true")
    args = parser.parse_args()
    os.umask(0o077)
    with tempfile.TemporaryDirectory(
        prefix="skillfs-ledger-test-", dir="/var/lib" if args.integration else None
    ) as tmp:
        root = Path(tmp)
        check_seed(root)
        if args.integration:
            integration(root)
