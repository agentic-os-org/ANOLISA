#!/usr/bin/env python3
"""Container-side checks; Tokenless is installed exclusively from supplied npm tarballs."""

import csv
import hashlib
import io
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
from pathlib import Path

RESULTS = Path("/results")
PROJECT = RESULTS / "project"
MANIFEST = json.loads(Path("/inputs/manifest.json").read_text())
REFERENCE = re.compile(r"If needed, run in shell: tokenless retrieve ([0-9a-f]{24})(?![\w-])")
REPORT = {"checks": [], "live": []}
SECRET = ""


def run(
    command: list[str],
    *,
    env: dict | None = None,
    data: bytes | None = None,
    timeout: int = 180,
    check: bool = True,
) -> subprocess.CompletedProcess:
    process = subprocess.run(
        command, input=data, capture_output=True, env=env, cwd=PROJECT, timeout=timeout
    )
    if check and process.returncode:
        raise RuntimeError(
            f"{command[0]} exited {process.returncode}: "
            f"{process.stderr.decode(errors='replace')[-2000:]}"
        )
    return process


def save(path: Path, value: object) -> None:
    text = json.dumps(value, ensure_ascii=False, indent=2) + "\n"
    path.write_text(text.replace(SECRET, "[REDACTED]") if SECRET else text)


def state_env(directory: Path) -> dict:
    directory.mkdir(parents=True, exist_ok=True)
    return {
        **os.environ,
        "TOKENLESS_DATA_DIR": str(directory),
        "TOKENLESS_STATS_ENABLED": "1",
        "TOKENLESS_SLS_ENABLED": "0",
        "TOKENLESS_COMPRESSION_ENABLED": "1",
    }


def request(
    operation: str, payload: dict, env: dict, *, check: bool = True
) -> subprocess.CompletedProcess:
    envelope = {
        "protocol_version": 2,
        "operation": operation,
        "attribution": {
            "agent_id": "release-regression",
            "session_id": "core",
            "tool_use_id": "call-1",
        },
        "input": payload,
    }
    return run(["tokenless", "compress"], env=env, data=json.dumps(envelope).encode(), check=check)


def post_tool(content: str, env: dict, **overrides: object) -> dict:
    payload = {
        "result_kind": "tool",
        "tool_name": "Bash",
        "content": content,
        "status": "success",
        "content_origin": "command_output",
        "output_optimization": "none",
        "capabilities": {
            "replace_output": True,
            "replace_with_text": True,
            "recovery": {"kind": "shell"},
        },
    }
    payload.update(overrides)
    return json.loads(request("post_tool", payload, env).stdout)["result"]


def rows(directory: Path, database: str, query: str) -> list[dict]:
    # Passthrough without recovery need not initialize either store.
    if not (directory / database).is_file():
        return []
    with sqlite3.connect(f"file:{directory / database}?mode=ro", uri=True) as connection:
        connection.row_factory = sqlite3.Row
        return [dict(row) for row in connection.execute(query)]


