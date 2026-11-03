"""Run the native C consumer against a simulated FFI library and real HTTPS load."""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

from test_resource_run import (
    SINGLE_RUN,
    assert_reaped,
    command,
    environment,
    executable,
    fixture_repo,
    git,
    run,
)
import collect_metrics
import resource_inputs
import resource_report
import validate_results
from test_resource_report import artifacts

FAKE_LIBRARY = r"""
#define _POSIX_C_SOURCE 200809L
#include "agentsight.h"
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/wait.h>
#include <unistd.h>
static int fd;
static pid_t child;
static FILE *pending;
AgentsightConfigHandle *agentsight_config_new(void) { return (void *)1; }
int agentsight_config_load_config(AgentsightConfigHandle *cfg, const char *json) {
    (void)cfg; return json && json[0] == '{' ? 0 : -1;
}
void agentsight_config_free(AgentsightConfigHandle *cfg) { (void)cfg; }
AgentsightHandle *agentsight_new(AgentsightConfigHandle *cfg) {
    (void)cfg; fd = eventfd(0, EFD_NONBLOCK); return (void *)1;
}
int agentsight_get_eventfd(AgentsightHandle *h) { (void)h; return fd; }
int agentsight_start(AgentsightHandle *h) {
    (void)h;
    char value[32]; snprintf(value, sizeof(value), "%d", fd);
    setenv("FFI_TEST_FD", value, 1);
    pending = fopen(getenv("FFI_TEST_PENDING"), "r");
    if (!pending) return -1;
    child = fork();
    if (child < 0) return -1;
    if (child == 0) {
        execl(getenv("PYTHON"), getenv("PYTHON"), getenv("FFI_TEST_OBSERVER"),
              "trace", "--config", getenv("FFI_TEST_CONFIG"), (char *)NULL);
        _exit(127);
    }
    return 0;
}
int agentsight_stop(AgentsightHandle *h) {
    (void)h; kill(child, SIGINT); waitpid(child, NULL, 0); return 0;
}
void agentsight_free(AgentsightHandle *h) { (void)h; if (pending) fclose(pending); close(fd); }
const char *agentsight_version(void) { return "simulated-ffi"; }
const char *agentsight_last_error(void) { return "simulated failure"; }
int agentsight_read(AgentsightHandle *h, agentsight_https_callback_fn http_cb, void *http_ud,
                    agentsight_llm_callback_fn cb, void *ud, int flags) {
    (void)h; (void)http_cb; (void)http_ud; (void)flags;
    if (getenv("FFI_TEST_READ_ERROR")) return -1;
    uint64_t count;
    if (read(fd, &count, sizeof(count)) < 0) return 0;
    if (!cb) return -1;
    AgentsightLLMData data = {0};
    for (uint64_t i = 0; i < count; ++i) {
        char id[256], message[512]; unsigned int tokens;
        clearerr(pending);
        if (fscanf(pending, "%255s %u", id, &tokens) != 2) return -1;
        snprintf(message, sizeof(message), "[{\"content\":\"[agentsight-benchmark:%s] hello\"}]", id);
        data.request_messages = message;
        data.response_id = "unrelated-generated-id";
        data.status_code = 200; data.total_tokens = tokens; data.llm_usage = true;
        cb(&data, ud);
    }
    return (int)count;
}
int agentsight_read_v2(AgentsightHandle *h, agentsight_https_callback_fn http_cb, void *http_ud,
                       agentsight_llm_callback_fn llm_cb, void *llm_ud,
                       agentsight_event_callback_fn cb, void *ud, int flags) {
    (void)h; (void)http_ud; (void)llm_ud; (void)flags;
    uint64_t count;
    if (read(fd, &count, sizeof(count)) < 0) return 0;
    if (!cb || http_cb || llm_cb) return -1;
    for (uint64_t i = 0; i < count; ++i) {
        char id[256], json[1024]; unsigned int tokens;
        clearerr(pending);
        if (fscanf(pending, "%255s %u", id, &tokens) != 2) return -1;
        int length = snprintf(json, sizeof(json),
            "{\"request\":{\"messages\":[{\"content\":\"[agentsight-benchmark:%s] hello\"}]},"
            "\"metadata\":{\"status_code\":\"200\",\"response_id\":\"unrelated-generated-id\"%s},"
            "\"token_usage\":{\"total_tokens\":%u}}", id,
            getenv("FFI_TEST_INCOMPLETE") && strncmp(id, "preflight-", 10)
                ? ",\"incomplete\":\"true\"" : "", tokens);
        AgentsightEvent event = {.event_type = AGENTSIGHT_EVENT_TYPE_LLM,
                                .payload_json = json, .payload_json_len = (uint32_t)length};
        cb(&event, ud);
    }
    return (int)count;
}
"""

