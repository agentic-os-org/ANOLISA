"""Run the ASC Compose contract on a disposable Linux Docker host, without a model."""

import argparse
import asyncio
import hashlib
import importlib.metadata
import json
import os
import re
import selectors
import shlex
import socket
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import Any

import harbor
from agent_sec_skill_score_runtime.egress import POLICY_PATH, prepare_egress
from harbor.environments.factory import EnvironmentFactory
from harbor.models.task.config import EnvironmentConfig as TaskEnvironmentConfig
from harbor.models.task.config import NetworkMode, NetworkPolicy
from harbor.models.trial.config import (
    EnvironmentConfig as TrialEnvironmentConfig,
)
from harbor.models.trial.paths import TrialPaths

ALLOW_HOST = "example.com"
DENY_HOST = "www.iana.org"


def command(arguments: list[str]) -> str:
    return subprocess.run(
        arguments, check=True, capture_output=True, text=True, timeout=30
    ).stdout


def package_hash() -> str:
    digest = hashlib.sha256()
    root = Path(harbor.__file__).parent
    for path in sorted(root.rglob("*")):
        if path.is_file() and "__pycache__" not in path.parts:
            digest.update(
                str(path.relative_to(root)).encode() + b"\0" + path.read_bytes()
            )
    return digest.hexdigest()


def objects(project: str, *, running: bool = False, service: str = "") -> list[str]:
    args = ["docker", "ps", "--filter", f"label=com.docker.compose.project={project}"]
    if not running:
        args.append("--all")
    if service:
        args.extend(["--filter", f"label=com.docker.compose.service={service}"])
    return command([*args, "--format", "{{.ID}}"]).split()


def diagnostic_command(arguments: list[str]) -> dict[str, Any]:
    """Bound diagnostics without allowing a failed probe to prevent cleanup."""
    data = bytearray()
    limit = 64 * 1024
    process = subprocess.Popen(
        arguments, stdout=subprocess.PIPE, stderr=subprocess.STDOUT
    )
    deadline = time.monotonic() + 10
    try:
        with selectors.DefaultSelector() as selected:
            if process.stdout is None:
                raise RuntimeError("diagnostic output pipe unavailable")
            selected.register(process.stdout, selectors.EVENT_READ)
            while time.monotonic() < deadline and len(data) <= limit:
                if not selected.select(timeout=max(0, deadline - time.monotonic())):
                    break
                chunk = process.stdout.read1(min(8192, limit + 1 - len(data)))
                if not chunk:
                    break
                data.extend(chunk)
        if process.poll() is None:
            process.kill()
        code = process.wait(timeout=2)
        return {
            "return_code": code,
            "output_limited": len(data) > limit,
            "timed_out": time.monotonic() >= deadline,
            "output": bytes(data[:limit]).decode("utf-8", errors="replace"),
        }
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=2)
        if process.stdout is not None:
            process.stdout.close()


def diagnostics(output: Path, project: str) -> None:
    """Capture only this test's containers; omit environment and credentials."""
    captured: dict[str, Any] = {"project": project, "containers": {}}
    try:
        for identifier in objects(project):
            fields = (
                '{"id":{{json .Id}},"name":{{json .Name}},'
                '"state":{{json .State}},"hosts_path":{{json .HostsPath}},'
                '"resolv_path":{{json .ResolvConfPath}},'
                '"network_mode":{{json .HostConfig.NetworkMode}},'
                '"networks":{{json .NetworkSettings.Networks}},'
                '"mounts":{{json .Mounts}}}'
            )
            item = {
                "inspect": diagnostic_command(
                    ["docker", "inspect", "--format", fields, identifier]
                ),
                "logs": diagnostic_command(
                    ["docker", "logs", "--tail", "100", identifier]
                ),
                "name_resolution": diagnostic_command(
                    [
                        "docker",
                        "exec",
                        identifier,
                        "sh",
                        "-c",
                        "cat /etc/hosts /etc/resolv.conf",
                    ]
                ),
            }
            if identifier in objects(project, service="asc-egress"):
                item["firewall"] = diagnostic_command(
                    [
                        "docker",
                        "exec",
                        identifier,
                        "sh",
                        "-c",
                        "iptables-nft-save -c; ip6tables-nft-save -c; cat /etc/asc-egress/policy.json",
                    ]
                )
            captured["containers"][identifier] = item
    except Exception as exc:
        captured["error"] = f"{type(exc).__name__}: {exc}"
    try:
        (output / "diagnostics.json").write_text(json.dumps(captured, indent=2) + "\n")
    except OSError as exc:
        print(f"Could not save diagnostics for {project}: {exc}", file=sys.stderr)