def core_checks(raw_log: str, failure_log: str, records: str) -> None:
    missing_command = run(["bash", "-c", "tokenless-release-missing-command"], check=False)
    assert missing_command.returncode == 127
    full_records = json.dumps(
        json.loads(Path("/inputs/full-records.json").read_text()), separators=(",", ":")
    )
    cases = [
        ("build_success", raw_log, {}, "build_log_reduction"),
        (
            "search_path_sharing",
            "".join(f"crates/long_directory/src/file.rs:{i}:match-{i}\r\n" for i in range(100)),
            {"tool_name": "Grep", "content_origin": "api_response"},
            "search_path_sharing",
        ),
        ("full_toon", full_records, {}, "toon"),
        ("records_contract", records, {}, "json_record_reduction"),
        (
            "tabular_full",
            '"name","value"\r\n' + '"alpha","001"\r\n' * 50,
            {},
            "tabular_compaction",
        ),
        ("tool_error", failure_log, {"status": "error"}, None),
        ("tool_error_diagnosis", missing_command.stderr.decode(), {"status": "error"}, None),
        (
            "file_content",
            (PROJECT / "Readme.md").read_text(),
            {"content_origin": "file_content"},
            None,
        ),
        ("plain_text", (PROJECT / "Readme.md").read_text(), {}, None),
        (
            "no_savings",
            json.dumps({"message": "ordinary text " * 25}, separators=(",", ":")),
            {
                "capabilities": {
                    "replace_output": True,
                    "replace_with_text": False,
                    "recovery": {"kind": "shell"},
                }
            },
            None,
        ),
        ("rtk_bypass", raw_log, {"output_optimization": "rtk"}, None),
        (
            "without_recovery",
            raw_log,
            {
                "capabilities": {
                    "replace_output": True,
                    "replace_with_text": True,
                    "recovery": {"kind": "none"},
                }
            },
            None,
        ),
    ]
    for name, delimiter in (("tabular_csv", ","), ("tabular_tsv", "\t")):
        content = f"id{delimiter}amount{delimiter}message\r\n" + "".join(
            f"{index:04}{delimiter}{index * 7 + 11}{delimiter}record-{index}-{'payload ' * 12}\r\n"
            for index in range(100)
        )
        cases.append((name, content, {}, "tabular_row_reduction"))
    for name, delimiter in (("tabular_full_bom_csv", ","), ("tabular_full_bom_tsv", "\t")):
        content = f'"\ufeffid"{delimiter}"name"\r\n' + f'"001"{delimiter}"alpha"\r\n' * 50
        cases.append((name, content, {}, "tabular_compaction"))
    for name, template in (
        ("source_definitions", "def function_{i:03}(a, b): return a + b"),
        ("source_calls", "assert_equal(expected_value_number_{i}, actual_value_number_{i})"),
        (
            "comma_prose",
            "- Note {i}, the parser rejected the header row while processing request {i} in the ingestion pipeline",
        ),
    ):
        content = "\n".join(template.format(i=i) for i in range(100))
        cases.append((name, content, {"content_origin": "api_response"}, None))
    for name, content, overrides, operation in cases:
        directory = RESULTS / "core" / name
        env = state_env(directory)
        result = post_tool(content, env, **overrides)
        save(directory / "response.json", result)
        stats = rows(directory, "stats.db", "SELECT * FROM stats")
        assert len(stats) == (1 if operation else 0), f"{name}: unexpected compression statistics"
        keys = result["stash_keys"]
        if operation:
            assert result["disposition"] == "applied", (name, result["disposition"])
            assert operation in result["applied_operations"], (name, result["applied_operations"])
            assert result["recoverability"] == (
                "lossless"
                if name in ("full_toon", "search_path_sharing") or name.startswith("tabular_full")
                else "retrievable"
            )
            assert result["after_tokens"] < result["before_tokens"]
            assert set(keys) == set(REFERENCE.findall(result["output"]))
            assert "<<tokenless:" not in result["output"]
            stashed = rows(directory, "stash.db", "SELECT hash, payload FROM stash")
            assert len(stashed) == len(keys) == (stats[0]["stash_writes"] or 0)
            if name == "search_path_sharing":
                lines = result["output"].splitlines(keepends=True)
                assert lines[0] == "[Search results grouped by file; all received lines retained]\n"
                path = json.loads(lines[1].removeprefix("File="))
                assert "".join(path + ":" + line for line in lines[2:]) == content
                assert not keys
            elif name == "records_contract":
                REPORT["record_target"] = next(
                    record["id"]
                    for record in json.loads(content)[4:-4]
                    if record["status"] == "ok"
                    and f'request-{record["id"]} ' not in result["output"]
                )
            elif name.startswith("tabular_full"):
                delimiter = "\t" if name.endswith("_tsv") else ","
                # Match CSV readers that consume a document BOM, but preserve
                # U+FEFF inside a quoted first field as cell data.
                output = result["output"].encode().decode("utf-8-sig")
                assert list(csv.reader(io.StringIO(output), delimiter=delimiter)) == list(
                    csv.reader(io.StringIO(content), delimiter=delimiter)
                ), "full table compaction changed cells"
            elif name in ("tabular_csv", "tabular_tsv"):
                (directory / "input.txt").write_bytes(content.encode())
                REPORT[name] = {
                    "target": next(
                        index
                        for index in range(4, 96)
                        if f"record-{index}-" not in result["output"]
                    ),
                    "total": sum(index * 7 + 11 for index in range(100)),
                }
            for entry in stashed:
                marker = entry["hash"]
                denied = request(
                    "retrieve", {"hash_or_marker": marker, "visible_markers": []}, env, check=False
                )
                assert denied.returncode == 1 and not denied.stdout
                retrieved = run(["tokenless", "retrieve", marker], env=env).stdout
                assert retrieved == entry["payload"].encode(), "CLI retrieval changed bytes"
                bypass = post_tool(retrieved.decode(), env, result_kind="retrieve")
                assert bypass["output"].encode() == retrieved and not bypass["applied_operations"]
                if name == "records_contract":
                    assert json.loads(retrieved) == json.loads(
                        content
                    ), "full array recovery failed"
                elif name.startswith("tabular_"):
                    assert retrieved == content.encode(), "full table recovery changed bytes"
                    assert result["content_type"] == "tabular"
                    assert "Incomplete table" in result["output"]
                else:
                    assert entry["payload"] in content, "stash payload is not a native log interval"
            hits = rows(directory, "stats.db", "SELECT * FROM retrieve_events")
            assert len(hits) == len(keys) and all(row["outcome"] == "hit" for row in hits)
            after = rows(directory, "stash.db", "SELECT hash, payload FROM stash")
            assert after == stashed, "Retrieve unexpectedly wrote Stash"
        else:
            assert result["output"] == content, name
            assert not result["applied_operations"] and not keys, name
            assert not rows(directory, "stash.db", "SELECT hash FROM stash"), name
            if name == "tool_error":
                assert result["disposition"] == "tool_error"
            if name == "tool_error_diagnosis":
                assert "ENV_DEPENDENCY_MISSING" in result["additional_context"]
            if name == "no_savings":
                assert result["disposition"] == "no_savings"
        REPORT["checks"].append(
            {
                "case": name,
                "status": "passed",
                "disposition": result["disposition"],
                "before_tokens": result["before_tokens"],
                "after_tokens": result["after_tokens"],
                "stash_entries": len(keys),
            }
        )
        print(f"Core {name}: passed", flush=True)
    directory = RESULTS / "core/dry_run"
    env = {**state_env(directory), "TOKENLESS_COMPRESSION_ENABLED": "0"}
    result = post_tool(records, env)
    save(directory / "response.json", result)
    assert result["disposition"] == "dry_run" and result["output"] == records
    assert not rows(directory, "stash.db", "SELECT hash FROM stash")
    REPORT["checks"].append({"case": "dry_run", "status": "passed", "stash_entries": 0})


