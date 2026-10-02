#!/usr/bin/env python3
"""Run installed-package regression in existing, Agent-only Docker images."""

import argparse
import hashlib
import json
import shutil
import subprocess
import tempfile
from pathlib import Path

IMAGES = {
    "claude-code": "tokenless-test-agent-claude-code:2.1.259",
    "opencode": "tokenless-test-agent-opencode:1.18.27",
    "agentscope2": "tokenless-test-agent-agentscope2:2.0.7.post1",
}
PROJECT_COMMIT = "8877f41873e37a30258d3935feaf1d2679321735"
ROOT = Path(__file__).resolve().parents[2]


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--project",
        type=Path,
        required=True,
        help="clean path-to-regexp checkout with a captured package-lock.json",
    )
    parser.add_argument("--packages", type=Path, default=ROOT / "npm/dist")
    parser.add_argument("--wheels", type=Path, default=ROOT / "target/wheels")
    parser.add_argument("--agents", nargs="+", choices=IMAGES, default=list(IMAGES))
    parser.add_argument(
        "--api-key-file",
        type=Path,
        help="enable live Agent tasks using a read-only mounted TokenPlan key",
    )
    parser.add_argument("--model", default="deepseek-v4-flash-0731")
    args = parser.parse_args()
    project = args.project.resolve()
    revision = subprocess.check_output(
        ["git", "-C", str(project), "rev-parse", "HEAD"], text=True
    ).strip()
    if revision != PROJECT_COMMIT:
        parser.error(f"expected path-to-regexp commit {PROJECT_COMMIT}, got {revision}")
    subprocess.run(["git", "-C", str(project), "diff", "--exit-code", "HEAD"], check=True)
    lock = project / "package-lock.json"
    if not lock.is_file():
        parser.error(
            "capture package-lock.json before running; dependency resolution is not a test"
        )
    if args.api_key_file and not args.api_key_file.is_file():
        parser.error("API key file does not exist")

    output = Path(tempfile.mkdtemp(prefix="tokenless-release-regression."))
    suite = output / "suite"
    shutil.copytree(
        Path(__file__).resolve().parent, suite, ignore=shutil.ignore_patterns("__pycache__")
    )
    inputs = output / "inputs"
    inputs.mkdir()
    packages = {}
    for directory in ("tokenless", "tokenless-linux-x64"):
        candidates = list((args.packages / directory).glob("*.tgz"))
        if len(candidates) != 1:
            parser.error(f"expected exactly one built tarball in {directory}")
        source = candidates[0]
        shutil.copy2(source, inputs / source.name)
        packages[directory] = {"file": source.name, "sha256": sha256(source)}
    wheels = {}
    if "agentscope2" in args.agents:
        for package in ("anolisa_tokenless", "anolisa_tokenless_agentscope"):
            candidates = list(args.wheels.glob(f"{package}-*.whl"))
            if len(candidates) != 1:
                parser.error(f"expected exactly one built wheel for {package}")
            source = candidates[0]
            shutil.copy2(source, inputs / source.name)
            wheels[package] = {"file": source.name, "sha256": sha256(source)}
    archive = subprocess.check_output(["git", "-C", str(project), "archive", "HEAD"])
    (inputs / "project.tar").write_bytes(archive)
    shutil.copy2(lock, inputs / "package-lock.json")
    fixture = ROOT / "benchmark/l1-compressor/fixtures/record_reduction.json"
    shutil.copy2(fixture, inputs / "full-records.json")
    records = json.loads(fixture.read_text())
    # Longer messages make field-name compaction insufficient by itself. This is
    # explicitly a synthetic recovery contract, never a workload savings sample.
    for record in records:
        record["message"] *= 2
    (inputs / "records.json").write_text(json.dumps(records, separators=(",", ":")))
    manifest = {
        "tokenless_commit": subprocess.check_output(
            ["git", "-C", str(ROOT), "rev-parse", "HEAD"], text=True
        ).strip(),
        "expected_binary_sha256": sha256(ROOT / "target/release/tokenless"),
        "expected_rtk_sha256": sha256(ROOT / "third_party/rtk/target/release/rtk"),
        "suite_sha256": {path.name: sha256(path) for path in suite.glob("*.py")},
        "project_url": "https://github.com/pillarjs/path-to-regexp",
        "project_commit": revision,
        "dependency_lock_sha256": sha256(lock),
        "packages": packages,
        "wheels": wheels,
        "model": args.model,
        "live_requested": bool(args.api_key_file),
        "agents": {},
    }
    (inputs / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"Results: {output}", flush=True)
    failed = False
    for agent in args.agents:
        image = IMAGES[agent]
        image_id = subprocess.check_output(
            ["docker", "image", "inspect", image, "--format", "{{.Id}}"], text=True
        ).strip()
        result_dir = output / agent
        result_dir.mkdir()
        name = f"{output.name}-{agent}".lower()
        command = [
            "docker",
            "run",
            "--rm",
            "--name",
            name,
            "--mount",
            f"type=bind,src={inputs},dst=/inputs,readonly",
            "--mount",
            f"type=bind,src={result_dir},dst=/results",
            "--mount",
            f"type=bind,src={suite},dst=/suite,readonly",
        ]
        if args.api_key_file:
            command += [
                "--mount",
                f"type=bind,src={args.api_key_file.resolve()},dst=/run/tokenplan-key,readonly",
            ]
        if agent == "agentscope2":
            command += [image, "python3", "/suite/agentscope_probe.py"]
        else:
            command += [image, "python3", "/suite/probe.py", agent]
        print(f"Running {agent}: {image_id}", flush=True)
        try:
            with (result_dir / "container.log").open("w") as log:
                completed = subprocess.run(
                    command, stdout=log, stderr=subprocess.STDOUT, timeout=1500
                )
            code = completed.returncode
        except subprocess.TimeoutExpired:
            subprocess.run(
                ["docker", "stop", "--time", "5", name], capture_output=True, check=False
            )
            code = 124
        manifest["agents"][agent] = {"image": image, "image_id": image_id, "exit_code": code}
        (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        failed |= code != 0
        print(f"{agent}: {'PASS' if code == 0 else 'FAIL'} (see {result_dir})", flush=True)
    raise SystemExit(int(failed))


if __name__ == "__main__":
    main()
