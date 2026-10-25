#!/usr/bin/env python3
"""Exercise packaged Windows CLI, RTK and ABI3 SDK under Windows Python in Wine."""

import asyncio
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path


def run(binary: Path, *arguments: str, payload: bytes | None = None, expected: int = 0) -> bytes:
    result = subprocess.run(
        [str(binary), *arguments],
        input=payload,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
    )
    assert result.returncode == expected, (arguments, result.returncode, result.stderr)
    return result.stdout


def main() -> None:
    assert sys.platform == "win32", "this smoke test must use Windows Python"
    archives, wheels = map(Path, sys.argv[1:3])
    version = sys.argv[3]
    # Windows keeps the imported .pyd locked until Python exits; the Wine prefix owns this fixture.
    root = Path(tempfile.mkdtemp(prefix="tokenless 中文 smoke "))
    with zipfile.ZipFile(archives / f"tokenless-{version}-windows-x86_64.zip") as package:
        package.extractall(root)
    os.environ.update(
        TOKENLESS_DATA_DIR=str(root / "state"),
        TOKENLESS_COMPRESSION_ENABLED="1",
        TOKENLESS_STATS_ENABLED="1",
        TOKENLESS_AGENT_ID="windows-ci",
        TOKENLESS_SESSION_ID="cli-smoke",
    )
    cli = root / "tokenless.exe"
    assert run(cli, "--version").decode().strip() == f"tokenless {version}"
    run(cli, "invalid-smoke-command", expected=2)
    assert b"rtk" in run(root / "rtk.exe", "--version")
    assert b"rtk git status" in run(root / "rtk.exe", "rewrite", "git status", expected=3)
    description = "中文 C:\\space path\\file.txt\r\n" + "schema details " * 500
    attribution = {"agent_id": "windows-ci", "session_id": "cli-smoke", "tool_use_id": "schema"}

    def request(
        operation: str, value: dict[str, object], expected: int = 0
    ) -> dict[str, object] | bytes:
        payload = json.dumps(
            {
                "protocol_version": 2,
                "operation": operation,
                "attribution": attribution,
                "input": value,
            },
            ensure_ascii=False,
        ).encode()
        output = run(cli, "compress", payload=payload, expected=expected)
        return json.loads(output)["result"] if expected == 0 else output

    before = request(
        "before_model",
        {
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "lookup",
                        "description": description,
                        "parameters": {"type": "object", "properties": {}},
                    },
                }
            ],
            "visible_context": "",
            "capabilities": {
                "replace_tools": True,
                "recovery": {"kind": "tool", "name": "tokenless_retrieve"},
            },
        },
    )
    (marker,) = before["visible_markers"]
    recovered = request("retrieve", {"hash_or_marker": marker, "visible_markers": [marker]})
    assert recovered["payload"].encode() == description.encode()
    assert request("retrieve", {"hash_or_marker": marker, "visible_markers": []}, expected=1) == b""
    print("PASS: packaged CLI compression, Stash, byte-exact recovery and denied retrieval")

    # Validate the actual npm payload and execute its packaged binaries, without claiming Node/hook coverage.
    with tarfile.open(root / "npm" / f"anolisa-tokenless-win32-x64-{version}.tgz") as package:
        manifest = json.load(package.extractfile("package/package.json"))
        assert manifest["version"] == version
        assert manifest["os"] == ["win32"] and manifest["cpu"] == ["x64"]
        for binary in ("tokenless", "rtk"):
            payload = package.extractfile(f"package/bin/{binary}.exe").read()
            assert payload == (root / f"{binary}.exe").read_bytes()
    with tarfile.open(root / "npm" / f"anolisa-tokenless-{version}.tgz") as package:
        manifest = json.load(package.extractfile("package/package.json"))
        assert manifest["optionalDependencies"]["@anolisa/tokenless-win32-x64"] == version
        assert "package/adapters/tokenless/qwenwork/hooks/hooks.json" in package.getnames()
    print("PASS: npm package identity, Windows binaries and QwenWork resources")

    (wheel,) = wheels.glob("*-cp311-abi3-win_amd64.whl")
    site = root / "site"
    with zipfile.ZipFile(wheel) as package:
        package.extractall(site)
    sys.path.insert(0, str(site))
    asyncio.run(sdk_smoke(root, description))
    print("PASS: Windows Python ABI3 SDK, packaged RTK, Stash, authorization and statistics")


async def sdk_smoke(root: Path, description: str) -> None:
    import anolisa_tokenless as sdk_api

    sdk = sdk_api.TokenlessSdk(sdk_api.TokenlessConfig(data_dir=root / "sdk-state"))
    attribution = sdk_api.Attribution("windows-ci", "sdk-smoke", "sdk-call")
    rewritten = await sdk.pre_tool(
        sdk_api.PreToolRequest(
            "shell",
            {"command": "git status", "other": "中文"},
            "command",
            sdk_api.PreToolCapabilities(True, False),
            attribution,
        )
    )
    assert rewritten.action == sdk_api.PreToolAction.REPLACE_ARGUMENTS, rewritten
    assert rewritten.arguments["other"] == "中文"
    before = await sdk.before_model(
        sdk_api.BeforeModelRequest(
            (
                {
                    "type": "function",
                    "function": {
                        "name": "lookup",
                        "description": description,
                        "parameters": {"type": "object", "properties": {}},
                    },
                },
            ),
            "",
            sdk_api.BeforeModelCapabilities(
                True, sdk_api.RecoveryMethod.tool("tokenless_retrieve")
            ),
            attribution,
        )
    )
    (marker,) = before.visible_markers
    restored = await sdk.retrieve(
        sdk_api.RetrieveRequest(marker, before.visible_markers, attribution)
    )
    assert restored.payload.encode() == description.encode()
    try:
        await sdk.retrieve(sdk_api.RetrieveRequest(marker, frozenset(), attribution))
    except sdk_api.TokenlessError as error:
        assert "not authorized" in str(error), str(error)
    else:
        raise AssertionError("unauthorized SDK retrieval succeeded")
    records = sdk.stats.list()
    assert records and all(record.agent_id == "windows-ci" for record in records), records
    assert all(record.session_id == "sdk-smoke" for record in records), records


if __name__ == "__main__":
    main()
