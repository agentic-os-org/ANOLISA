"""Probe real SDK-owned processes against a loopback-only synthetic TLS model.

Run inside a disposable network namespace containing only loopback. SDK packages
and BPF dependencies must already be installed; this runner never downloads them.
"""

import argparse
import hashlib
from importlib.metadata import version
import json
import os
from pathlib import Path
import signal
import stat
import resource
import socket
import subprocess
import sys
import time

from correlation import process_ref
from run_case import ROOT, line, records, stop, wait_for
from transport_cases import build
from uprobe_counter import resolve

sys.path.insert(0, str(ROOT / "fixtures"))
from sdk_transport_server import ModelServer
from sdk_transport_auth import AuthProxy


def descendants(pid: int) -> set[int]:
    found = {pid}
    pending = [pid]
    while pending:
        parent = pending.pop()
        try:
            children = set()
            for task in Path(f"/proc/{parent}/task").iterdir():
                children.update(map(int, (task / "children").read_text().split()))
        except FileNotFoundError:
            continue
        for child in children - found:
            found.add(child)
            pending.append(child)
    return found


def owns_model_connection(pid: int, port: int, client_port: int | None = None) -> bool:
    """Match an owned socket FD to the model TCP connection before TLS resumes."""
    inodes = set()
    for descriptor in Path(f"/proc/{pid}/fd").iterdir():
        try:
            link = os.readlink(descriptor)
        except FileNotFoundError:
            continue
        if link.startswith("socket:["):
            inodes.add(link[8:-1])
    for family in ("tcp", "tcp6"):
        rows = Path(f"/proc/{pid}/net/{family}").read_text().splitlines()[1:]
        for row in rows:
            fields = row.split()
            if (
                fields[9] in inodes
                and int(fields[2].rsplit(":", 1)[1], 16) == port
                and (client_port is None or int(fields[1].rsplit(":", 1)[1], 16) == client_port)
            ):
                return True
    return False


