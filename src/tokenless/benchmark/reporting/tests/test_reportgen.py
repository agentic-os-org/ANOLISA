# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""任意层子集、未知字段、评分绑定与匿名输出的离线回归。"""

import itertools
import json
import subprocess
import sys
from pathlib import Path

import pytest

from reportgen import ReportError, Row
from reportgen.__main__ import generate, parser
from reportgen.loaders import l1, l2, l3
from reportgen.model import combine, digest
from reportgen.privacy import check_output
from reportgen.render import comparisons, report
from reportgen.semantic import attach
from reportgen.statistics import summary


def test_source_aggregate_contracts_do_not_invent_row_measurements(tmp_path):
    l1_file = save(
        tmp_path / "legacy-l1.json",
        {
            "canonical": {
                "response": {"raw_tokens": 100, "compressed_tokens": 40, "savings_pct": 60.0}
            },
            "stacking": {},
            "cost_analysis": {"models": [{"model": "not-certified"}]},
            "rtk": {},
        },
    )
    l2_file = save(
        tmp_path / "legacy-l2.json",
        {
            "summary": {},
            "categories": [
                {
                    "category": "json",
                    "tokenless": {
                        "n": 3,
                        "compression_mean": 0.4,
                        "retention_passed": 2,
                        "retention_total": 3,
                    },
                }
            ],
            "tasks": [],
            "quality_gate": [],
        },
    )
    rows = l1(l1_file) + l2(l2_file)
    assert all(row.evidence_kind == "aggregate" and row.rate is None for row in rows)
    assert summary(rows)["measured"] == 0
    text = report(rows)
    assert "源汇总结果（不反推逐行测量）" in text
    assert "bytes/4 辅助估算" in text
    assert "not-certified" not in text


@pytest.mark.parametrize("field", ["input", "output"])
def test_declared_hash_cannot_override_verified_text(tmp_path, field):
    path = data(tmp_path)["l2"]
    doc = json.loads(path.read_text())
    raw = doc["rows"][0]["row"]
    doc["text_store"] = {}
    for key in ("input", "output"):
        text = raw.pop(key)
        reference = digest(text)
        raw[key + "_ref"] = reference
        doc["text_store"][reference] = text
    raw["payload_hash" if field == "input" else "output_hash"] = digest("not the measured text")
    save(path, doc)
    with pytest.raises(ReportError, match="指纹"):
        l2(path)


@pytest.mark.parametrize("name", ["tokenless-report_zh.md", "comparison-report_zh.md"])
def test_output_file_never_overwrites_source_json(tmp_path, name):
    source = data(tmp_path)["l1"]
    destination = tmp_path / name
    destination.write_bytes(source.read_bytes())
    original = destination.read_bytes()
    args = parser().parse_args(["--l1", str(destination), "--out", str(tmp_path)])
    with pytest.raises(ReportError, match="覆盖"):
        generate(args)
    assert destination.read_bytes() == original


@pytest.mark.parametrize("alias", ["symlink", "hardlink"])
def test_l3_companion_alias_cannot_be_overwritten(tmp_path, alias):
    import os

    source = data(tmp_path)["l3"]
    destination = tmp_path / "tokenless-report_zh.md"
    save(destination, {"rows": []})
    original = destination.read_bytes()
    companion = source.with_name("lifecycle.json")
    if alias == "symlink":
        companion.symlink_to(destination)
    else:
        os.link(destination, companion)
    args = parser().parse_args(["--l3", str(source), "--out", str(tmp_path)])
    with pytest.raises(ReportError, match="覆盖"):
        generate(args)
    assert destination.read_bytes() == original and companion.read_bytes() == original


def test_l3_model_score_requires_matching_source_document(tmp_path):
    row = Row(
        "l3",
        "tokenless",
        "recovery",
        "scenario/test",
        "json",
        10,
        5,
        input_hash=digest("input"),
        output_hash=digest("output"),
        source_run=digest("report-A"),
    )
    plan = {
        "layer": "l3",
        "units": [
            {
                "row_id": row.sample,
                "side": "tokenless",
                "variant": "recovery",
                "original": row.input_hash,
                "compressed": row.output_hash,
            }
        ],
    }
    stats = {
        "layer": "l3",
        "rows": [
            {
                "row_id": row.sample,
                "side": "tokenless",
                "variant": "recovery",
                "conditions": {"inline": {"missing_pairs": 0, "semantic_retention": {"pooled": 1}}},
            }
        ],
    }
    save(tmp_path / "plan.json", plan)
    save(tmp_path / "stats.json", stats)
    assert attach([row], [("l3", tmp_path)])[0].semantic == {}
    assert "缺来源运行" in report([row])
    plan["source_run"] = stats["source_run"] = row.source_run
    save(tmp_path / "plan.json", plan)
    save(tmp_path / "stats.json", stats)
    assert attach([row], [("l3", tmp_path)])[0].semantic["inline"]["information_retained"] == 1
    row.source_run = digest("report-B")
    assert attach([row], [("l3", tmp_path)])[0].semantic == {}
    stats["source_run"] = digest("report-C")
    save(tmp_path / "stats.json", stats)
    with pytest.raises(ReportError, match="source_run"):
        attach([row], [("l3", tmp_path)])