FAKE_FFI_PRODUCER = """#!/usr/bin/env python3
import json, os, signal, sys, time
from pathlib import Path
configuration = json.loads(Path(os.environ['FFI_TEST_CONFIG']).read_text())
directory = Path(configuration['storage']['base_path']).parent
job = json.loads((directory / 'job.json').read_text())
total = int(job['load_args'][job['load_args'].index('--tokens-per-response') + 1]) + 12
Path(os.environ['AGENTSIGHT_METRICS_FILE']).write_text('agentsight_channel_dropped_total 0\\n')
signal.signal(signal.SIGINT, lambda *_: sys.exit(0))
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
print('AgentSight initialized: simulated FFI without SQLite', flush=True)
seen = set()
with open(os.environ['FFI_TEST_PENDING'], 'a', buffering=1) as pending:
    while True:
        for path in directory.glob('request-logs/*.jsonl'):
            for line in path.read_text().splitlines():
                request_id = json.loads(line)['request_id']
                if request_id in seen:
                    continue
                seen.add(request_id)
                tokens = 13 if request_id.startswith('preflight-') else total
                if request_id.startswith('preflight-') and os.environ.get('FFI_TEST_WRONG_TOKENS'):
                    tokens += 1
                pending.write(f'{request_id} {tokens}\\n')
                os.write(int(os.environ['FFI_TEST_FD']), (1).to_bytes(8, sys.byteorder))
        time.sleep(0.02)
"""