def tool_events(agent: str, events: list[dict]) -> tuple[dict, dict, str, dict]:
    calls, results, usage = {}, {}, {}
    final = ""
    for event in events:
        if agent == "claude-code":
            for block in event.get("message", {}).get("content", []):
                if not isinstance(block, dict):
                    continue
                if block.get("type") == "tool_use":
                    calls[block["id"]] = block.get("input", {}).get("command", "")
                elif block.get("type") == "tool_result":
                    content = block.get("content", "")
                    if isinstance(content, list):
                        content = "".join(
                            part["text"] for part in content if part.get("type") == "text"
                        )
                    results[block["tool_use_id"]] = content
            if event.get("type") == "result":
                assert not event.get("is_error"), event.get("result", event.get("subtype"))
                final, usage = event.get("result", ""), event.get("usage", {})
        else:
            part = event.get("part", {})
            if (
                event.get("type") == "tool_use"
                and part.get("state", {}).get("status") == "completed"
            ):
                state = part["state"]
                calls[part["callID"]] = state.get("input", {}).get("command", "")
                results[part["callID"]] = state.get("output", "")
            elif event.get("type") == "text":
                final += part.get("text", "")
            elif event.get("type") == "step_finish":
                usage.setdefault("steps", []).append(part.get("tokens", {}))
            elif event.get("type") == "error":
                raise RuntimeError(str(event.get("error")))
    return calls, results, final, usage


