"""Policy generation regressions; no Docker daemon or Harbor import required."""

import json

import pytest

from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_skill_score_runtime.egress import POLICY_PATH, prepare_egress
from agent_sec_skill_score_runtime.images import sidecar_image


def test_proxy_resolution_is_not_a_main_mount(tmp_path):
    task = tmp_path / "task"
    prepare_egress(task, "Example.COM", ["192.0.2.2", "192.0.2.1", "192.0.2.2"])
    compose = json.loads((task / "environment/docker-compose.yaml").read_text())
    main = compose["services"]["main"]
    sidecar = compose["services"]["asc-egress"]
    assert main["network_mode"] == "service:asc-egress"
    assert main["pull_policy"] == "never"
    assert main["depends_on"] == {"asc-egress": {"condition": "service_healthy"}}
    assert set(main["cap_drop"]) == {"NET_ADMIN", "NET_RAW"}
    assert "volumes" not in main and "extra_hosts" not in main
    assert main["healthcheck"]["test"] == ["CMD-SHELL", "kill -0 1"]
    assert sidecar["image"] == sidecar_image()
    assert sidecar["pull_policy"] == "never" and sidecar["restart"] == "no"
    assert sidecar["volumes"] == [
        {
            "type": "bind",
            "source": str(tmp_path / "egress-policy.json"),
            "target": POLICY_PATH,
            "read_only": True,
            "bind": {"create_host_path": False},
        }
    ]
    assert sidecar["extra_hosts"] == ["example.com:192.0.2.1", "example.com:192.0.2.2"]
    assert (tmp_path / "egress-policy.json").stat().st_mode & 0o777 == 0o600


def test_mapping_misses_cannot_use_shared_hosts_or_dns(tmp_path):
    prepare_egress(tmp_path / "task", "example.com", ["192.0.2.1"])
    policy = json.loads((tmp_path / "egress-policy.json").read_text())
    service = policy["services"][0]
    # Numeric JSON metadata silently becomes mark 0 in the pinned GOST parser.
    assert service["metadata"]["so_mark"] == "114514"
    assert service["hosts"] == "asc-model-hosts"
    assert service["resolver"] == "asc-no-dns"
    assert policy["resolvers"] == [{"name": "asc-no-dns", "nameservers": []}]
    assert policy["hosts"] == [
        {"name": "asc-model-hosts", "mappings": [{"hostname": "example.com", "ip": "192.0.2.1"}]}
    ]
    assert policy["bypasses"] == [
        {"name": "asc-allowed-hosts", "whitelist": True, "matchers": ["example.com"]}
    ]
    assert "reload" not in json.dumps(policy)
    assert "192.0.2.1" not in policy["bypasses"][0]["matchers"]
    effective = json.loads((tmp_path / "effective-network.json").read_text())
    assert effective["mode"] == "static-allowlist" and effective["owner"] == "agent-sec-core"


@pytest.mark.parametrize(
    "hostname",
    ["", "*.example.com", ".example.com", "a/b", "a\nb", "a:443", "a$B", "x." + "a" * 64],
)
def test_invalid_host_never_creates_policy(tmp_path, hostname):
    with pytest.raises(ContractError, match="exact DNS"):
        prepare_egress(tmp_path / "task", hostname, ["192.0.2.1"])
    assert not list(tmp_path.iterdir())


@pytest.mark.parametrize("addresses", [[], ["::1"], ["bad"], ["192.0.2.1\nexample.net"]])
def test_unresolved_or_unsupported_addresses_fail_before_policy(tmp_path, addresses):
    with pytest.raises(ContractError, match="IPv4"):
        prepare_egress(tmp_path / "task", "example.com", addresses)
    assert not list(tmp_path.iterdir())