def test_l3_different_source_runs_are_never_merged(tmp_path):
    first = data(tmp_path)["l3"]
    doc = json.loads(first.read_text())
    doc["summary"] = {"git_sha": "revision-a"}
    save(first, doc)
    other = first.with_name("other.json")
    doc["summary"]["git_sha"] = "revision-b"
    save(other, doc)
    rows = combine(l3(first) + l3(other))
    assert len(rows) == 6
    assert len({row.source_run for row in rows}) == 2


@pytest.mark.parametrize("value", [None, [], {}, [None], [""]])
def test_missing_l3_entry_points_do_not_publish_full_marks(tmp_path, value):
    path = data(tmp_path)["l3"]
    doc = json.loads(path.read_text())
    doc["scenarios"][0]["tokenless_entry_points"] = value
    save(path, doc)
    row = next(row for row in l3(path) if row.side == "tokenless")
    assert not row.applicable and row.facts is None
    row.semantic = {"inline": {"information_retained": 1, "task_original": 1}}
    text = report([row])
    assert "100.0%" not in text and "不适用" in text


@pytest.mark.parametrize("value", [0, True, 20260101, ""])
def test_invalid_l4_recorded_run_id_is_not_silently_discarded(tmp_path, value):
    from reportgen.l4 import load

    root = data(tmp_path)["l4"]
    save(root / "out/repeat-1/arm-b/run/run_metadata.json", {"run_id": value})
    with pytest.raises(ReportError, match="run_id"):
        load(root)


def test_l4_cumulative_metadata_keeps_identity_limits_visible(tmp_path):
    from reportgen.l4 import load

    root = data(tmp_path)["l4"]
    save(
        root / "out/repeat-1/arm-b/run/run_metadata.json",
        {"started_at": "2026-01-01T00:00:00+00:00", "run_count": 2},
    )
    text = report(load(root))
    assert "多次运行" in text and "派生" in text and "运行标识未记录" in text


def test_l2_schema2_text_store_is_bound_to_its_hashes(tmp_path):
    path = data(tmp_path)["l2"]
    doc = json.loads(path.read_text())
    raw = doc["rows"][0]["row"]
    doc["schema_version"] = 2
    doc["text_store"] = {}
    for field in ("input", "output"):
        text = raw.pop(field)
        key = digest(text)
        doc["text_store"][key] = text
        raw[field + "_ref"] = key
    save(path, doc)
    row = l2(path)[0]
    assert row.input_hash == digest("original")
    doc["text_store"][raw["input_ref"]] = "wrong bytes"
    save(path, doc)
    with pytest.raises(ReportError, match="指纹"):
        l2(path)


def test_group_labels_expose_dataset_and_tokenizer_without_raw_names(tmp_path):
    from reportgen.privacy import anonymous

    files = data(tmp_path)
    first = json.loads(files["l1"].read_text())
    first["set"] = "set-alpha-private"
    second = json.loads(json.dumps(first))
    second["set"] = "set-beta-private"
    second["rows"][0]["tokens_after"] = 9
    save(tmp_path / "first.json", first)
    save(tmp_path / "second.json", second)
    text = report(l1(tmp_path / "first.json") + l1(tmp_path / "second.json"))
    assert anonymous(first["set"]) in text and anonymous(second["set"]) in text
    assert first["set"] not in text and second["set"] not in text
    assert "数据集" in text and "Token计数工具" in text and "o200k_base" in text


@pytest.mark.parametrize("value", [None, False, 0, ""])
def test_missing_sample_identity_has_layer_and_row_context(tmp_path, value):
    path = data(tmp_path)["l1"]
    doc = json.loads(path.read_text())
    doc["rows"][0]["case"]["id"] = value
    save(path, doc)
    with pytest.raises(ReportError, match="L1 行 0"):
        l1(path)


def test_null_tokenizer_is_missing_not_an_unsortable_key(tmp_path):
    path = data(tmp_path)["l1"]
    doc = json.loads(path.read_text())
    doc["rows"][0]["tokenizer"] = None
    save(path, doc)
    assert "o200k_base" in report(l1(path))


def test_l3_rejects_negative_tokens_but_keeps_legitimate_expansion(tmp_path):
    path = data(tmp_path)["l3"]
    doc = json.loads(path.read_text())
    doc["scenarios"][0]["tokenless_rate"] = 1.8
    save(path, doc)
    with pytest.raises(ReportError, match="负 Token"):
        l3(path)
    doc["scenarios"][0]["tokenless_rate"] = -0.5
    save(path, doc)
    row = l3(path)[0]
    assert row.after == 15 and row.rate == -0.5