def live_check(agent: str, case: str, prompt: str, expected_operation: str) -> None:
    directory = RESULTS / "live" / case
    env = state_env(directory)
    model = MANIFEST["model"]
    if agent == "claude-code":
        env.update(
            {
                "ANTHROPIC_BASE_URL": "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
                "ANTHROPIC_AUTH_TOKEN": SECRET,
                "ANTHROPIC_MODEL": model,
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
            }
        )
        command = [
            "claude",
            "-p",
            prompt,
            "--model",
            model,
            "--verbose",
            "--output-format",
            "stream-json",
            "--dangerously-skip-permissions",
            "--max-turns",
            "6",
            "--tools",
            "Bash",
        ]
    else:
        env["TOKENPLAN_API_KEY"] = SECRET
        command = [
            "opencode",
            "run",
            "--dir",
            str(PROJECT),
            "--format",
            "json",
            "--model",
            f"tokenplan/{model}",
            "--auto",
            prompt,
        ]
    try:
        completed = run(command, env=env, timeout=300, check=False)
    except subprocess.TimeoutExpired as error:
        for label, content in (("stdout", error.stdout), ("stderr", error.stderr)):
            (directory / f"agent.{label}").write_text(
                (content or b"").decode(errors="replace").replace(SECRET, "[REDACTED]")
            )
        raise
    for label, content in (("stdout", completed.stdout), ("stderr", completed.stderr)):
        (directory / f"agent.{label}").write_text(
            content.decode(errors="replace").replace(SECRET, "[REDACTED]")
        )
    assert completed.returncode == 0, f"Agent exited {completed.returncode}; see agent.stderr"
    events = [json.loads(line) for line in completed.stdout.splitlines() if line.strip()]
    calls, outputs, final, usage = tool_events(agent, events)
    assert final, "Agent did not produce a final answer"
    assert (directory / "stats.db").is_file(), "Core was not invoked; inspect installed Agent hooks"
    stats = rows(directory, "stats.db", "SELECT * FROM stats WHERE applied_operations IS NOT NULL")
    applied = [row for row in stats if expected_operation in json.loads(row["applied_operations"])]
    assert len(applied) == 1, f"expected one {expected_operation} statistic, got {len(applied)}"
    compressed = applied[0]
    visible = outputs.get(compressed["tool_use_id"], "")
    assert isinstance(visible, str) and REFERENCE.search(
        visible
    ), "no recovery instruction in model-visible tool output"
    assert "<<tokenless:" not in visible
    stashed = rows(directory, "stash.db", "SELECT hash, payload FROM stash")
    assert len(stashed) == compressed["stash_writes"] == 1
    entry = stashed[0]
    assert entry["hash"] in visible
    retrieval_ids = [
        call_id
        for call_id, command in calls.items()
        if re.fullmatch(
            r"tokenless retrieve (?:[0-9a-f]{24}|'[0-9a-f]{24}'|\"[0-9a-f]{24}\")", command.strip()
        )
    ]
    assert len(retrieval_ids) == 1, f"expected one standalone Retrieve call, got {retrieval_ids}"
    restored = outputs[retrieval_ids[0]]
    # Record host newline normalization separately from exact CLI recovery.
    assert restored.replace("\r\n", "\n").rstrip("\n") == entry["payload"].replace(
        "\r\n", "\n"
    ).rstrip("\n"), "Agent did not receive the recovered payload"
    byte_env = {**env, "TOKENLESS_STATS_ENABLED": "0"}
    cli_payload = run(["tokenless", "retrieve", entry["hash"]], env=byte_env).stdout
    assert cli_payload == entry["payload"].encode(), "installed CLI changed retrieved bytes"
    hits = rows(directory, "stats.db", "SELECT * FROM retrieve_events")
    assert len(hits) == 1 and hits[0]["outcome"] == "hit" and hits[0]["source"] == "cli"
    retrieve_stats = [row for row in stats if row["tool_use_id"] == retrieval_ids[0]]
    assert not retrieve_stats, "Retrieve output was counted as another compression"
    if case == "build_log":
        assert "484" in final
        recovered_names = re.findall(r"redos > (.*?) - /", entry["payload"])
        assert any(
            name in final and name not in visible for name in recovered_names
        ), "final answer lacks an omitted test"
    elif case.startswith("tabular_"):
        target = f'record-{REPORT[case]["target"]}-'
        assert target in final and target not in visible
        assert str(REPORT[case]["total"]) in final.replace(",", ""), "wrong full-table sum"
        assert "Incomplete table" in visible
    else:
        target = f'request-{REPORT["record_target"]} '
        assert target.strip() in final and target not in visible
    gross = compressed["before_tokens"] - compressed["after_tokens"]
    REPORT["live"].append(
        {
            "case": case,
            "status": "passed",
            "command": command,
            "tool_calls": calls,
            "model_visible_compressed_output": visible,
            "applied_operations": json.loads(compressed["applied_operations"]),
            "gross_saved_tokens": gross,
            "retrieved_tokens": hits[0]["payload_tokens"],
            "saved_minus_retrieved_tokens": gross - hits[0]["payload_tokens"],
            "cli_retrieve_byte_exact": True,
            "model_tool_payload_byte_exact": restored == entry["payload"],
            "provider_usage": usage,
            "final_answer": final,
        }
    )
    print(f"Live {agent}/{case}: passed", flush=True)