@pytest.mark.parametrize(
    "api,failure",
    [("typed", ""), ("v2", ""), ("typed", "read"), ("typed", "tokens"), ("v2", "incomplete")],
)
def test_native_ffi_host_reuses_load_capture_and_resource_reporting(
    tmp_path: Path, environment: dict[str, str], api: str, failure: str
) -> None:
    compiler = shutil.which("cc")
    if not compiler:
        pytest.skip("C compiler unavailable")
    source = tmp_path / "fake.c"
    source.write_text(FAKE_LIBRARY)
    library = tmp_path / "original.so"
    subprocess.run(
        [
            compiler,
            "-shared",
            "-fPIC",
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-I",
            str(SINGLE_RUN.parents[2] / "include"),
            str(source),
            "-o",
            str(library),
        ],
        check=True,
    )
    observer = executable(tmp_path / "observer.py", FAKE_FFI_PRODUCER)
    pending = tmp_path / "pending-events"
    pending.touch()
    output = tmp_path / "result"
    args = command(output, library)
    index = args.index("--agentsight-bin")
    args[index] = "--candidate-lib"
    configuration = json.loads((SINGLE_RUN.parents[2] / "agentsight.json").read_text())
    configuration["features"]["sqlite_storage"]["enabled"] = False
    config = tmp_path / "config.json"
    config.write_text(json.dumps(configuration))
    args.extend(["--config", str(config)])
    poison = tmp_path / "other-libraries"
    poison.mkdir()
    (poison / "libagentsight.so").write_text("must not load this library")
    test_env = dict(
        environment,
        FFI_TEST_OBSERVER=str(observer),
        FFI_TEST_CONFIG=str(output / "run/config.json"),
        FFI_TEST_PENDING=str(pending),
        LD_LIBRARY_PATH=str(poison),
    )
    if failure == "read":
        test_env["FFI_TEST_READ_ERROR"] = "1"
    elif failure == "tokens":
        test_env["FFI_TEST_WRONG_TOKENS"] = "1"
        args.extend(["--ready-timeout", "2"])
    elif failure == "incomplete":
        test_env["FFI_TEST_INCOMPLETE"] = "1"
    result = run(
        args + ["--ffi", "--ffi-api", api, "--duration", "4", "--startup-delay", "1"],
        test_env,
    )
    if failure == "tokens":
        assert result.returncode != 0, result.stdout + result.stderr
        readiness = json.loads((output / "run/capture-readiness.json").read_text())
        assert readiness["matched"] == 2 and readiness["token_correct"] == 0
        assert not (output / "run/agents/start.json").exists()
        assert_reaped(output)
        return
    if failure == "read":
        assert result.returncode != 0, result.stdout + result.stderr
        summary = json.loads((output / "resource-summary.json").read_text())
        assert not summary["valid"] and summary["runs"]["run"]["ffi_error"]
        assert not summary["runs"]["run"]["ffi_consumer"]["completed"]
        assert "FFI read failed" in (output / "run/trace.log").read_text()
        assert_reaped(output)
        return
    if failure == "incomplete":
        assert result.returncode != 0, result.stdout + result.stderr
        summary = json.loads((output / "resource-summary.json").read_text())
        measured = summary["runs"]["run"]
        assert not summary["valid"] and measured["capture"] == "FAIL"
        assert measured["capture_counts"]["matched"] == measured["capture_counts"]["sent"] > 0
        assert measured["capture_counts"]["complete"] == 0
        assert measured["capture_counts"]["token_correct"] == 0
        assert measured["capture_completeness"] == 0
        assert measured["ffi_consumer"]["completed"] and measured["ffi_error"] is None
        assert_reaped(output)
        return
    assert result.returncode == 0, result.stdout + result.stderr
    summary = json.loads((output / "resource-summary.json").read_text())
    measured = summary["runs"]["run"]
    assert summary["valid"] and summary["settings"]["mode"] == "ffi"
    assert measured["ffi_consumer"]["api"] == api
    assert measured["ffi_consumer"]["llm_events"] == measured["capture_counts"]["sent"] + 2
    assert (measured["ffi_consumer"]["json_events"] > 0) == (api == "v2")
    assert (
        measured["ffi_consumer"]["pid"]
        == json.loads((output / "run/tracer-ready.json").read_text())["pid"]
    )
    assert measured["rss_peak_mib"] > 0 and measured["cpu_samples"] > 0
    assert not (output / "run/db").exists()
    assert json.loads((output / "run/report.json").read_text())["capture_source"] == "ffi_callbacks"
    assert measured["source"]["library_sha256"] == resource_inputs.digest(library)
    assert measured["source"]["sha256"] == resource_inputs.digest(output / "run/agentsight")
    assert (output / "run/libagentsight.so").read_bytes() == library.read_bytes()
    assert "Scope: minimal C FFI host" in result.stdout
    assert_reaped(output)


