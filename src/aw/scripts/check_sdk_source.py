"""Build exported SDK sources and an application outside the OS checkout."""

import json
import shutil
import tarfile
import tempfile
import tomllib
from pathlib import Path
from typing import Callable

from sdk_source import SDK, Snapshot, export


def local_packages(metadata: dict, root: Path) -> None:
    """Cargo may not resolve a local package back into the original checkout."""
    for package in metadata["packages"]:
        if package["source"] is None:
            path = Path(package["manifest_path"]).resolve()
            if not path.is_relative_to(root.resolve()):
                raise ValueError(f"exported consumer escaped its source materials: {path}")


def locked_dependencies(original: bytes, consumer: bytes) -> None:
    """An external application may add itself, not silently update SDK dependencies."""
    def identities(data: bytes) -> set[tuple]:
        return {
            (p["name"], p["version"], p.get("source"), p.get("checksum"))
            for p in tomllib.loads(data.decode())["package"]
            if p["name"] != "sdk-consumer"
        }
    if not identities(consumer) <= identities(original):
        raise ValueError("external consumer changed the SDK's locked dependency resolution")


def check(aw: Path, run: Callable) -> None:
    """Validate a committed candidate, preserving its SDK lockfile."""
    repo = aw.parents[1]
    output = aw / "target/sdk-source"
    output.mkdir(parents=True, exist_ok=True)
    # /tmp is deliberate: this acceptance must not inherit the OS checkout's
    # directory tree. Compilation output remains in the designated target.
    with tempfile.TemporaryDirectory(prefix="aw-sdk-consumer-", dir="/tmp") as temporary:
        root = Path(temporary)
        archive = root / "source.tar.gz"
        evidence = export(Snapshot(repo, "HEAD"), archive)
        duplicate = root / "repeat.tar.gz"
        if export(Snapshot(repo, evidence["source_commit"]), duplicate)["sha256"] != evidence["sha256"]:
            raise ValueError("SDK source export is not deterministic")
        with tarfile.open(archive) as stream:
            stream.extractall(root, filter="data")
        source = root / f"aw-sdk-{evidence['sdk_version']}"
        sdk = source / SDK
        cargo = ["cargo", "--config", f'build.target-dir={json.dumps(str(output / "cargo"))}']
        metadata = json.loads(run(
            [*cargo, "metadata", "--format-version", "1", "--locked"], sdk, capture=True,
        ))
        local_packages(metadata, source)
        run([*cargo, "test", "--locked"], sdk)

        consumer = root / "application"
        (consumer / "src").mkdir(parents=True)
        (consumer / "Cargo.toml").write_text(
            '[package]\nname = "sdk-consumer"\nversion = "0.0.0"\nedition = "2024"\n'
            '[workspace]\n[dependencies]\n'
            f'aw-sdk = {{ path = {json.dumps(str(sdk))}, version = "={evidence["sdk_version"]}" }}\n',
            encoding="utf-8",
        )
        (consumer / "src/main.rs").write_text(CONSUMER, encoding="utf-8")
        (consumer / "rust-toolchain.toml").write_bytes((sdk / "rust-toolchain.toml").read_bytes())
        # Applications own their lockfiles. Seed the example with the SDK's
        # tested resolution, then admit only the new application package.
        locked = (sdk / "Cargo.lock").read_bytes()
        (consumer / "Cargo.lock").write_bytes(locked)
        run([*cargo, "check", "--offline"], consumer)
        locked_dependencies(locked, (consumer / "Cargo.lock").read_bytes())
        metadata = json.loads(run(
            [*cargo, "metadata", "--format-version", "1", "--locked"], consumer, capture=True,
        ))
        local_packages(metadata, root)
        run([*cargo, "run", "--locked", "--", str(root / "state")], consumer)
        evidence.update(
            source_export_deterministic=True, sdk_tests=True,
            external_consumer=True, local_packages_within_export=True,
        )
        artifact = output / "aw-sdk-source.tar.gz"
        shutil.copyfile(archive, artifact)
        evidence["file"] = artifact.name
        (output / "result.json").write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
    if root.exists():
        raise ValueError(f"external SDK consumer cleanup failed: {root}")


CONSUMER = r'''use aw_sdk::{CallContext, Client, Config, Error, sec_core, tokenless};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let state = std::env::args_os().nth(1).ok_or("missing state directory")?;
    let client = Client::new(Config {
        sec_core_socket: Some("/does-not-exist/aw-sdk/daemon.sock".into()),
        tokenless: Some(tokenless::Config::new(std::path::PathBuf::from(state))),
    })?;
    let context = CallContext::new("external-consumer");
    let result = client.sec_core()?.scan_code(
        &sec_core::ScanRequest {
            code: "echo hello".into(), language: "bash".into(),
            rules: None, mode: Some("regex".into()),
        }, &context, Duration::from_secs(1),
    );
    if !matches!(result, Err(Error::SecCore(sec_core::ScanError::Transport(_)))) {
        return Err("missing daemon must not become a local scan".into());
    }
    let input = "  repeated whitespace is still caller content  ";
    let output = client.tokenless()?.post_tool(
        &tokenless::PostToolRequest {
            result_kind: tokenless::ResultKind::Tool, tool_name: "api".into(),
            content: input.into(), status: tokenless::ToolResultStatus::Error,
            content_origin: tokenless::ContentOrigin::ApiResponse, command: None,
            output_optimization: tokenless::OutputOptimization::None,
            capabilities: tokenless::PostToolCapabilities {
                replace_output: true, replace_with_text: true,
                recovery: tokenless::RecoveryMethod::None,
            },
        }, &context,
    )?;
    if output.output != input || output.is_applied() {
        return Err("failed tool result was modified".into());
    }
    Ok(())
}
'''