def test_l3_composite_dispositions_and_primary_mode_survive_rendering(tmp_path):
    from reportgen.terms import DISPOSITIONS

    path = data(tmp_path)["l3"]
    doc = json.loads(path.read_text())
    doc["summary"] = {"tokenless_mode": "recovery"}
    doc["scenarios"][0]["tokenless_lifecycle"] = {"dispositions": ["applied", "passthrough"]}
    save(path, doc)
    rows = l3(path)
    assert rows[0].mode == "recovery"
    text = report(rows)
    assert DISPOSITIONS["applied"] in text and DISPOSITIONS["passthrough"] in text


def test_duplicate_l3_lifecycle_identity_is_refused(tmp_path):
    path = data(tmp_path)["l3"]
    row = {
        "suite": "scenario",
        "scenario": "fixture",
        "side": "tokenless",
        "mode_id": "recovery",
        "input": {"messages": []},
    }
    save(path.with_name("lifecycle.json"), {"rows": [row, row], "tokenless_mode": "recovery"})
    with pytest.raises(ReportError, match="重复"):
        l3(path)


def test_l2_same_payload_with_distinct_commands_does_not_combine_latencies(tmp_path):
    path = data(tmp_path)["l2"]
    doc = json.loads(path.read_text())
    row = doc["rows"][0]
    row["entry_kind"] = "explicit RTK"
    row["row"]["entry"]["command"] = "git log --oneline"
    other = json.loads(json.dumps(row))
    other["row"]["entry"]["command"] = "git log --format=%s"
    other["row"]["latency_s"] = 1
    doc["rows"].append(other)
    save(path, doc)
    rows = combine(l2(path))
    assert len(rows) == 2
    assert sorted(row.latency_ms for row in rows) == [[10.0], [1000]]


def test_l4_usage_excludes_unselected_instances_and_displays_boundaries(tmp_path):
    from reportgen.l4 import load, usage

    root = data(tmp_path)["l4"]
    arm = root / "out" / "repeat-1" / "arm-b"

    def measured(tokens, requests):
        return {
            "input_tokens": tokens,
            "cache_read_tokens": 0,
            "output_tokens": 1,
            "requests": requests,
        }

    save(
        arm / "report.json",
        {
            "instances": {
                "fixture": measured(100, 1),
                "timeout": measured(0, 0),
                "extra": measured(9000, 90),
            }
        },
    )
    tokens, requests, note = usage(arm, {"fixture", "timeout"})
    assert tokens == 100 and requests == 1 and "已排除：1" in note
    text = report(load(root))
    assert "已排除：1" in text
    assert "官方评分只证明补丁任务结果" in text
    assert "已完成且无补丁（由记录推断）" in text


def test_unregistered_notes_are_visible_as_gaps_without_exposing_text():
    row = alias_rows()[0]
    row.notes = ["private unavailable detail"]
    text = report([row], comparison=True)
    assert "未登记限制说明已隐藏（1条）" in text
    assert row.notes[0] not in text


def test_independent_statistics_do_not_share_mutable_row_state():
    from reportgen.statistics import independent

    source = alias_rows()[0]
    source.notes = ["fixture note"]
    observed = independent([source])[0]
    observed.notes.append("added note")
    assert source.notes == ["fixture note"]


def test_l3_no_entry_preserves_counts_without_polluting_statistics(tmp_path):
    path = data(tmp_path)["l3"]
    doc = json.loads(path.read_text())
    raw = doc["scenarios"][0]
    raw.update(
        tokens_before=100,
        tokenless_rate=0,
        tokenless_gap_reason="private recovery-unavailable detail",
        tokenless_entry_points=[],
    )
    save(path, doc)
    rows = l3(path)
    row = next(row for row in rows if row.side == "tokenless")
    assert row.before == row.after == 100
    assert not row.applicable and row.rate is None
    assert summary([row])["measured"] == 0
    text = report([row])
    assert "100→100" in text and "生产入口不适用或恢复能力不可用" in text
    assert "private recovery-unavailable detail" not in text
    assert not any(line.startswith("| L3 |") for line in comparisons(rows))
    raw.update(tokenless_gap_reason=None, tokenless_entry_points=["PostTool json"])
    save(path, doc)
    applicable = next(row for row in l3(path) if row.side == "tokenless")
    assert applicable.applicable and applicable.rate == 0
    assert summary([applicable])["measured"] == 1


def test_l4_metadata_identity_survives_relocation(tmp_path):
    import shutil

    from reportgen.l4 import load

    root = data(tmp_path)["l4"]
    save(
        root / "out/repeat-1/arm-b/run/run_metadata.json",
        {"started_at": "2026-01-01T00:00:00+00:00", "instance_ids": ["fixture", "timeout"]},
    )
    moved = tmp_path / "relocated"
    shutil.copytree(root, moved)
    original = load(root)[0]
    relocated = load(moved)[0]
    assert original.sample == relocated.sample
    assert any("运行标识未记录" in note for note in original.notes)
    assert any("原始字节摘要派生" in note for note in original.notes)
    combined = combine([original, relocated])
    assert len(combined) == 1 and combined[0].observations == 2


def test_pseudonyms_cannot_be_recovered_by_plain_sha256():
    from reportgen.privacy import anonymous

    assert anonymous("case-0007") != "样本-" + digest("case-0007")[:12]
    assert anonymous("case-0007") == anonymous("case-0007")


