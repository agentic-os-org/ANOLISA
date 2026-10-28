#!/usr/bin/env python3
"""Verify distribution ownership of RPM integration contracts without compiling."""

from __future__ import annotations

import fnmatch
import hashlib
import os
import re
import runpy
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = ROOT.parents[1]
COMPONENTS = (
    "cosh-ng",
    "sec-core",
    "agentsight",
    "tokenless",
    "skillfs",
    "ws-ckpt",
    "agent-memory",
)
REGISTRY = ROOT / "manifests/components"
SPECS = {
    "sec-core": "agent-sec-core/packaging/rpm/agent-sec-core.spec.v2.in",
    **{name: f"{name}/packaging/rpm/{name}.spec.in" for name in COMPONENTS if name != "sec-core"},
}
OLD_OWNERS = {
    "cosh-ng": "0.28.1",
    "agent-sec-cli": "1.1.2",
    "agentsight": "0.15.1",
    "tokenless": "0.9.2",
    "skillfs": "0.5.5",
    "ws-ckpt": "0.6.1",
    "agent-memory": "0.2.13",
}


def run(*args: str, **kwargs: object) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, text=True, capture_output=True, check=True, **kwargs)


def contract(component: str) -> dict:
    return tomllib.loads((REGISTRY / component / "component.toml").read_text())


def file_patterns(spec: str, *, builddir: Path | None = None) -> dict[str, list[str]]:
    """Read actual role inventories, excluding directory-only ownership entries."""
    name = re.search(r"^Name:\s+(\S+)", spec, re.M).group(1)
    macros = {
        "%{name}": name,
        "%{_datadir}": "/usr/share",
        "%{_bindir}": "/usr/bin",
        "%{_adapterdir_tokenless}": "/usr/share/anolisa/adapters/tokenless",
        "%{_extensiondir_tokenless}": "/usr/share/anolisa/extensions/tokenless",
    }
    inventories = {}
    for match in re.finditer(
        r"^%files(?: ([^\n]+))?\n(.*?)(?=^%(?:files|preun|postun|post|pre|changelog|package|description|install|build|prep)\b|\Z)",
        spec,
        re.M | re.S,
    ):
        args, body = match.groups()
        arguments = shlex.split(args or "")
        if "-f" in arguments:
            position = arguments.index("-f")
            if builddir is None or position + 1 == len(arguments):
                raise AssertionError("RPM generated file list requires its install build directory")
            body += "\n" + (builddir / arguments[position + 1]).read_text()
            del arguments[position : position + 2]
        if not arguments:
            package = name
        elif len(arguments) == 2 and arguments[0] == "-n":
            package = arguments[1]
        elif len(arguments) == 1:
            package = f"{name}-{arguments[0]}"
        else:
            raise AssertionError("Unexpected RPM file-list arguments: " + str(arguments))
        paths = []
        for line in body.splitlines():
            if line.startswith(("%dir", "%defattr", "%license", "%doc", "#")):
                continue
            line = re.sub(r"%(?:attr|config)\([^)]*\)\s*", "", line).strip()
            for macro, value in macros.items():
                line = line.replace(macro, value)
            if line.startswith("/"):
                paths.append(line)
        inventories[package] = paths
    return inventories