def main() -> None:
    global SECRET
    agent = sys.argv[1]
    PROJECT.mkdir()
    run(["tar", "xf", "/inputs/project.tar", "-C", str(PROJECT)])
    shutil.copy2("/inputs/package-lock.json", PROJECT / "package-lock.json")
    prefix = Path.home() / ".local"
    packages = [str(Path("/inputs") / value["file"]) for value in MANIFEST["packages"].values()]
    installed = run(
        [
            "npm",
            "install",
            "--global",
            "--prefix",
            str(prefix),
            "--offline",
            "--no-audit",
            "--no-fund",
            *packages,
        ]
    )
    (RESULTS / "install.log").write_bytes(installed.stdout + installed.stderr)
    os.environ["PATH"] = f'{prefix / "bin"}:{os.environ["PATH"]}'
    binary = Path(shutil.which("tokenless")).resolve()
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == MANIFEST["expected_binary_sha256"]
    rtk = Path(shutil.which("rtk")).resolve()
    assert hashlib.sha256(rtk.read_bytes()).hexdigest() == MANIFEST["expected_rtk_sha256"]
    adapters = prefix / "share/anolisa/adapters/tokenless"
    assert (adapters / "manifest.json").is_file()
    for bundled_agent in ("claude-code", "qwencode"):
        dispatcher = adapters / bundled_agent / "hooks/run-hook.sh"
        assert (
            dispatcher.is_file() and not dispatcher.is_symlink()
        ), f"missing packaged {bundled_agent} hook"
    REPORT["installation"] = {
        "binary": str(binary),
        "sha256": MANIFEST["expected_binary_sha256"],
        "version": run(["tokenless", "--version"]).stdout.decode().strip(),
        "rtk_version": run(["rtk", "--version"]).stdout.decode().strip(),
        "agent_version": run(["claude" if agent == "claude-code" else agent, "--version"])
        .stdout.decode()
        .strip(),
    }
    install_plugin = run(["bash", str(adapters / agent / "scripts/install.sh")])
    (RESULTS / "plugin-install.log").write_bytes(install_plugin.stdout + install_plugin.stderr)
    os.environ.update({"NO_COLOR": "1", "npm_config_update_notifier": "false"})
    dependencies = run(["npm", "ci", "--no-audit", "--no-fund"], timeout=300)
    (RESULTS / "dependency-install.log").write_bytes(dependencies.stdout + dependencies.stderr)
    baseline = run(["npm", "test"], timeout=180)
    raw_log = (baseline.stdout + baseline.stderr).decode()
    (RESULTS / "build-success.log").write_text(raw_log)
    assert "484 passed" in raw_log, "pinned real project tests did not all pass"
    failure = run(
        ["npm", "exec", "--", "vitest", "run", "--config", "missing.config.ts"], check=False
    )
    assert failure.returncode != 0
    failure_log = (failure.stdout + failure.stderr).decode()
    (RESULTS / "build-failure.log").write_text(failure_log)
    records = json.dumps(
        json.loads(Path("/inputs/records.json").read_text()), separators=(",", ":")
    )
    core_checks(raw_log, failure_log, records)
    if not MANIFEST["live_requested"]:
        REPORT["live"].append({"status": "not_run", "reason": "no API key file supplied"})
        return
    SECRET = Path("/run/tokenplan-key").read_text().strip()
    assert SECRET, "API key file is empty"
    if agent == "opencode":
        config = Path.home() / ".config/opencode/opencode.json"
        save(
            config,
            {
                "provider": {
                    "tokenplan": {
                        "npm": "@ai-sdk/openai-compatible",
                        "options": {
                            "baseURL": "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
                            "apiKey": "{env:TOKENPLAN_API_KEY}",
                        },
                        "models": {
                            MANIFEST["model"]: {
                                "name": MANIFEST["model"],
                                "limit": {"context": 128000, "output": 8192},
                            }
                        },
                    }
                },
                "permission": {"*": "deny", "bash": "allow"},
            },
        )
    prompts = [
        (
            "build_log",
            "Run exactly `npm test` in the current project using the shell tool, once. "
            "Do not modify files, redirect output, pipe, or rerun tests. Report test totals. "
            "If output includes a recovery instruction, execute its standalone retrieve command "
            "exactly once and cite one concrete slow redos test from the recovered omitted lines. "
            "Do not read logs or databases directly.",
            "build_log_reduction",
        ),
        (
            "records",
            "Run exactly `node -e 'process.stdout.write(JSON.stringify(require(\"/inputs/records.json\")))'` "
            "using the shell tool. This is a synthetic recovery contract, "
            "not real workload data. Do not filter, redirect or pipe the initial output. "
            "Follow the recovery instruction exactly once to recover the full array. "
            f'Report the message for id {REPORT["record_target"]}. '
            "Do not read the input file or database by another method.",
            "json_record_reduction",
        ),
    ]
    for case, format_name in (("tabular_csv", "CSV"), ("tabular_tsv", "TSV")):
        prompts.append(
            (
                case,
                "Run exactly `node -e 'process.stdout.write(require(\"fs\").readFileSync(\""
                f"/results/core/{case}/input.txt"
                "\"))'` using the shell tool. "
                f"This is a synthetic {format_name} recovery contract. "
                "Do not filter, redirect or pipe the initial output. "
                "Follow the recovery instruction exactly once to recover the full table. "
                f'Report the message for id {REPORT[case]["target"]:04}, '
                "and the sum of the amount column over ALL 100 data rows. "
                "Do not read the input file or database by another method. "
                "Do not calculate from a partial table.",
                "tabular_row_reduction",
            )
        )
    for case, prompt, operation in prompts:
        try:
            live_check(agent, case, prompt, operation)
        except (AssertionError, RuntimeError, subprocess.TimeoutExpired) as error:
            REPORT["live"].append({"case": case, "status": "failed", "error": str(error)})
            print(f"Live {agent}/{case}: failed; see report.json", flush=True)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        REPORT["error"] = str(error)
    finally:
        save(RESULTS / "report.json", REPORT)
    raise SystemExit(
        int("error" in REPORT or any(item["status"] == "failed" for item in REPORT["live"]))
    )
