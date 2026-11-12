"""DPROC-014: render and validate the actual V2 packaging target."""

import subprocess
import tomllib
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[3]


def test_v2_stages_root_system_unit(tmp_path):
    subprocess.run(
        [
            "make",
            "install-systemd-system",
            f"DESTDIR={tmp_path}",
            "SYSTEMD_SERVICE_BINDIR=/usr/bin",
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    unit = tmp_path / "usr/lib/systemd/system/agent-sec-core.service"
    content = unit.read_text()
    assert 'ExecStart="/usr/bin/agent-sec-daemon" serve' in content
    assert "{bindir}" not in content
    assert not (tmp_path / "usr/lib/systemd/user").exists()
    for setting in (
        "User=root",
        "Group=root",
        "RuntimeDirectory=agent-sec-core",
        "RuntimeDirectoryMode=0755",
        "RuntimeDirectoryPreserve=yes",
        "Type=simple",
        "Restart=on-failure",
        "TimeoutStopSec=75",
        "KillMode=control-group",
        "StartLimitBurst=5",
        "StandardError=journal",
        "CapabilityBoundingSet=CAP_DAC_OVERRIDE CAP_CHOWN CAP_FOWNER CAP_SYS_PTRACE",
        "AmbientCapabilities=",
        "StateDirectory=agent-sec/skillsec",
        "LogsDirectory=agent-sec",
        "SystemCallFilter=@system-service",
        "SystemCallArchitectures=native",
        "MemoryDenyWriteExecute=true",
    ):
        assert setting in content.splitlines()
    assert not (tmp_path / "usr/lib/sysusers.d/agent-sec-core.conf").exists()
    spec = (ROOT / "packaging/rpm/agent-sec-core.spec.v2.in").read_text()
    assert "%systemd_postun_with_restart agent-sec-core.service" in spec
    assert "rpm -qa --qf '%%{NAME}\\n'" in spec
    assert "grep -Fxq agent-sec-cli" in spec
    assert "%pre\n" in spec and "%posttrans\n" in spec
    assert "if [ -e /usr/lib/systemd/user/agent-sec-core.service ]; then" in spec
    assert "/usr/bin/systemctl enable --now agent-sec-core.service" in spec
    assert "systemd_user_" not in spec and "_userunitdir" not in spec
    assert "systemd-sysusers" not in spec
    assert "%pre -n agent-sec-cli" not in spec
    makefile = (ROOT / "packaging/install.mk").read_text()
    targets = [
        line
        for line in makefile.splitlines()
        if line.startswith("install-all-for-rpmbuild-v2:")
    ]
    assert any("install-systemd-system" in line for line in targets)
    assert all("install-systemd-user" not in line for line in targets)


def test_source_build_stages_v2_unit_with_source_binary_path(tmp_path):
    subprocess.run(
        [
            "make",
            "install-systemd-system-source",
            f"DESTDIR={tmp_path}",
            "BINDIR=/usr/local/bin",
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    unit = tmp_path / "etc/systemd/system/agent-sec-core.service"
    content = unit.read_text()
    assert 'ExecStart="/usr/local/bin/agent-sec-daemon" serve' in content
    assert "{bindir}" not in content
    assert not (tmp_path / "usr/lib/systemd/user").exists()


def test_shared_manifest_staging(tmp_path):
    subprocess.run(
        [
            "make",
            "stage-component-manifest",
            "install-component-manifest",
            f"BUILD_DIR={tmp_path / 'build'}",
            f"DESTDIR={tmp_path / 'install'}",
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    installed = (
        tmp_path / "install/usr/share/anolisa/components/sec-core/component.toml"
    )
    staged = tmp_path / "build/share/anolisa/components/sec-core/component.toml"
    assert (
        installed.read_bytes()
        == staged.read_bytes()
        == (ROOT / "packaging/anolisa/component.toml").read_bytes()
    )
    component = tomllib.loads(installed.read_text())["component"]
    assert component["services"][0]["scope"] == "system"


def test_gateway_stages_unit_drop_in_and_data_plane_files(tmp_path):
    binaries = tmp_path / "binaries"
    binaries.mkdir()
    for name in (
        "agent-sec-cli",
        "agent-sec-daemon",
        "agent-sec-daemon-health",
        "asc-sandbox-docker-guard",
    ):
        (binaries / name).write_text("#!/bin/sh\nexit 0\n")
    staged = tmp_path / "staged"
    build = tmp_path / "build"
    subprocess.run(
        ["make", "stage-gateway", f"BUILD_DIR={build}"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    subprocess.run(
        [
            "make",
            "install-all-for-rpmbuild-v2",
            "RPM_INSTALL_COMMON_TARGETS=",
            f"V2_BIN_BUILD_DIR={binaries}",
            f"BUILD_DIR={build}",
            f"DESTDIR={staged}",
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    datadir = staged / "usr/share/anolisa/sec-core/gateway"
    assert sorted(path.name for path in datadir.iterdir()) == [
        "asc_gateway_addon.py",
        "asc_gateway_policy.py",
        "github-policy.json",
        "install-mitmproxy.sh",
        "mitmproxy.provenance",
    ]
    for path in datadir.iterdir():
        expected_mode = 0o755 if path.name == "install-mitmproxy.sh" else 0o644
        assert path.stat().st_mode & 0o777 == expected_mode
    assert not (staged / "opt/agent-sec-gateway").exists()
    assert not (staged / "run/agent-sec-gateway").exists()
    assert not (staged / "var/lib/agent-sec/credentials").exists()
    units = staged / "usr/lib/systemd/system"
    unit = (units / "agent-sec-gateway.service").read_text()
    assert "{datadir}" not in unit
    assert "-s /usr/share/anolisa/sec-core/gateway/asc_gateway_addon.py" in unit
    lines = unit.splitlines()
    for setting in (
        "User=agent-sec-gw",
        "CapabilityBoundingSet=",
        "NoNewPrivileges=true",
        "ProtectSystem=strict",
        "ReadWritePaths=/run/agent-sec-gateway/status",
    ):
        assert setting in lines
    listeners = [
        port for port in range(15001, 15009) if f"transparent@127.0.0.1:{port}" in unit
    ]
    assert listeners == list(range(15001, 15009))
    drop_in = (units / "agent-sec-core.service.d/gateway.conf").read_text()
    assert "CapabilityBoundingSet=CAP_NET_ADMIN" in drop_in.splitlines()


@pytest.mark.parametrize(
    "erase,manager,manifest,active,disable_exit,expected_exit,expected_actions",
    [
        (0, True, True, True, 0, 0, ["disable", "continue"]),
        (0, True, False, True, 0, 0, ["disable", "continue"]),
        (0, True, True, True, 1, 1, ["disable"]),
        (0, True, False, False, 0, 0, ["continue"]),
        (0, False, True, True, 0, 0, ["continue"]),
        (1, True, True, True, 0, 0, ["continue"]),
    ],
    ids=[
        "manifest",
        "active-unit",
        "disable-failed",
        "inactive",
        "no-systemd",
        "upgrade",
    ],
)
def test_rpm_erase_withdraws_gateway_before_service_cleanup(
    tmp_path,
    erase,
    manager,
    manifest,
    active,
    disable_exit,
    expected_exit,
    expected_actions,
):
    actions = tmp_path / "actions"
    manager_dir = tmp_path / "manager"
    if manager:
        manager_dir.mkdir()
    manifest_file = tmp_path / "active.json"
    if manifest:
        manifest_file.touch()
    systemctl = tmp_path / "systemctl"
    systemctl.write_text(f"#!/bin/sh\nexit {0 if active else 3}\n")
    systemctl.chmod(0o755)
    cli = tmp_path / "agent-sec-cli"
    cli.write_text(
        f"#!/bin/sh\n[ \"$1 $2\" = 'gateway disable' ] || exit 2\n"
        f"printf 'disable\\n' >> '{actions}'\nexit {disable_exit}\n"
    )
    cli.chmod(0o755)
    spec = (ROOT / "packaging/rpm/agent-sec-core.spec.v2.in").read_text()
    preun = spec.split("%preun\n", 1)[1].split("%systemd_preun", 1)[0]
    preun = (
        preun.replace("/run/systemd/system", str(manager_dir))
        .replace("/run/agent-sec-gateway/active.json", str(manifest_file))
        .replace("/usr/bin/systemctl", str(systemctl))
        .replace("/usr/bin/agent-sec-cli", str(cli))
    )
    script = tmp_path / "preun.sh"
    script.write_text(preun + f"printf 'continue\\n' >> '{actions}'\n")
    result = subprocess.run(
        ["sh", str(script), str(erase)],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == expected_exit
    assert actions.read_text().splitlines() == expected_actions
