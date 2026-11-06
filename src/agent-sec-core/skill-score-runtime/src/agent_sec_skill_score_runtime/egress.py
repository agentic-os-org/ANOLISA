"""Static task egress through Harbor's public Compose extension."""

import ipaddress
import json
import re
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_skill_score_runtime.images import sidecar_image
from agent_sec_skill_score_runtime.resources import OWNER_LABEL

SERVICE = "asc-egress"
POLICY_PATH = "/etc/asc-egress/policy.json"


def prepare_egress(
    task: Path, model_host: str, addresses: list[str], request_id: str = ""
) -> None:
    """Bind the proxy policy outside the untrusted main container's mounts."""
    host = model_host.lower()
    labels = host.split(".")
    if len(host) > 253 or any(
        not re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", label)
        for label in labels
    ):
        raise ContractError(
            "model endpoint must have an exact DNS hostname or IPv4 address"
        )
    try:
        resolved = sorted(
            {str(ipaddress.IPv4Address(address)) for address in addresses}
        )
    except ipaddress.AddressValueError as exc:
        raise ContractError(
            "model endpoint resolution must contain only IPv4 addresses"
        ) from exc
    if not resolved:
        raise ContractError("model endpoint resolution returned no IPv4 addresses")

    policy: dict[str, Any] = {
        "services": [
            {
                "name": "asc-transparent-egress",
                "addr": ":12345",
                "bypass": "asc-allowed-hosts",
                "hosts": "asc-model-hosts",
                "resolver": "asc-no-dns",
                # GOST 0.10.9 GetInt does not accept JSON's float64 numbers.
                "metadata": {"so_mark": "114514"},
                "handler": {
                    "type": "red",
                    "metadata": {
                        "sniffing": True,
                        "sniffing.timeout": "5s",
                        "sniffing.fallback": True,
                    },
                },
                "listener": {"type": "red"},
            }
        ],
        "bypasses": [
            {"name": "asc-allowed-hosts", "whitelist": True, "matchers": [host]}
        ],
        "hosts": [
            {
                "name": "asc-model-hosts",
                "mappings": [{"hostname": host, "ip": address} for address in resolved],
            }
        ],
        # GOST's mapper is case-sensitive. Never fall back to shared /etc/hosts
        # or DNS when a request misses it, including mixed-case SNI.
        "resolvers": [{"name": "asc-no-dns", "nameservers": []}],
    }
    policy_path = task.parent / "egress-policy.json"
    policy_path.write_text(json.dumps(policy, indent=2) + "\n")
    policy_path.chmod(0o600)
    compose = {
        "services": {
            "main": {
                "pull_policy": "never",
                "network_mode": f"service:{SERVICE}",
                "depends_on": {SERVICE: {"condition": "service_healthy"}},
                "cap_drop": ["NET_ADMIN", "NET_RAW"],
                "healthcheck": {
                    "test": ["CMD-SHELL", "kill -0 1"],
                    "interval": "1s",
                    "timeout": "2s",
                    "retries": 5,
                },
            },
            SERVICE: {
                "image": sidecar_image(),
                "pull_policy": "never",
                "restart": "no",
                "cap_add": ["NET_ADMIN", "NET_RAW"],
                # Docker shares this hosts file with main. It is only a client
                # convenience; GOST trusts the separate read-only policy above.
                "extra_hosts": [f"{host}:{address}" for address in resolved],
                "volumes": [
                    {
                        "type": "bind",
                        "source": str(policy_path.resolve()).replace("$", "$$"),
                        "target": POLICY_PATH,
                        "read_only": True,
                        "bind": {"create_host_path": False},
                    }
                ],
                "healthcheck": {
                    "test": ["CMD", "/usr/local/bin/asc-egress-healthcheck"],
                    "interval": "1s",
                    "timeout": "2s",
                    "start_period": "10s",
                    "retries": 5,
                },
            },
        }
    }
    if request_id:
        for service in compose["services"].values():
            service["labels"] = {OWNER_LABEL: request_id}
        compose["networks"] = {"default": {"labels": {OWNER_LABEL: request_id}}}
    environment = task / "environment"
    environment.mkdir(parents=True, exist_ok=True)
    # JSON is a YAML subset, with no handwritten quoting rules.
    (environment / "docker-compose.yaml").write_text(
        json.dumps(compose, indent=2) + "\n"
    )
    (task.parent / "effective-network.json").write_text(
        json.dumps(
            {
                "mode": "static-allowlist",
                "owner": "agent-sec-core",
                "model_host": host,
                "resolved_ipv4": resolved,
                "local_traffic": "direct",
                "icmp": "direct",
                "sidecar_image": sidecar_image(),
                "phase_policy": "shared across all agent and verifier steps",
            },
            indent=2,
        )
        + "\n"
    )