def one(
    kind: str,
    output: Path,
    assets: tuple,
    sdk: Path,
    token_file: Path | None = None,
    auth_broker: Path | None = None,
) -> dict:
    directory = output / kind
    directory.mkdir(mode=0o700)
    home = directory / "home"
    home.mkdir()
    temporary = directory / "tmp"
    temporary.mkdir(mode=0o700)
    server = ModelServer(assets[1], assets[2])
    endpoint = f"https://localhost:{server.server_port}"
    environment = {
        "PATH": "/usr/bin:/bin",
        "HOME": str(home),
        "TMPDIR": str(temporary),
        "LANG": "C.UTF-8",
        "NODE_EXTRA_CA_CERTS": str(assets[1]),
        "QODER_CONFIG_DIR": str(home / ".qoder"),
        "QODER_USER_CONFIG_DIR": str(home / ".qoder"),
        "XDG_CONFIG_HOME": str(home / ".config"),
    }
    command = (
        ["node", str(ROOT / "fixtures/sdk_transport_client.mjs"), str(sdk), endpoint]
        if kind == "qoder"
        else [
            sys.executable,
            str(ROOT / "fixtures/sdk_transport_client.py"),
            kind,
            endpoint,
            str(assets[1]),
        ]
    )
    auth_proxy = None
    token_fd = None
    target = None
    collectors = []
    result = {
        "sdk": kind,
        "native_hooks": False,
        "aw_registration": False,
        "network": (
            "loopback_with_allowlisted_bootstrap"
            if kind == "qoder" and token_file
            else "loopback_only"
        ),
        "processes": [],
    }
    try:
        if kind == "qoder" and token_file:
            if auth_broker is None:
                raise ValueError("auth_broker_required")
            token_fd = os.open(token_file, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
            info = os.fstat(token_fd)
            if (
                not stat.S_ISREG(info.st_mode)
                or info.st_uid != os.getuid()
                or info.st_mode & 0o077
                or not 1 <= info.st_size <= 8192
            ):
                raise ValueError("private_token_file_required")
            auth_proxy = AuthProxy(
                auth_broker, output / "auth-cert.pem", output / "auth-key.pem", server.server_port
            )
            environment["SDK_TRANSPORT_TOKEN_FD"] = str(token_fd)
            bundle = directory / "ca-bundle.pem"
            bundle.write_bytes(assets[1].read_bytes() + (output / "auth-cert.pem").read_bytes())
            environment["NODE_EXTRA_CA_CERTS"] = str(bundle)
            command.append(f"http://127.0.0.1:{auth_proxy.server_port}")
        with (Path(os.devnull) if token_fd is not None else directory / "client.stderr").open(
            "w"
        ) as error:
            target = subprocess.Popen(
                command,
                env=environment,
                cwd=directory,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=error,
                text=True,
                start_new_session=True,
                pass_fds=(() if token_fd is None else (token_fd,)),
            )
        if token_fd is not None:
            os.close(token_fd)
            token_fd = None
        result["ready"] = line(target, 30)
        if kind != "qoder" or auth_proxy:
            target.stdin.write("\n")
            target.stdin.flush()
        result["tls_connection_started"] = (
            server.accepted.wait(30) if kind != "qoder" or auth_proxy else False
        )
        if auth_proxy:
            auth_proxy.disable_auth()
            result["auth_closed_before_observation"] = True
            result["custom_provider_allowed"] = auth_proxy.custom_provider_allowed
            if not result["tls_connection_started"]:
                try:
                    result["client_result"] = line(target, 25)
                except (TimeoutError, RuntimeError, json.JSONDecodeError) as error:
                    result["client_result"] = {
                        "completed": False,
                        "error_type": type(error).__name__,
                    }
                result.update(
                    model_request_verified=False,
                    transport_observed=False,
                    status="FAIL",
                    failure_reason="sdk_initialization_before_local_model",
                    proxy_rejections=auth_proxy.rejections,
                    denied_metadata_routes=auth_proxy.denied_routes,
                    proxy_errors=auth_proxy.proxy_errors,
                    proxy_blocked_connections_or_paths=auth_proxy.blocked,
                )
                if result["custom_provider_allowed"] is False and not result["proxy_errors"]:
                    result["failure_reason"] = "custom_provider_not_authorized"
                result["status"] = classify(result)
                return result
        owned = descendants(target.pid)
        for pid in sorted(owned):
            entry = {"role": "frontend" if pid == target.pid else "worker", "pid": pid}
            try:
                entry["model_sender"] = owns_model_connection(
                    pid,
                    auth_proxy.server_port if auth_proxy else server.server_port,
                    auth_proxy.model_peer_port if auth_proxy else None,
                )
                identity = process_ref(pid)
                entry["identity"] = identity
                entry["executable_sha256"] = hashlib.sha256(
                    Path(f"/proc/{pid}/exe").read_bytes()
                ).hexdigest()
                for function in ("SSL_write", "SSL_write_ex"):
                    resolve(pid, function)
                    permit = directory / f"permit-{pid}.json"
                    permit.write_text(
                        json.dumps(
                            {
                                "schema_version": "aw.ptrace-permit-poc/v0",
                                "authorized_experiment": True,
                                "process": identity,
                                "exe_sha256": entry["executable_sha256"],
                            }
                        )
                    )
                    permit.chmod(0o600)
                    events = directory / f"events-{pid}-{function}.jsonl"
                    with (directory / f"collector-{pid}-{function}.stderr").open("w") as error:
                        collector = subprocess.Popen(
                            [
                                sys.executable,
                                str(ROOT / "scripts/uprobe_transport.py"),
                                "--permit",
                                str(permit),
                                "--library",
                                str(output / "loader.so"),
                                "--object",
                                str(output / "transport.bpf.o"),
                                "--function",
                                function,
                                "--output",
                                str(events),
                                "--privileged-fixture-setup",
                                "--duration",
                                "30",
                            ],
                            stderr=error,
                        )
                    collectors.append((collector, events, pid))
                    wait_for(lambda: records(events) or collector.poll() is not None)
                    if collector.poll() is not None:
                        raise RuntimeError("collector_setup_failed")
                entry["probe"] = "attached"
            except (OSError, ValueError, RuntimeError) as error:
                entry["probe"] = str(error)
            result["processes"].append(entry)
        if kind == "qoder" and not auth_proxy:
            target.stdin.write("\n")
            target.stdin.flush()
            result["tls_connection_started"] = server.accepted.wait(12)
        server.release.set()
        try:
            result["client_result"] = line(target, 25)
        except (TimeoutError, RuntimeError, json.JSONDecodeError) as error:
            result["client_result"] = {"completed": False, "error_type": type(error).__name__}
        time.sleep(0.15)
        for collector, events, pid in collectors:
            if collector.poll() is None:
                collector.send_signal(signal.SIGINT)
            collector.wait(5)
            stream = records(events)
            observed = [event for event in stream if event["kind"] == "transport.before_send"]
            assert "SYNTHETIC_TRANSPORT_PROBE" not in events.read_text()
            assert "synthetic-fixture-key" not in events.read_text()
            assert "FIXTURE_OK" not in events.read_text()
            result.setdefault("sources", []).append(
                {
                    "pid": pid,
                    "exit": collector.returncode,
                    "events": len(observed),
                    "captured_bytes": sum(event["transport"]["length_bytes"] for event in observed),
                    "digests": sum(
                        event["transport"]["payload_sha256"] is not None for event in observed
                    ),
                    "server_body_digest_matches": sum(
                        event["transport"]["payload_sha256"] == request["sha256"]
                        for event in observed
                        for request in server.requests
                    ),
                    "server_wire_digest_matches": sum(
                        event["transport"]["payload_sha256"] == request["wire_sha256"]
                        for event in observed
                        for request in server.requests
                    ),
                    "summary": stream[-1] if stream else None,
                }
            )
        result["server_requests"] = server.requests
        result["model_request_verified"] = any(item["marker_present"] for item in server.requests)
        result["transport_observed"] = any(item["events"] for item in result.get("sources", []))
        if auth_proxy:
            result["proxy_blocked_connections_or_paths"] = auth_proxy.blocked
            result["proxy_rejections"] = auth_proxy.rejections
            result["denied_metadata_routes"] = auth_proxy.denied_routes
            result["proxy_errors"] = auth_proxy.proxy_errors
        result["status"] = classify(result)
        return result
    finally:
        server.release.set()
        for collector, _, _ in collectors:
            stop(collector)
        if target:
            try:
                os.killpg(target.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            stop(target)
            try:
                os.killpg(target.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        server.close()
        if auth_proxy:
            auth_proxy.close()
        if token_fd is not None:
            os.close(token_fd)


def classify(result: dict) -> str:
    """Separate verified unsupported coverage and offline auth from failures."""
    for process in result["processes"]:
        if process["probe"] not in ("attached", "unsupported_tls_library"):
            return "FAIL"
    for source in result.get("sources", []):
        if source["exit"] != 0 or not source["summary"].get("accounting_complete"):
            return "FAIL"
    if result.get("proxy_errors"):
        return "FAIL"
    if not result["model_request_verified"] or not result["client_result"].get("completed"):
        if result["sdk"] == "qoder" and result.get("custom_provider_allowed") is False:
            return "BLOCKED"
        if (
            result["sdk"] == "qoder"
            and result["client_result"].get("blocked_reason") == "offline_auth_initialization"
        ):
            return "BLOCKED"
        return "FAIL"
    if result["transport_observed"]:
        if result.get("auth_closed_before_observation"):
            senders = {
                process["pid"] for process in result["processes"] if process.get("model_sender")
            }
            if not any(
                source["pid"] in senders
                and (
                    source.get("server_body_digest_matches", 0)
                    or source.get("server_wire_digest_matches", 0)
                )
                for source in result.get("sources", [])
            ):
                return "UNVERIFIED_TRANSPORT"
        return "OBSERVED"
    if any(
        process.get("model_sender") and process["probe"] == "unsupported_tls_library"
        for process in result["processes"]
    ):
        return "UNSUPPORTED"
    return "FAIL"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--qoder-sdk", type=Path, required=True)
    parser.add_argument("--qoder-token-file", type=Path)
    parser.add_argument("--auth-broker", type=Path)
    parser.add_argument("--case", choices=["langchain", "claude", "qoder"], action="append")
    options = parser.parse_args()
    if {name for _, name in socket.if_nameindex()} != {"lo"} or os.geteuid() == 0:
        raise ValueError("run_as_nonroot_in_loopback_only_network_namespace")
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    os.umask(0o077)
    output = options.output.resolve()
    assets = build(output)
    subprocess.run(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost,IP:127.0.0.1",
            "-keyout",
            str(assets[2]),
            "-out",
            str(assets[1]),
        ],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=15,
    )
    if options.qoder_token_file:
        subprocess.run(
            [
                "openssl",
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=openapi.qoder.sh",
                "-addext",
                "subjectAltName=DNS:openapi.qoder.sh,DNS:api2.qoder.sh",
                "-keyout",
                str(output / "auth-key.pem"),
                "-out",
                str(output / "auth-cert.pem"),
            ],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=15,
        )
    results = {
        "versions": {
            name: version(name)
            for name in ("langchain-openai", "langchain-core", "claude-agent-sdk")
        },
        "cases": [],
    }
    package = json.loads((options.qoder_sdk.resolve().parent.parent / "package.json").read_text())
    results["versions"]["qoder-sdk"] = package["version"]
    results["versions"]["qoder-worker"] = package["qoderCliVersion"]
    results["versions"]["node"] = subprocess.check_output(
        ["node", "--version"], text=True, timeout=5
    ).strip()
    for kind in options.case or ["langchain", "claude", "qoder"]:
        try:
            result = one(
                kind,
                output,
                assets,
                options.qoder_sdk.resolve(),
                options.qoder_token_file,
                options.auth_broker,
            )
        except Exception as error:
            result = {
                "sdk": kind,
                "error_type": type(error).__name__,
                "error": "private_auth_fixture_failed" if options.qoder_token_file else str(error),
                "status": "FAIL",
            }
        results["cases"].append(result)
        (output / "results.json").write_text(json.dumps(results, indent=2))
        print(
            json.dumps(
                {
                    "sdk": kind,
                    "model_request_verified": result.get("model_request_verified"),
                    "transport_observed": result.get("transport_observed"),
                    "status": result["status"],
                    "error_type": result.get("error_type"),
                }
            ),
            flush=True,
        )

    if any(
        case["status"] in ("FAIL", "BLOCKED", "UNVERIFIED_TRANSPORT") for case in results["cases"]
    ):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