@pytest.mark.parametrize(
    "failure", ["missing-stats", "lost-events", "queue-drops", "late-probe-drops", "exit-failure"]
)
def test_ffi_failure_invalidates_an_otherwise_valid_capture(tmp_path: Path, failure: str) -> None:
    directory = artifacts(tmp_path, "run", 25, 40)
    (directory / "clients-ready.json").write_text('{"request_ids":["preflight-0"]}')
    (directory / "worker-result.json").write_text(
        json.dumps(
            {
                "status": "complete",
                "returncode": 0,
                "trace_survived": True,
                "trace_returncode": 1 if failure == "exit-failure" else 0,
            }
        )
    )
    if failure != "missing-stats":
        (directory / "ffi-consumer.json").write_text(
            json.dumps(
                {
                    "completed": True,
                    "llm_events": 1 if failure == "lost-events" else 2,
                }
            )
        )
    (directory / "internal-metrics.prom").write_text(
        f"agentsight_channel_dropped_total {1 if failure == 'late-probe-drops' else 0}\n"
        f"agentsight_ffi_dropped_total{{reason=\"bytes\"}} {1 if failure == 'queue-drops' else 0}\n"
        'agentsight_ffi_dropped_total{reason="count"} 0\n'
    )
    assert not resource_report.report(tmp_path, {"run": {"mode": "ffi"}})
    summary = json.loads((tmp_path / "resource-summary.json").read_text())
    assert not summary["valid"]
    if failure != "late-probe-drops":
        assert summary["runs"]["run"]["ffi_error"]


def test_ffi_labeled_drop_counters_are_added(tmp_path: Path) -> None:
    path = tmp_path / "metrics.prom"
    path.write_text(
        'agentsight_ffi_dropped_total{reason="count"} 3\n'
        'agentsight_ffi_dropped_total{reason="bytes"} 4\n'
    )
    assert collect_metrics.read_internal_metrics(path) == {"ffi_dropped": "7.0"}


def test_ffi_git_input_builds_and_freezes_library(tmp_path: Path) -> None:
    repo = fixture_repo(tmp_path)
    output = tmp_path / "result"
    output.mkdir()
    inputs = resource_inputs.Inputs(repo, output, ffi=True)
    commands = []

    def build(command: list[str], log: Path, **kwargs: object) -> int:
        commands.append(command)
        if command[0] == "cargo":
            artifact = tmp_path / "cache/target/release/libagentsight.so"
            artifact.parent.mkdir(parents=True)
            artifact.write_bytes(b"library fixture")
        else:
            Path(command[-1]).write_bytes(b"runner fixture")
        return 0

    try:
        source = inputs.prepare("run", "HEAD", None)
        inputs.build(source, build, tmp_path / "cache")
        assert commands[0][-1] == "--lib"
        assert source["library_sha256"] == resource_inputs.digest(output / "run/libagentsight.so")
        assert source["sha256"] == resource_inputs.digest(output / "run/agentsight")
        assert "-Wl,--disable-new-dtags,-rpath,$ORIGIN" in commands[1]
    finally:
        inputs.close()
    assert git(repo, "worktree", "list", "--porcelain").count("worktree ") == 1


def test_callback_receipts_wait_for_complete_lines_and_match_exact_ids(tmp_path: Path) -> None:
    path = tmp_path / "ffi-events.log"
    prefix = "T\tpreflight-run-0\t200\t13\t1\n"
    partial = "T\tbench-run-0-0\t200\t1012"
    path.write_text(prefix + partial)
    captured, offset = validate_results.load_ffi_captured(path)
    assert captured == {"preflight-run-0": [("complete", 13)]}
    assert offset == len(prefix)
    with path.open("a") as handle:
        handle.write("\t1\nT\tbench-other-0-0\t200\t1012\t1\n")
    captured, _ = validate_results.load_ffi_captured(
        path, expected={"bench-run-0-0"}, offset=offset
    )
    assert captured == {"bench-run-0-0": [("complete", 1012)]}


@pytest.mark.parametrize("status,usage,total", [(500, 1, 13), (200, 0, 13), (200, 1, 14)])
def test_callback_token_and_http_errors_fail_validation(
    tmp_path: Path, status: int, usage: int, total: int
) -> None:
    path = tmp_path / "ffi-events.log"
    path.write_text(f"T\tpreflight-run-0\t{status}\t{total}\t{usage}\n")
    captured, _ = validate_results.load_ffi_captured(path)
    report = validate_results.make_report({"preflight-run-0"}, {"preflight-run-0"}, captured, 13)
    assert report["token_accuracy"] == 0