def curl(host: str, address: str = "") -> str:
    args = ["curl", "--noproxy", "*", "--ipv4", "-ksSf", "--max-time", "7"]
    if address:
        args.extend(["--resolve", f"{host}:443:{address}"])
    return shlex.join([*args, f"https://{host}/"])


def environment(
    root: Path, project: str, image: str, address: str, failing: bool
) -> Any:
    root.mkdir()
    prepare_egress(root / "task", ALLOW_HOST, [address])
    compose_path = root / "task/environment/docker-compose.yaml"
    compose = json.loads(compose_path.read_text())
    if failing:
        compose["services"]["asc-egress"]["entrypoint"] = ["/bin/sh", "-c", "exit 42"]
    compose_path.write_text(json.dumps(compose))
    policy_path = root / "egress-policy.json"
    policy = json.loads(policy_path.read_text())
    policy["log"] = {"level": "debug"}
    policy_path.write_text(json.dumps(policy))
    paths = TrialPaths(trial_dir=root / "trial")
    paths.mkdir()
    public = NetworkPolicy(network_mode=NetworkMode.PUBLIC)
    return EnvironmentFactory.create_environment_from_config(
        config=TrialEnvironmentConfig(type="docker", delete=True),
        environment_dir=root / "task/environment",
        environment_name=project,
        session_id=project,
        trial_paths=paths,
        task_env_config=TaskEnvironmentConfig(
            docker_image=image,
            network_mode=NetworkMode.PUBLIC,
            workdir="/",
            cpus=1,
            memory_mb=512,
        ),
        network_policy=public,
        phase_network_policies=[public],
    )


