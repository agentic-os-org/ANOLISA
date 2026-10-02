"""DPROC-014: render and validate the actual V2 packaging target."""

import subprocess
import tomllib
from pathlib import Path

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
        "CapabilityBoundingSet=CAP_DAC_OVERRIDE CAP_CHOWN CAP_FOWNER",
        "AmbientCapabilities=",
        "StateDirectory=agent-sec/skillsec",
        "LogsDirectory=agent-sec",
        "SystemCallFilter=@system-service",
        "SystemCallArchitectures=native",
        "MemoryDenyWriteExecute=true",
    ):
        assert setting in content.splitlines()
    assert not (tmp_path / "usr/lib/sysusers.d/agent-sec-core.conf").exists()
    spec = (ROOT / "agent-sec-core.spec.v2.in").read_text()
    assert "%systemd_postun_with_restart agent-sec-core.service" in spec
    assert "systemd_user_" not in spec and "_userunitdir" not in spec
    assert "systemd-sysusers" not in spec
    assert "%pre -n agent-sec-cli" not in spec
    makefile = (ROOT / "Makefile").read_text()
    targets = [
        line
        for line in makefile.splitlines()
        if line.startswith("install-all-for-rpmbuild-v2:")
    ]
    assert any("install-systemd-system" in line for line in targets)
    assert all("install-systemd-user" not in line for line in targets)


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
        == (ROOT / ".anolisa/component.toml").read_bytes()
    )
    component = tomllib.loads(installed.read_text())["component"]
    assert component["services"][0]["scope"] == "user"
