"""Interrupted publications preserve resumable, parseable campaign evidence."""

from __future__ import annotations

import json
import sys
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).parents[1] / "campaign"))

import campaign_manifest
import pytest
from test_benchmark import campaign
from test_campaign_resume_checkpoints import harness, measure
from test_campaign_runner import campaign_data


def test_manifest_is_only_visible_after_complete_publication(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "campaign.json"
    data = campaign_data(tmp_path)
    source.write_text(json.dumps(data))
    results = tmp_path / "results"
    original = Path.replace
    observed = []

    def publish(path: Path, target: Path) -> Path:
        if Path(target).name == "manifest.json":
            assert not Path(target).exists()
            assert json.loads(path.read_text())["frozen"]["campaign_sha256"]
            observed.append(path)
        return original(path, target)

    monkeypatch.setattr(Path, "replace", publish)
    campaign_manifest.ensure_manifest(results, source, data)
    assert len(observed) == 1
    assert not observed[0].exists()
    before = (results / "manifest.json").read_bytes()
    campaign_manifest.ensure_manifest(results, source, data)
    assert (results / "manifest.json").read_bytes() == before


def test_interrupted_manifest_can_be_frozen_again(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    source = tmp_path / "campaign.json"
    source.write_text(json.dumps(data))
    original = Path.replace

    def fail(path: Path, target: Path) -> Path:
        raise OSError("simulated publication failure")

    monkeypatch.setattr(Path, "replace", fail)
    with pytest.raises(OSError, match="publication failure"):
        campaign_manifest.ensure_manifest(tmp_path / "results", source, data)
    assert not (tmp_path / "results/manifest.json").exists()
    assert not list((tmp_path / "results").iterdir())
    monkeypatch.setattr(Path, "replace", original)
    campaign_manifest.ensure_manifest(tmp_path / "results", source, data)
    assert json.loads((tmp_path / "results/manifest.json").read_text())["frozen"]


def test_run_summary_is_preserved_if_runtime_annotation_cannot_be_published(
    harness: tuple[dict[str, Any], Path, list[list[str]]], monkeypatch: pytest.MonkeyPatch
) -> None:
    _, results, _ = harness
    original = Path.replace
    observed = []

    def fail(path: Path, target: Path) -> Path:
        if Path(target).name == "run-summary.json":
            observed.append(Path(target).read_bytes())
            raise OSError("simulated summary failure")
        return original(path, target)

    monkeypatch.setattr(Path, "replace", fail)
    with pytest.raises(OSError, match="summary failure"):
        measure(harness)
    summary = next(results.glob("runs/**/run-summary.json"))
    assert summary.read_bytes() == observed[0]
    assert json.loads(summary.read_text())["trace_completeness"] == 1
    assert not list(results.glob("runs/**/run-result.json"))
    monkeypatch.setattr(Path, "replace", original)
    assert measure(harness, resume=True)["evaluation"]["verdict"] == "PASS"


def test_resolved_load_levels_keep_previous_bytes_on_publication_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    for version in data["versions"]:
        (tmp_path / f"capacity-{version}.json").write_text(
            json.dumps({"maximum_sustainable_qps": 500, "first_failed_qps": 550})
        )
    data["capacity"]["qps_resolution"] = 50
    data["matrix"]["qps"] = [100, 200, 300, 400, 500]
    assert campaign.resolved_qps(data, tmp_path) == ([100, 200, 300, 400, 500], 400, 550)
    target = tmp_path / "campaign-resolution.json"
    before = target.read_bytes()

    def fail(path: Path, destination: Path) -> Path:
        raise OSError("simulated resolution failure")

    monkeypatch.setattr(Path, "replace", fail)
    with pytest.raises(OSError, match="resolution failure"):
        campaign.resolved_qps(data, tmp_path)
    assert target.read_bytes() == before
    assert not list(tmp_path.glob(".*.tmp"))


def test_concurrent_writers_do_not_share_staging_files(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = tmp_path / "checkpoint.json"
    campaign_manifest.write_json(path, {"value": "original"})
    path.chmod(0o640)
    barrier = threading.Barrier(2)
    original = Path.replace

    def publish(staging: Path, target: Path) -> Path:
        barrier.wait(timeout=5)
        assert json.loads(staging.read_text())["value"] in ("a", "b")
        return original(staging, target)

    monkeypatch.setattr(Path, "replace", publish)
    with ThreadPoolExecutor(max_workers=2) as executor:
        futures = [
            executor.submit(campaign_manifest.write_json, path, {"value": value})
            for value in ("a", "b")
        ]
        for future in futures:
            future.result(timeout=10)
    assert json.loads(path.read_text())["value"] in ("a", "b")
    assert path.stat().st_mode & 0o777 == 0o640
    assert not list(tmp_path.glob(".*.tmp"))