class ComponentContracts(unittest.TestCase):
    def test_registry_components_are_reachable_through_identity_indexes(self) -> None:
        for filename in ("components.toml", "components-v2.toml"):
            index = tomllib.loads((ROOT / "manifests" / filename).read_text())
            rows = {row["name"]: row for row in index["components"]}
            for component in COMPONENTS:
                with self.subTest(index=filename, component=component):
                    package = contract(component)["backends"]["rpm"]["package"]
                    self.assertIn(component, rows)
                    self.assertIn(
                        {"kind": "rpm", "package": package},
                        [
                            {"kind": backend["kind"], "package": backend["package"]}
                            for backend in rows[component]["backends"]
                        ],
                    )

    def test_prebuilt_archive_stage_has_executable_sibling_data(self) -> None:
        script = (ROOT / "packaging/prebuilt/package.sh").read_text()
        body = script.split('STAGE="$WORK/stage"\n', 1)[1].split("ARTIFACT=", 1)[0]
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary)
            binaries = workspace / "binaries"
            binaries.mkdir()
            binary = binaries / "anolisa"
            binary.write_text("#!/bin/sh\nexit 0\n")
            binary.chmod(0o755)
            stage = workspace / "stage"
            env = {
                **os.environ,
                "STAGE": str(stage),
                "OUTPUT_DIR": str(workspace / "output"),
                "BIN_DIR": str(binaries),
                "SOURCE_ROOT": str(ROOT),
            }
            run("bash", "-eu", "-c", body, env=env)
            self.assertEqual(
                {str(path.relative_to(stage)) for path in stage.rglob("*") if path.is_file()},
                {"bin/anolisa"}
                | {f"share/anolisa/components/{name}/component.toml" for name in COMPONENTS},
            )
            self.assertTrue(os.access(stage / "bin/anolisa", os.X_OK))

    def test_installer_rejects_old_rpm_owners_before_promotion(self) -> None:
        installer = (ROOT / "scripts/install-anolisa.sh").read_text()
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary)
            source = workspace / "source"
            shutil.copytree(ROOT / "manifests", source / "manifests")
            (source / "templates").mkdir()
            (source / "target/release").mkdir(parents=True)
            binary = source / "target/release/anolisa"
            binary.write_text("#!/bin/sh\nexit 0\n")
            binary.chmod(0o755)
            prefix = workspace / "prefix"
            registry = prefix / "share/anolisa"
            old = registry / "components/tokenless/component.toml"
            old.parent.mkdir(parents=True)
            old.write_text("old RPM contract must survive\n")
            commands = workspace / "commands"
            commands.mkdir()
            rpm = commands / "rpm"
            rpm.write_text(
                '#!/bin/bash\nfor path in "$@"; do :; done\n'
                'if [ "$path" = "$OWNED_CONTRACT" ]; then printf "%s\\n" "$RPM_OWNER"; '
                "else exit 1; fi\n"
            )
            rpm.chmod(0o755)
            env = {
                **os.environ,
                "PATH": f"{commands}:{os.environ['PATH']}",
                "ANOLISA_PREFIX": str(prefix),
                "OWNED_CONTRACT": str(old),
                "RPM_OWNER": "tokenless",
            }
            with self.assertRaises(subprocess.CalledProcessError) as failure:
                run(
                    "bash",
                    str(ROOT / "scripts/install-anolisa.sh"),
                    "--from-local",
                    str(source),
                    env=env,
                )
            self.assertIn("upgrade that package", failure.exception.stderr)
            self.assertEqual(old.read_text(), "old RPM contract must survive\n")
            self.assertFalse((prefix / "bin/anolisa").exists())

            detached = workspace / "detached/install-anolisa.sh"
            detached.parent.mkdir()
            shutil.copyfile(ROOT / "scripts/install-anolisa.sh", detached)
            bundle = workspace / "manifests.tar.gz"
            run("bash", str(ROOT / "scripts/package-manifests.sh"), str(bundle))
            env.update(
                {
                    "ANOLISA_BIN_URL": binary.as_uri(),
                    "ANOLISA_MANIFEST_BUNDLE_URL": bundle.as_uri(),
                    "ANOLISA_BIN_SHA256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                    "ANOLISA_MANIFEST_BUNDLE_SHA256": hashlib.sha256(
                        bundle.read_bytes()
                    ).hexdigest(),
                }
            )
            with self.assertRaises(subprocess.CalledProcessError) as failure:
                run("bash", str(detached), "--strict", env=env)
            self.assertIn("upgrade that package", failure.exception.stderr)
            self.assertEqual(old.read_text(), "old RPM contract must survive\n")
            self.assertFalse((prefix / "bin/anolisa").exists())

            # Removing an old payload does not relinquish its rpmdb ownership.
            old.unlink()
            for arguments in (
                [str(ROOT / "scripts/install-anolisa.sh"), "--from-local", str(source)],
                [str(detached), "--strict"],
            ):
                with self.assertRaises(subprocess.CalledProcessError) as failure:
                    run("bash", *arguments, env=env)
                self.assertIn("upgrade that package", failure.exception.stderr)
                self.assertFalse(old.exists())
                self.assertFalse((prefix / "bin/anolisa").exists())

            # Exercise the same preflight with a second, higher-priority registry
            # without creating files in the host's real /usr/share/anolisa.
            body = installer.split("preflight_component_contract_owners() {", 1)[1]
            body = body.split("\n}\n", 1)[0]
            helper = "preflight_component_contract_owners() {" + body + "\n}\n"
            helper = 'err() { printf "%s\\n" "$*" >&2; }\n' + helper
            helper += 'COMPONENT_CONTRACTS=(tokenless)\npreflight_component_contract_owners "$EMPTY" "$REGISTRY"\n'
            env.update({"EMPTY": str(workspace / "unused"), "REGISTRY": str(registry)})
            for owner in OLD_OWNERS:
                env["RPM_OWNER"] = owner
                with self.assertRaises(subprocess.CalledProcessError) as failure:
                    run("bash", "-eu", "-c", helper, env=env)
                self.assertIn(owner, failure.exception.stderr)
            env["RPM_OWNER"] = "anolisa"
            run("bash", "-eu", "-c", helper, env=env)
            self.assertIn(
                'preflight_component_contract_owners "$FINAL_DATADIR" /usr/share/anolisa', installer
            )

    def test_contracts_keep_complete_layout_and_current_adapter_roles(self) -> None:
        for component in COMPONENTS:
            with self.subTest(component=component):
                data = contract(component)
                self.assertEqual(data["component"]["name"], component)
                self.assertTrue(data["component"]["layout"]["files"])
                self.assertIn("health_check", data["component"])
                self.assertNotIn("@VERSION@", (REGISTRY / component / "component.toml").read_text())
        adapters = contract("tokenless")["adapters"]
        self.assertEqual(len(adapters), 11)
        self.assertTrue({"dsh", "opencode"}.issubset({item["framework"] for item in adapters}))
        self.assertEqual(
            {item["framework"] for item in contract("sec-core")["adapters"]},
            {"openclaw", "hermes", "codex", "qoder", "qwencode", "cosh"},
        )
        self.assertEqual(contract("agentsight")["adapters"][0]["framework"], "dsh")

    def test_adapter_bundle_files_are_owned_by_declared_rpm_roles(self) -> None:
        if not (REPOSITORY / "src/tokenless").is_dir():
            self.skipTest("component RPM sources are outside this standalone source archive")
        for component in COMPONENTS:
            data = contract(component)
            spec = (REPOSITORY / "src" / SPECS[component]).read_text()
            if component == "tokenless":
                fixture = runpy.run_path(str(REPOSITORY / "src/tokenless/tests/test_rpm_split.py"))[
                    "install_fixture"
                ]
                with tempfile.TemporaryDirectory(prefix="rpm-contract-") as temporary:
                    builddir, _, _ = fixture(Path(temporary), spec)
                    inventories = file_patterns(spec, builddir=builddir)
            else:
                inventories = file_patterns(spec)
            health_binary = data["component"]["health_check"]["binary"]
            self.assertTrue(health_binary.startswith("{bindir}/"))
            # The RPM inventory fixture uses /usr/bin; ANOLISA's runtime
            # layout expands {bindir} according to its own install mode.
            rpm_binary = health_binary.replace("{bindir}", "/usr/bin", 1)
            self.assertTrue(any(rpm_binary in paths for paths in inventories.values()), rpm_binary)
            for adapter in data.get("adapters", []):
                with self.subTest(component=component, framework=adapter["framework"]):
                    rpm = adapter["backends"]["rpm"]
                    self.assertTrue(rpm["resource_root"].startswith("/"))
                    self.assertNotIn("{datadir}", rpm["resource_root"])
                    packages = rpm["managed_packages"]
                    self.assertTrue(
                        packages, "compatibility metadata alone cannot supply plugin files"
                    )
                    for package in packages:
                        self.assertIn(package, inventories)
                    root = rpm["resource_root"]
                    root = root.replace("{datadir}", "/usr/share/anolisa").replace(
                        "{component}", component
                    )
                    entry = adapter.get("bundle", {}).get("entry", "")
                    bundle = f"{root.rstrip('/')}/{entry}".rstrip("/")
                    owners = {
                        package
                        for package, paths in inventories.items()
                        if any(
                            fnmatch.fnmatch(bundle, path)
                            or bundle.startswith(path.rstrip("/") + "/")
                            for path in paths
                        )
                    }
                    self.assertTrue(owners.intersection(packages), (bundle, owners, packages))
                    self.assertNotIn(
                        data.get("backends", {}).get("rpm", {}).get("package", component), owners
                    )

    def test_rpm_installs_registry_without_component_payloads(self) -> None:
        spec = (ROOT / "anolisa.spec.in").read_text()
        for package, evr in OLD_OWNERS.items():
            self.assertRegex(spec, rf"(?m)^Conflicts:\s+{package} < {re.escape(evr)}$")
        self.assertNotRegex(
            spec.split("%description", 1)[0], r"(?m)^Requires:\s+(?:cosh-ng|tokenless|agent-sec)"
        )
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "source"
            buildroot = Path(temporary) / "rpmroot"
            shutil.copytree(ROOT / "manifests", source / "manifests")
            (source / "target/release").mkdir(parents=True)
            binary = source / "target/release/anolisa"
            binary.write_text("#!/bin/sh\nexit 0\n")
            body = spec.split("%install\n", 1)[1].split("\n%files", 1)[0]
            for macro, value in {
                "buildroot": str(buildroot),
                "_bindir": "/usr/bin",
                "_sysconfdir": "/etc",
                "_datadir_anolisa": "/usr/share/anolisa",
            }.items():
                body = body.replace(f"%{{{macro}}}", value)
            run("bash", "-eu", "-c", body, cwd=source)
            files = {
                str(path.relative_to(buildroot)) for path in buildroot.rglob("*") if path.is_file()
            }
            expected = {
                f"usr/share/anolisa/components/{name}/component.toml" for name in COMPONENTS
            }
            self.assertEqual(files, expected | {"usr/bin/anolisa", "etc/anolisa/repo.toml"})
            for name in COMPONENTS:
                self.assertEqual(
                    (
                        buildroot / f"usr/share/anolisa/components/{name}/component.toml"
                    ).read_bytes(),
                    (REGISTRY / name / "component.toml").read_bytes(),
                )

    def test_local_and_url_bundle_install_the_same_registry(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary)
            source = workspace / "source"
            shutil.copytree(ROOT / "manifests", source / "manifests")
            (source / "templates").mkdir()
            (source / "target/release").mkdir(parents=True)
            binary = source / "target/release/anolisa"
            binary.write_text("#!/bin/sh\nexit 0\n")
            binary.chmod(0o755)
            local = workspace / "local"
            env = {**os.environ, "ANOLISA_PREFIX": str(local)}
            run(
                "bash",
                str(ROOT / "scripts/install-anolisa.sh"),
                "--from-local",
                str(source),
                env=env,
            )
            bundle = workspace / "manifests.tar.gz"
            run("bash", str(ROOT / "scripts/package-manifests.sh"), str(bundle))
            with tarfile.open(bundle) as archive:
                metadata = {name for name in archive.getnames() if name.endswith("/component.toml")}
            self.assertEqual(
                metadata, {f"manifests/components/{name}/component.toml" for name in COMPONENTS}
            )
            detached = workspace / "detached/install-anolisa.sh"
            detached.parent.mkdir()
            shutil.copyfile(ROOT / "scripts/install-anolisa.sh", detached)
            remote = workspace / "url"
            env.update(
                {
                    "ANOLISA_PREFIX": str(remote),
                    "ANOLISA_BIN_URL": binary.as_uri(),
                    "ANOLISA_MANIFEST_BUNDLE_URL": bundle.as_uri(),
                    "ANOLISA_BIN_SHA256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                    "ANOLISA_MANIFEST_BUNDLE_SHA256": hashlib.sha256(
                        bundle.read_bytes()
                    ).hexdigest(),
                }
            )
            run("bash", str(detached), "--strict", env=env)
            for name in COMPONENTS:
                member = Path(f"share/anolisa/components/{name}/component.toml")
                self.assertEqual((local / member).read_bytes(), (remote / member).read_bytes())
            self.assertFalse((remote / "share/anolisa/manifests/components").exists())
            (source / "manifests/components/sec-core/component.toml").unlink()
            missing = workspace / "missing"
            env["ANOLISA_PREFIX"] = str(missing)
            with self.assertRaises(subprocess.CalledProcessError) as failure:
                run(
                    "bash",
                    str(ROOT / "scripts/install-anolisa.sh"),
                    "--from-local",
                    str(source),
                    env=env,
                )
            self.assertIn("distribution integration contract missing", failure.exception.stderr)
            self.assertFalse(missing.exists(), "incomplete metadata must fail before promotion")


if __name__ == "__main__":
    unittest.main()