@pytest.mark.parametrize("incomplete", [True, "true", False, "false", None])
def test_v2_completion_respects_incomplete_metadata_even_with_correct_tokens(
    tmp_path: Path, incomplete: bool | str | None
) -> None:
    request_id = "bench-run-0-0"
    metadata = {"status_code": "200"}
    if incomplete is not None:
        metadata["incomplete"] = incomplete
    call = {
        "request": {"messages": [{"content": f"[agentsight-benchmark:{request_id}] hello"}]},
        "metadata": metadata,
        "token_usage": {"total_tokens": 1012},
    }
    path = tmp_path / "ffi-events.log"
    path.write_text("J\t" + json.dumps(call) + "\n")
    captured, _ = validate_results.load_ffi_captured(path)
    report = validate_results.make_report({request_id}, {request_id}, captured, 1012)
    expected = 0 if incomplete in (True, "true") else 1
    assert report["matched"] == 1
    assert report["complete"] == expected
    assert report["token_correct"] == expected
    assert report["completeness_ratio"] == expected


def test_v2_complete_receipt_can_follow_an_incomplete_receipt(tmp_path: Path) -> None:
    request_id = "bench-run-0-0"
    call = {
        "request": {"messages": [{"content": f"[agentsight-benchmark:{request_id}] hello"}]},
        "metadata": {"status_code": "200", "incomplete": "true"},
        "token_usage": {"total_tokens": 1012},
    }
    path = tmp_path / "ffi-events.log"
    path.write_text("J\t" + json.dumps(call) + "\n")
    captured, offset = validate_results.load_ffi_captured(path)
    partial = validate_results.make_report({request_id}, {request_id}, captured, 1012)
    assert partial["complete"] == partial["token_correct"] == 0
    call["metadata"].pop("incomplete")
    with path.open("a") as handle:
        handle.write("J\t" + json.dumps(call) + "\n")
    later, _ = validate_results.load_ffi_captured(path, offset=offset)
    validate_results.merge_captured(captured, later)
    report = validate_results.make_report({request_id}, {request_id}, captured, 1012)
    assert report["matched"] == report["complete"] == report["token_correct"] == 1


@pytest.mark.parametrize("request_id", ["bench-run-0-0", "preflight-run-churn-1-0"])
def test_v2_metadata_only_receipt_is_matched_as_incomplete_without_body(
    tmp_path: Path, request_id: str
) -> None:
    call = {
        "request": {"messages": []},
        "response": {"messages": [{"parts": [], "finish_reason": "incomplete"}]},
        "metadata": {"request_id": request_id, "metadata_only": "true", "incomplete": "true",
                     "abandon_reason": "lru_evicted", "status_code": "200"},
    }
    path = tmp_path / "ffi-events.log"
    path.write_text("J\t" + json.dumps(call) + "\n")
    captured, _ = validate_results.load_ffi_captured(path, expected={request_id})
    assert captured == {request_id: [("error", None)]}
    report = validate_results.make_report({request_id}, {request_id}, captured, 1012)
    assert report["matched"] == 1
    assert report["missing_ids"] == []
    assert report["complete"] == report["token_correct"] == 0


def test_v2_metadata_request_id_respects_prefix_and_expected_filter(tmp_path: Path) -> None:
    path = tmp_path / "ffi-events.log"
    call = {"metadata": {"request_id": "bench-other", "status_code": "200"}}
    path.write_text("J\t" + json.dumps(call) + "\n")
    assert validate_results.load_ffi_captured(path, prefix="preflight-")[0] == {}
    assert validate_results.load_ffi_captured(path, expected={"bench-wanted"})[0] == {}