@pytest.mark.parametrize("reverse", [False, True])
@pytest.mark.parametrize("score_count", [1, 2])
def test_multiple_l3_runs_attach_only_their_own_scores(tmp_path, reverse, score_count):
    paths, directories, expected = [], [], {}
    for index, score in enumerate((0.75, 0.25)):
        path = data(tmp_path / f"run-{index}")["l3"]
        doc = json.loads(path.read_text())
        doc["summary"] = {"git_sha": f"revision-{index}", "tokenless_mode": "recovery"}
        save(path, doc)
        save(
            path.with_name("lifecycle.json"),
            {
                "tokenless_mode": "recovery",
                "rows": [
                    {
                        "suite": "scenario",
                        "scenario": "fixture",
                        "side": "tokenless",
                        "mode_id": "recovery",
                        "input": {"messages": [{"content": "same input"}]},
                        "output": {"messages": [{"content": "same output"}]},
                    }
                ],
            },
        )
        row = next(row for row in l3(path) if row.side == "tokenless")
        scores = path.parent / "scores"
        unit = {
            "row_id": row.sample,
            "side": row.side,
            "variant": row.mode,
            "original": row.input_hash,
            "compressed": row.output_hash,
        }
        save(scores / "plan.json", {"layer": "l3", "source_run": row.source_run, "units": [unit]})
        save(
            scores / "stats.json",
            {
                "layer": "l3",
                "source_run": row.source_run,
                "rows": [
                    {
                        **unit,
                        "conditions": {
                            "inline": {"missing_pairs": 0, "semantic_retention": {"pooled": score}}
                        },
                    }
                ],
            },
        )
        paths.append(path)
        if index < score_count:
            directories.append(("l3", scores))
        expected[row.source_run] = score if index < score_count else None
    if reverse:
        paths.reverse()
        directories.reverse()
    rows = attach(
        combine(list(itertools.chain.from_iterable(l3(path) for path in paths))), directories
    )
    assert {
        row.source_run: row.semantic.get("inline", {}).get("information_retained")
        for row in rows
        if row.side == "tokenless"
    } == expected
    arguments = list(itertools.chain.from_iterable(("--l3", str(path)) for path in paths))
    for layer, scores in directories:
        arguments.extend(("--llm", f"{layer}={scores}"))
    text = generate(parser().parse_args([*arguments, "--out", str(tmp_path / "generated")]))[
        "tokenless-report_zh.md"
    ]
    assert "75.0%" in text and ("25.0%" in text) is (score_count == 2)
    script = Path(__file__).resolve().parents[1] / "generate_report.py"
    result = subprocess.run(
        [sys.executable, str(script), *arguments, "--out", str(tmp_path / "cli")],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr


def l4_snapshot(tmp_path, run_id=None):
    root = data(tmp_path)["l4"]
    arm = root / "out/repeat-1/arm-b"
    save(arm / "selection.json", {"instance_ids": ["fixture"], "attempt_complete": True})
    meta = {"started_at": "2026-01-01T00:00:00+00:00", "instance_ids": ["fixture"], "run_count": 1}
    if run_id is not None:
        meta["run_id"] = run_id
    save(arm / "run/run_metadata.json", meta)
    save(arm / "run/preds.json", {"fixture": {"model_patch": "fixture patch"}})
    save(
        arm / "report.json",
        {
            "instances": {
                "fixture": {
                    "input_tokens": 100,
                    "cache_read_tokens": 0,
                    "output_tokens": 1,
                    "requests": 1,
                }
            }
        },
    )
    return root


def score_l4_snapshot(root, resolved):
    arm = root / "out/repeat-1/arm-b"
    evaluation = root / "eval/repeat-1-arm-b"
    save(evaluation / "evaluate/model.run.json", {"resolved_ids": resolved})
    (evaluation / ".eval-complete").write_text(
        digest((arm / "run/preds.json").read_text()) + " run\n"
    )


def test_copied_l4_windows_count_usage_once_and_keep_changed_evidence(tmp_path):
    import shutil

    from reportgen.l4 import load

    root = l4_snapshot(tmp_path / "original")
    copy = tmp_path / "copy"
    shutil.copytree(root, copy)
    for first, second in ((root, copy), (copy, root)):
        rows = combine(load(first) + load(second))
        assert len(rows) == 1 and rows[0].observations == 2
        assert rows[0].task["prompt_tokens"] == 100 and rows[0].task["requests"] == 1
        text = generate(
            parser().parse_args(
                ["--l4", str(first), "--l4", str(second), "--out", str(tmp_path / "generated")]
            )
        )["tokenless-report_zh.md"]
        assert "同一采集窗口被多次导入" in text
    meter = copy / "out/repeat-1/arm-b/report.json"
    doc = json.loads(meter.read_text())
    doc["instances"]["fixture"]["input_tokens"] = 101
    save(meter, doc)
    assert len(combine(load(root) + load(copy))) == 2


@pytest.mark.parametrize("reverse", [False, True])
def test_matching_l4_snapshot_retains_later_verified_score(tmp_path, reverse):
    import shutil

    from reportgen.l4 import load

    before = l4_snapshot(tmp_path / "before", run_id="recorded-run")
    after = tmp_path / "after"
    shutil.copytree(before, after)
    score_l4_snapshot(after, ["fixture"])
    paths = [after, before] if reverse else [before, after]
    rows = combine(load(paths[0]) + load(paths[1]))
    assert len(rows) == 1 and rows[0].observations == 2
    assert rows[0].task["resolved"] == 1 and rows[0].task["status"] == "scored"
    text = generate(
        parser().parse_args(
            ["--l4", str(paths[0]), "--l4", str(paths[1]), "--out", str(tmp_path / "generated")]
        )
    )["tokenless-report_zh.md"]
    assert "官方已评分" in text and "未完成评分不发布解决率" not in text
    assert "| 1 | 1 | 1 | 100.0% |" in text


def test_conflicting_l4_scores_are_not_order_dependent(tmp_path):
    import shutil

    from reportgen.l4 import load

    first = l4_snapshot(tmp_path / "first", run_id="recorded-run")
    second = tmp_path / "second"
    shutil.copytree(first, second)
    score_l4_snapshot(first, ["fixture"])
    score_l4_snapshot(second, [])
    for paths in ((first, second), (second, first)):
        with pytest.raises(ReportError, match="评分.*不一致"):
            combine(load(paths[0]) + load(paths[1]))


def test_l3_source_document_is_fingerprinted_once(tmp_path, monkeypatch):
    from reportgen import loaders

    path = data(tmp_path)["l3"]
    doc = json.loads(path.read_text())
    doc["scenarios"] = [dict(doc["scenarios"][0], scenario=f"case-{index}") for index in range(100)]
    save(path, doc)
    original = json.dumps
    calls = []

    def tracked(value, *args, **kwargs):
        if isinstance(value, dict) and "scenarios" in value:
            calls.append(value)
        return original(value, *args, **kwargs)

    monkeypatch.setattr(loaders.json, "dumps", tracked)
    assert len(l3(path)) == 300
    assert len(calls) == 1


def score_import_fixture(tmp_path, wrong_source=False, empty=False):
    path = data(tmp_path)["l3"]
    save(
        path.with_name("lifecycle.json"),
        {
            "rows": [
                {
                    "suite": "scenario",
                    "scenario": "fixture",
                    "side": "tokenless",
                    "mode_id": "unknown",
                    "input": {"messages": [{"content": "source"}]},
                    "output": {"messages": [{"content": "output"}]},
                }
            ]
        },
    )
    row = next(row for row in l3(path) if row.side == "tokenless")
    source = digest("another run") if wrong_source else row.source_run
    directory = tmp_path / "scores"
    unit = {
        "row_id": row.sample,
        "side": row.side,
        "variant": row.mode,
        "original": row.input_hash,
        "compressed": row.output_hash,
    }
    save(directory / "plan.json", {"layer": "l3", "source_run": source, "units": [unit]})
    save(
        directory / "stats.json",
        {
            "layer": "l3",
            "source_run": source,
            "rows": []
            if empty
            else [
                {
                    **unit,
                    "conditions": {
                        "inline": {"missing_pairs": 0, "semantic_retention": {"pooled": 0.91}}
                    },
                }
            ],
        },
    )
    return path, directory


@pytest.mark.parametrize("empty", [False, True])
def test_unused_score_directory_is_visible_in_cli_and_reports(tmp_path, empty):
    import os

    path, scores = score_import_fixture(tmp_path, wrong_source=True, empty=empty)
    script = Path(__file__).resolve().parents[1] / "generate_report.py"
    outputs = []
    for supplied in (False, True):
        arguments = [
            sys.executable,
            str(script),
            "--l3",
            str(path),
            "--out",
            str(tmp_path / str(supplied)),
        ]
        if supplied:
            arguments.extend(["--llm", f"l3={scores}"])
        env = {
            key: value
            for key, value in os.environ.items()
            if key in ("PATH", "HOME", "LANG", "TMPDIR")
        }
        env["REPORTGEN_SALT"] = "fixture-only-labels"
        result = subprocess.run(arguments, env=env, text=True, capture_output=True)
        assert result.returncode == 0, result.stderr
        assert ("未绑定" in result.stderr) is supplied
        text = (tmp_path / str(supplied) / "tokenless-report_zh.md").read_text()
        assert "91.0%" not in text
        assert ("显式提供的模型测题证据未绑定" in text) is supplied
        assert str(scores) not in result.stderr + text
        outputs.append(text)
    assert outputs[0] != outputs[1]


@pytest.mark.parametrize(
    "plan_source,stats_source",
    [("invalid", "invalid"), (digest("A"), digest("B")), (42, 42), (None, digest("A"))],
)
def test_empty_score_rows_still_validate_directory_provenance(tmp_path, plan_source, stats_source):
    path, scores = score_import_fixture(tmp_path, empty=True)
    for name, source in (("plan", plan_source), ("stats", stats_source)):
        doc = json.loads((scores / f"{name}.json").read_text())
        doc["source_run"] = source
        save(scores / f"{name}.json", doc)
    with pytest.raises(ReportError, match="source_run"):
        attach(l3(path), [("l3", scores)])
    script = Path(__file__).resolve().parents[1] / "generate_report.py"
    result = subprocess.run(
        [
            sys.executable,
            str(script),
            "--l3",
            str(path),
            "--llm",
            f"l3={scores}",
            "--out",
            str(tmp_path / "cli"),
        ],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 2 and "source_run" in result.stderr


def test_missing_prediction_files_are_not_observed_zero(tmp_path):
    from reportgen.l4 import load

    root = tmp_path / "empty-arm"
    arm = root / "out/repeat-1/arm-b"
    arm.mkdir(parents=True)
    (arm / "notes.txt").write_text("unrelated")
    with pytest.raises(ReportError, match="采集证据"):
        load(root)
    save(arm / "selection.json", {"instance_ids": ["fixture"], "attempt_complete": False})
    rows = load(root)
    assert rows[0].task["submitted"] is None
    assert rows[0].task["status"] == "unscored"
    assert "未知" in report(rows) and "None" not in report(rows)


def test_explicit_null_run_id_uses_recorded_id(tmp_path):
    from reportgen.l4 import load

    root = l4_snapshot(tmp_path, run_id="recorded")
    metadata = root / "out/repeat-1/arm-b/run/run_metadata.json"
    save(metadata, {"run_id": None, "id": "recorded"})
    assert load(root)[0].source_run == digest("recorded")
    assert not any("运行标识未记录" in note for note in load(root)[0].notes)


def save(path, document):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(document), encoding="utf-8")
    return path


def data(tmp):
    files = {}
    files["l1"] = save(
        tmp / "l1.json",
        {
            "set": "synthetic",
            "rows": [
                {
                    "case": {
                        "id": "fixture",
                        "domain": "json",
                        "mode": "direct-default",
                        "input": "original",
                    },
                    "output": "short",
                    "tokens_before": 10,
                    "tokens_after": 5,
                    "disposition": "compressed",
                    "latency_s": 0.01,
                }
            ],
        },
    )
    files["l2"] = save(
        tmp / "l2.json",
        {
            "rows": [
                {
                    "side": "tokenless",
                    "mode": "default",
                    "category": "json",
                    "row": {
                        "sample_id": "fixture",
                        "input": "original",
                        "output": "short",
                        "tokens_before": 10,
                        "tokens_after": 5,
                        "latency_s": 0.01,
                        "entry": {"post_tool": {"disposition": "applied"}},
                    },
                }
            ]
        },
    )
    files["l3"] = save(
        tmp / "l3" / "report.json",
        {
            "scenarios": [
                {
                    "suite": "scenario",
                    "scenario": "fixture",
                    "content_type": "json",
                    "tokens_before": 10,
                    "tokenless_rate": 0.5,
                    "tokenless_ms": 1,
                    "tokenless_retention": [1, 1],
                    "tokenless_entry_points": ["PostTool json"],
                }
            ]
        },
    )
    files["l4"] = tmp / "l4"
    save(
        files["l4"] / "out" / "repeat-1" / "arm-b" / "selection.json",
        {"instance_ids": ["fixture", "timeout"], "attempt_complete": True},
    )
    return files


SUBSETS = [
    subset
    for size in range(1, 5)
    for subset in itertools.combinations(("l1", "l2", "l3", "l4"), size)
]


@pytest.mark.parametrize("layers", SUBSETS)
def test_every_nonempty_layer_subset_generates_both_reports(tmp_path, layers):
    inputs = data(tmp_path)
    arguments = list(
        itertools.chain.from_iterable(("--" + layer, str(inputs[layer])) for layer in layers)
    )
    args = parser().parse_args([*arguments, "--out", str(tmp_path / "output")])
    documents = generate(args)
    assert set(documents) == {"tokenless-report_zh.md", "comparison-report_zh.md"}
    for text in documents.values():
        assert "名词速查" in text
        for layer in ("l1", "l2", "l3", "l4"):
            section = text.split(f"## {layer.upper()} 测试数据", 1)[1].split("\n## ", 1)[0]
            assert ("未运行或未提供" in section) is (layer not in layers)
        check_output(text)
    standalone = documents["tokenless-report_zh.md"].split("## L1 测试数据", 1)[1]
    assert "| Headroom |" not in standalone


def test_l4_zero_prediction_tasks_stay_in_denominator(tmp_path):
    inputs = data(tmp_path)
    args = parser().parse_args(["--l4", str(inputs["l4"]), "--out", str(tmp_path / "out")])
    text = generate(args)["tokenless-report_zh.md"]
    assert "| 2 | 0 | 0 | 0.0% |" in text
    assert "计量不完整" in text


def test_l4_scoring_binds_to_current_predictions_and_keeps_timeout(tmp_path):
    inputs = data(tmp_path)
    root = inputs["l4"]
    arm = root / "out" / "repeat-1" / "arm-b"
    predictions = save(arm / "run" / "preds.json", {"fixture": {"model_patch": "fixture patch"}})
    evaluation = root / "eval" / "repeat-1-arm-b"
    save(evaluation / "evaluate" / "model.run.json", {"resolved_ids": ["fixture"]})
    (evaluation / ".eval-complete").write_text(digest(predictions.read_text()) + " run\n")
    args = parser().parse_args(["--l4", str(root), "--out", str(tmp_path / "output")])
    assert "| 2 | 1 | 1 | 50.0% |" in generate(args)["tokenless-report_zh.md"]
    predictions.write_text("{}")
    (arm / "selection.json").write_text(
        '{"instance_ids":["fixture","timeout"],"attempt_complete":false}'
    )
    assert "未评分" in generate(args)["tokenless-report_zh.md"]


def test_malformed_l4_usage_is_not_fabricated_zero(tmp_path):
    inputs = data(tmp_path)
    arm = inputs["l4"] / "out" / "repeat-1" / "arm-b"
    save(
        arm / "report.json",
        {
            "instances": {
                "fixture": {
                    "input_tokens": None,
                    "cache_read_tokens": 0,
                    "output_tokens": 1,
                    "requests": 1,
                }
            }
        },
    )
    args = parser().parse_args(["--l4", str(inputs["l4"]), "--out", str(tmp_path / "output")])
    assert "计量不完整" in generate(args)["tokenless-report_zh.md"]


def test_input_label_and_arbitrary_mechanism_are_not_public(tmp_path):
    address = ".".join(map(str, (10, 23, 45, 67)))
    inputs = data(tmp_path)
    doc = json.loads(inputs["l1"].read_text())
    row = doc["rows"][0]
    row["case"]["id"] = "fixture-account:" + address
    row["mechanism"] = "credential = " + "sk-" + "a" * 24
    row["case"]["input"] += " private employee value"
    save(inputs["l1"], doc)
    args = parser().parse_args(["--l1", str(inputs["l1"]), "--out", str(tmp_path / "output")])
    text = generate(args)["tokenless-report_zh.md"]
    assert address not in text and row["mechanism"] not in text
    assert "private employee value" not in text
    assert "样本-" in text


def test_output_gate_precedes_any_write(tmp_path):
    inputs = data(tmp_path)
    output = tmp_path / "output"
    args = parser().parse_args(
        ["--l1", str(inputs["l1"]), "--out", str(output), "--forbid", "Tokenless"]
    )
    with pytest.raises(ReportError, match="禁止公开"):
        generate(args)
    assert not output.exists()


def test_negative_zero_bool_or_missing_tokens_do_not_become_valid(tmp_path):
    rows = []
    for value in (None, -1, True, "10"):
        path = save(
            tmp_path / "l1.json",
            {
                "rows": [
                    {
                        "case": {"id": "fixture", "input": "value"},
                        "output": "v",
                        "tokens_before": value,
                        "tokens_after": 0,
                    }
                ]
            },
        )
        row = l1(path)[0]
        assert row.rate is None
        rows.append(row)
    assert all(row.before is None for row in rows)


def test_unknown_fact_denominator_not_perfect_score(tmp_path):
    path = data(tmp_path)["l2"]
    doc = json.loads(path.read_text())
    doc["rows"][0]["row"].update(retention_passed=0, retention_total=0)
    save(path, doc)
    assert l2(path)[0].facts is None


def test_semantic_hash_binding_prevents_borrowing_scores(tmp_path):
    row = l1(data(tmp_path)["l1"])[0]
    directory = tmp_path / "llm"
    save(
        directory / "plan.json",
        {
            "layer": "l1",
            "units": [
                {
                    "row_id": "synthetic/fixture",
                    "side": "tokenless",
                    "variant": "direct-default",
                    "original": digest("different input"),
                }
            ],
        },
    )
    save(
        directory / "stats.json",
        {
            "layer": "l1",
            "rows": [
                {
                    "row_id": "synthetic/fixture",
                    "side": "tokenless",
                    "variant": "direct-default",
                    "conditions": {"inline": {"missing_pairs": 0}},
                }
            ],
        },
    )
    attach([row], [("l1", directory)])
    assert not row.semantic


def test_repeat_timing_does_not_increase_independent_samples():
    row = Row(
        "l1",
        "tokenless",
        "default",
        "fixture",
        "json",
        10,
        5,
        input_hash=digest("original"),
        output_hash=digest("short"),
        latency_ms=[1],
    )
    copy = Row(
        "l1",
        "tokenless",
        "default",
        "fixture",
        "json",
        10,
        5,
        input_hash=digest("original"),
        output_hash=digest("short"),
        latency_ms=[2],
    )
    merged = combine([row, copy])
    assert len(merged) == 1 and merged[0].observations == 2
    changed = Row(
        "l1",
        "tokenless",
        "default",
        "fixture",
        "json",
        10,
        4,
        input_hash=digest("original"),
        output_hash=digest("other"),
    )
    combined = combine([row, changed])
    assert all(len(item.output_hashes) == 2 for item in combined)
    assert "同输入输出不一致" in report(combined)


def test_missing_or_empty_inputs_fail_without_output(tmp_path):
    args = parser().parse_args(["--out", str(tmp_path / "output")])
    with pytest.raises(ReportError, match="至少"):
        generate(args)
    assert not (tmp_path / "output").exists()


@pytest.mark.parametrize("changed", [False, True])
def test_matching_semantic_output_required(tmp_path, changed):
    row = l1(data(tmp_path)["l1"])[0]
    directory = tmp_path / "llm-valid"
    save(
        directory / "plan.json",
        {
            "layer": "l1",
            "units": [
                {
                    "row_id": "synthetic/fixture",
                    "side": "tokenless",
                    "variant": "direct-default",
                    "original": row.input_hash,
                    "compressed": digest("other") if changed else row.output_hash,
                }
            ],
        },
    )
    score = {
        "row_id": "synthetic/fixture",
        "side": "tokenless",
        "variant": "direct-default",
        "conditions": {
            "inline": {
                "missing_pairs": 0,
                "semantic_retention": {"pooled": 0.8},
                "task_retention": {"pooled": 0.5},
            }
        },
    }
    save(directory / "stats.json", {"layer": "l1", "rows": [score]})
    attach([row], [("l1", directory)])
    assert bool(row.semantic) is not changed
    if not changed:
        assert row.semantic["inline"]["information_retained"] == 0.8


def test_script_runs_outside_its_source_directory(tmp_path):
    inputs = data(tmp_path)
    script = Path(__file__).resolve().parents[1] / "generate_report.py"
    result = subprocess.run(
        [
            sys.executable,
            str(script),
            "--l1",
            str(inputs["l1"]),
            "--out",
            str(tmp_path / "output"),
        ],
        cwd=tmp_path,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    assert (tmp_path / "output" / "tokenless-report_zh.md").is_file()


def test_l3_pure_stage_does_not_borrow_router_scores_or_timing(tmp_path):
    path = data(tmp_path)["l3"]
    doc = json.loads(path.read_text())
    doc["scenarios"][0].update(
        headroom_router_rate=0.5, headroom_pure_rate=0.4, headroom_retention=[2, 3], headroom_ms=7
    )
    save(path, doc)
    rows = l3(path)
    router = next(row for row in rows if row.mode == "router")
    pure = next(row for row in rows if row.mode == "pure_stage")
    assert router.facts == (2, 3) and router.latency_ms == [7]
    assert pure.facts is None and pure.latency_ms == []
    assert "不借用内容分派流程成绩" in report([pure], comparison=True)


def alias_rows(side="headroom", mode="router"):
    return [
        Row(
            "l2",
            side,
            mode,
            name,
            "code",
            100,
            60,
            input_hash=digest("same bytes"),
            output_hash=digest("same result"),
            latency_ms=[index + 1],
        )
        for index, name in enumerate(
            ("static_code_diff", "static_code_diff_aggressive", "static_code_diff_smart")
        )
    ]


def test_same_input_aliases_preserve_rows_but_count_once():
    rows = combine(alias_rows())
    assert len(rows) == 3
    result = summary(rows)
    assert result["measured"] == 1 and result["weighted"] == 0.4
    assert result["rates"]["ci95"] is None
    assert result["latency_ms"]["median"] == 2


def test_aliases_with_different_outputs_are_not_stable_observations():
    rows = alias_rows()
    rows[1].output_hash = digest("changed result")
    assert summary(combine(rows))["measured"] == 0


def test_different_processing_conditions_remain_separate():
    rows = alias_rows("tokenless", "default")
    rows[1].processing_condition = "explicit-condition"
    rows[2].mode = "no-stash"
    assert summary(combine(rows))["measured"] == 3


def test_aliases_do_not_inflate_paired_sample_count():
    rows = combine(alias_rows() + alias_rows("tokenless", "default"))
    table = comparisons(rows)
    cells = [line.split(" | ") for line in table if line.startswith("| L2 |")]
    assert len(cells) == 1 and cells[0][3] == "1"
    header = next(line for line in table if line.startswith("| 层 |"))
    columns = [cell.strip(" |") for cell in header.split(" | ")]
    assert cells[0][columns.index("差值95%估计范围")].startswith("样本不足")


@pytest.mark.parametrize(
    "basis, explanation",
    [
        ("production entry: runtime in-process + rtk wrapped-minus-raw", "含运行时仲裁"),
        ("worker-internal", "不含进程启动、管道传输或JSON封装"),
        ("in-process", "进程内调用计时"),
        ("unknown private value", "具体计时范围未记录或无法识别"),
    ],
)
def test_latency_basis_is_explained_without_publishing_free_text(tmp_path, basis, explanation):
    path = data(tmp_path)["l2"]
    doc = json.loads(path.read_text())
    doc["rows"][0]["row"]["latency_basis"] = basis
    save(path, doc)
    text = report(l2(path))
    assert explanation in text
    assert "unknown private value" not in text
