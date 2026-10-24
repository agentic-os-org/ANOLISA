"""A damaged JSONL record must not erase independent load/capture evidence."""

from __future__ import annotations

import gzip
import json
import subprocess
import sys
from pathlib import Path

import pytest
from test_campaign_evidence import campaign_evidence, render_report, validate_results


def point(second: int) -> bytes:
    return (
        json.dumps(
            {
                "metric": "benchmark_requests",
                "request_id": f"bench-{second}",
                "data": {"time": second, "value": 1, "status": 200},
                "description": "世界",
            },
            ensure_ascii=False,
        )
        + "\n"
    ).encode()


def artifact(path: Path, compressed: bool, payload: bytes) -> Path:
    if compressed:
        path = path.with_suffix(path.suffix + ".gz")
        with gzip.open(path, "wb") as handle:
            handle.write(payload)
    else:
        path.write_bytes(payload)
    return path


@pytest.mark.parametrize("compressed", [False, True])
@pytest.mark.parametrize("damage", [b"\xff\n", b'{"note":"\xff"}\n', b'{"broken":\n', b"null\n"])
def test_all_consumers_retain_records_on_both_sides(
    tmp_path: Path, compressed: bool, damage: bytes
) -> None:
    measurement = tmp_path / "measurement"
    measurement.mkdir()
    path = artifact(measurement / "k6.jsonl", compressed, point(0) + damage + point(2))
    before = path.read_bytes()
    objects = list(validate_results.json_lines(path))
    assert [value["request_id"] for value in objects] == ["bench-0", "bench-2"]
    assert objects[1]["description"] == "世界"
    assert list(render_report.json_lines(path)) == objects
    assert validate_results.load_expected(path) == ({"bench-0", "bench-2"}, {"bench-0", "bench-2"})
    assert render_report.summarize_load(path)["requests"] == 2
    load = campaign_evidence.load_samples(tmp_path / "run-result.json")
    assert load["effective_qps"] == [(0, 1), (1, 0), (2, 1)]
    assert (
        campaign_evidence.continuous_recovery(
            load["effective_qps"], 1, 0.1, 2, higher_is_better=True
        )
        is None
    )
    assert path.read_bytes() == before


@pytest.mark.parametrize("compressed", [False, True])
def test_bad_bytes_inside_a_request_id_do_not_invent_a_replacement_id(
    tmp_path: Path, compressed: bool
) -> None:
    path = artifact(
        tmp_path / "requests.jsonl",
        compressed,
        b'{"request_id":"bench-\xff","http_status":200}\n'
        b'{"request_id":"bench-valid","http_status":200}\n',
    )
    assert validate_results.load_request_log(path, "bench-") == ({"bench-valid"}, {"bench-valid"})


@pytest.mark.parametrize("compressed", [False, True])
def test_torn_utf8_tail_keeps_the_last_complete_record(tmp_path: Path, compressed: bool) -> None:
    path = artifact(tmp_path / "k6.jsonl", compressed, point(0) + b'{"text":"\xe4\xb8')
    assert render_report.summarize_load(path)["requests"] == 1
    assert validate_results.load_expected(path)[0] == {"bench-0"}


@pytest.mark.parametrize("compressed", [False, True])
def test_render_cli_still_publishes_paired_reports(tmp_path: Path, compressed: bool) -> None:
    path = artifact(tmp_path / "k6.jsonl", compressed, point(0) + b"\xff\n" + point(2))
    output, summary = tmp_path / "report.md", tmp_path / "summary.json"
    result = subprocess.run(
        [
            sys.executable,
            render_report.__file__,
            "--protocol",
            "sse",
            "--qps",
            "1",
            "--duration",
            "3",
            "--load-results",
            str(path),
            "--output",
            str(output),
            "--summary-output",
            str(summary),
        ],
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 0, result.stderr
    assert "Skipped 1 malformed JSONL records" in result.stderr
    assert output.exists()
    assert json.loads(summary.read_text())["effective_qps"] == pytest.approx(2 / 3)


def test_filesystem_errors_are_not_treated_as_bad_records(tmp_path: Path) -> None:
    with pytest.raises(FileNotFoundError):
        list(validate_results.json_lines(tmp_path / "missing.jsonl"))


def test_corrupt_compression_container_is_not_silently_accepted(tmp_path: Path) -> None:
    path = tmp_path / "broken.jsonl.gz"
    path.write_bytes(b"this is not gzip")
    with pytest.raises(gzip.BadGzipFile):
        list(validate_results.json_lines(path))