async def exercise(args: argparse.Namespace, report: dict[str, Any]) -> None:
    addresses = {
        host: sorted(
            {row[4][0] for row in socket.getaddrinfo(host, 443, socket.AF_INET)}
        )[0]
        for host in (ALLOW_HOST, DENY_HOST)
    }
    report["resolved_ipv4"] = addresses
    for host, address in addresses.items():
        command(shlex.split(curl(host, address)))
    project = "asc-egress-" + uuid.uuid4().hex[:12]
    normal = environment(
        args.output / "normal", project, args.image, addresses[ALLOW_HOST], False
    )

    async def check(
        name: str, text: str, *, success: bool = True, service: str = ""
    ) -> str:
        result = (
            await normal.service_exec(text, service=service, timeout_sec=20)
            if service
            else await normal.exec(text, timeout_sec=20)
        )
        report["checks"][name] = {
            "return_code": result.return_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
        }
        if (result.return_code == 0) != success:
            raise AssertionError(f"{name}: unexpected return code {result.return_code}")
        return result.stdout or ""

    try:
        await asyncio.wait_for(normal.start(force_build=False), 90)
        assert not normal.capabilities.network_allowlist
        assert "Example Domain" in await check("allowed", curl(ALLOW_HOST))
        await check("denied", curl(DENY_HOST, addresses[DENY_HOST]), success=False)
        await check(
            "policy_read_only",
            f"printf 'tampered' >> {POLICY_PATH}",
            service="asc-egress",
            success=False,
        )
        await check("policy_not_mounted_in_main", f"test ! -e {POLICY_PATH}")
        await check(
            "socket_mark_denied",
            shlex.join(
                [
                    "python",
                    "-c",
                    "import socket\ns=socket.socket()\ntry: s.setsockopt(socket.SOL_SOCKET,36,114514)\nexcept PermissionError: pass\nelse: raise AssertionError('main can set SO_MARK')",
                ]
            ),
        )
        await check(
            "udp_denied",
            shlex.join(
                [
                    "python",
                    "-c",
                    "import socket\ns=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)\ntry: s.sendto(b'ASC synthetic egress probe',('1.1.1.1',9))\nexcept PermissionError: pass\nelse: raise AssertionError('outside UDP escaped')",
                ]
            ),
        )
        await check(
            "local_source_port",
            shlex.join(
                [
                    "python",
                    "-c",
                    "import socket\ns=socket.socket();s.bind(('127.0.0.1',0));s.listen()\nc=socket.socket();c.bind(('127.0.0.1',41023));c.connect(s.getsockname())\np,peer=s.accept();assert peer==('127.0.0.1',41023);print(peer)\np.close();c.close();s.close()",
                ]
            ),
        )
        upload = args.output / "upload.txt"
        upload.write_text("ASC public file transfer\n")
        await normal.upload_file(upload, "/run/asc-transfer.txt")
        await normal.download_file(
            "/run/asc-transfer.txt", args.output / "download.txt"
        )
        assert (args.output / "download.txt").read_bytes() == upload.read_bytes()
        report["checks"]["file_transfer"] = True

        # Docker's namespace-sharing HostsPath is writable from main. The
        # independent mapper must keep both lower-case and mixed-case requests safe.
        poison = f"{addresses[DENY_HOST]} {ALLOW_HOST}\n"
        await check(
            "poison_shared_hosts",
            shlex.join(
                [
                    "python",
                    "-c",
                    f"from pathlib import Path; Path('/etc/hosts').write_text({poison!r})",
                ]
            ),
        )
        shared = await check(
            "sidecar_observes_poison", "cat /etc/hosts", service="asc-egress"
        )
        assert poison in shared
        assert "Example Domain" in await check(
            "mapper_ignores_shared_hosts", curl(ALLOW_HOST)
        )
        await check(
            "mixed_case_mapping_miss_denied",
            shlex.join(
                [
                    "python",
                    "-c",
                    f"import socket,ssl\nc=ssl._create_unverified_context()\ns=socket.create_connection(({addresses[DENY_HOST]!r},443),timeout=7)\nc.wrap_socket(s,server_hostname='ExAmPlE.CoM')",
                ]
            ),
            success=False,
        )
        sidecar_ids = objects(project, running=True, service="asc-egress")
        assert len(sidecar_ids) == 1
        # Docker logs may be on stderr; preserve both for the resolver oracle.
        result = subprocess.run(
            ["docker", "logs", sidecar_ids[0]],
            check=True,
            capture_output=True,
            text=True,
            timeout=20,
        )
        logs = result.stdout + result.stderr
        (args.output / "sidecar.log").write_text(logs)
        assert f"hit host mapper: {ALLOW_HOST}" in logs
        assert "resolver: domain ExAmPlE.CoM does not exist" in logs
        await check(
            "pause_proxy",
            "kill -STOP $(cat /run/asc-egress.gost.pid)",
            service="asc-egress",
        )
        await check("paused_proxy_denied", curl(ALLOW_HOST), success=False)
        await check(
            "resume_proxy",
            "kill -CONT $(cat /run/asc-egress.gost.pid)",
            service="asc-egress",
        )
        assert "Example Domain" in await check("resumed_proxy", curl(ALLOW_HOST))
        command(["docker", "stop", "--time", "2", sidecar_ids[0]])
        assert len(objects(project, running=True, service="main")) == 1
        await check("exited_proxy_denied", curl(ALLOW_HOST), success=False)
    finally:
        diagnostics(args.output / "normal", project)
        await normal.stop(delete=True)
        assert not objects(project)
        assert not command(
            [
                "docker",
                "network",
                "ls",
                "--filter",
                f"label=com.docker.compose.project={project}",
                "--format",
                "{{.ID}}",
            ]
        ).strip()
        report["checks"]["normal_cleanup"] = True

    failed_project = project + "-failed"
    failed = environment(
        args.output / "failed", failed_project, args.image, addresses[ALLOW_HOST], True
    )
    try:
        try:
            await asyncio.wait_for(failed.start(force_build=False), 45)
        except Exception as exc:
            report["checks"]["startup_rejected"] = type(exc).__name__
        else:
            raise AssertionError("sidecar failure did not prevent startup")
        assert not objects(failed_project, running=True, service="main")
        ids = objects(failed_project, service="asc-egress")
        assert len(ids) == 1
        assert (
            command(
                ["docker", "inspect", "--format", "{{.State.ExitCode}}", ids[0]]
            ).strip()
            == "42"
        )
    finally:
        diagnostics(args.output / "failed", failed_project)
        await failed.stop(delete=True)
        assert not objects(failed_project)
        assert not command(
            [
                "docker",
                "network",
                "ls",
                "--filter",
                f"label=com.docker.compose.project={failed_project}",
                "--format",
                "{{.ID}}",
            ]
        ).strip()
        report["checks"]["failure_cleanup"] = True


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--egress-image", required=True)
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("run this integration test on Linux")
    if importlib.metadata.version("harbor") != "0.22.0":
        parser.error("this contract targets Harbor 0.22.0")
    for identity in (args.image, args.egress_image):
        if not re.fullmatch(r"sha256:[a-f0-9]{64}", identity):
            parser.error("pass both immutable image IDs from runtime-manifest.json")
    os.environ["ASC_SKILL_SCORE_EGRESS_IMAGE"] = args.egress_image
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    before = package_hash()
    report: dict[str, Any] = {"status": "ERROR", "checks": {}, "harbor_before": before}
    try:
        asyncio.run(exercise(args, report))
        report["status"] = "PASS"
    except Exception as exc:
        report["error"] = f"{type(exc).__name__}: {exc}"
        raise
    finally:
        report["harbor_after"] = package_hash()
        report["harbor_unchanged"] = report["harbor_after"] == before
        if not report["harbor_unchanged"]:
            report["status"] = "FAIL"
        (args.output / "result.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps({"status": report["status"], "output": str(args.output)}))
    if report["status"] != "PASS":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
