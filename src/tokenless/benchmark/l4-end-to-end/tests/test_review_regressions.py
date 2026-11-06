# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.

import importlib.util
import json
import os
import re
import subprocess
from pathlib import Path
from types import ModuleType

import pytest

SCRIPTS = Path(__file__).parents[1] / "assets" / "scripts"


def load_analysis() -> ModuleType:
    spec = importlib.util.spec_from_file_location(
        "l4_analysis_review", SCRIPTS / "analyze_runs.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.mark.parametrize("refused", [True, False])
def test_failed_report_is_not_an_instance_only_exclusion(
    tmp_path: Path, refused: bool
) -> None:
    analysis = load_analysis()
    analysis.ARMS = ["a", "b"]
    repeat = tmp_path / "host1" / "out" / "repeat-1"
    repeat.mkdir(parents=True)
    notice = (
        "token-report refused arm b"
        if refused
        else "token-report instance exclusions (per-turn vs aggregate mismatch): bad"
    )
    (repeat / "sweep-summary.txt").write_text(
        "[sweep] sweep repeat=1 arms=a,b instances=2\n"
        "[sweep] --- arm a ---\n[sweep] runner rc=0\n"
        f"[sweep] --- arm b ---\n[sweep] {notice}\n"
        "[sweep] arms whose evidence did not hold: b\n",
        encoding="utf-8",
    )
    for arm, tokens in (("a", 100), ("b", 50)):
        root = repeat / f"arm-{arm}"
        (root / "run" / "results").mkdir(parents=True)
        (root / "report.json").write_text(
            json.dumps(
                {
                    "instances": {
                        "good": {
                            "prompt_tokens": tokens,
                            "requests": 2,
                            "aggregate_agrees": True,
                        },
                        "bad": {"prompt_tokens": tokens, "aggregate_agrees": False},
                    }
                }
            )
        )
        for instance in ("good", "bad"):
            (root / "run" / "results" / f"{instance}.json").write_text(
                json.dumps(
                    {
                        "instance_id": instance,
                        "success": True,
                        "patch_produced": True,
                        "openclaw_returncode": 0,
                    }
                )
            )
    clean = analysis.guard_clean(str(tmp_path), {"host1": "host1"}, 1)
    pool = analysis.paired(str(tmp_path), {"host1": "host1"}, clean, "b", True)
    assert clean["host1"]["b"] == (0 if refused else 1)
    assert pool["n"] == (0 if refused else 1)


def test_provisioned_tokenless_binary_is_rebuilt_incrementally(tmp_path: Path) -> None:
    work = tmp_path / "work"
    binary = work / "tokenless" / "target" / "release" / "tokenless"
    binary.parent.mkdir(parents=True)
    binary.write_text("old-artifact")
    binary.chmod(0o755)
    (work / "logs").mkdir()
    tools = tmp_path / "tools"
    tools.mkdir()
    cargo = tools / "cargo"
    cargo.write_text(
        '#!/bin/sh\nprintf "%s\\n" "$*" >> "$WORK/cargo-calls"\nprintf new-artifact > target/release/tokenless\n'
    )
    cargo.chmod(0o755)
    # The RTK checkout is a path dependency, so the recipe that provisions it has
    # to run before the build; the stub records the order.
    just = tools / "just"
    just.write_text('#!/bin/sh\nprintf "just %s\\n" "$*" >> "$WORK/cargo-calls"\n')
    just.chmod(0o755)
    rtk_manifest = work / "tokenless" / "third_party" / "rtk" / "Cargo.toml"
    rtk_manifest.parent.mkdir(parents=True)
    rtk_manifest.write_text("[package]\n")
    source = (SCRIPTS / "remote_setup.sh").read_text()
    begin = source.index('step "tokenless"')
    end = source.index("# --- 8. headroom venv", begin)
    env = dict(
        os.environ,
        WORK=str(work),
        LOG=str(work / "logs"),
        PATH=f"{tools}:{os.environ['PATH']}",
    )
    result = subprocess.run(
        ["bash", "-c", "step() { :; }; fail() { exit 1; };\n" + source[begin:end]],
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    assert binary.read_text() == "new-artifact"
    calls = (work / "cargo-calls").read_text().splitlines()
    assert calls.index("just setup-rtk") < calls.index("build --release --locked")
    # RTK and TOON are linked in, so nothing else is built or installed.
    assert not [call for call in calls if "manifest-path" in call or "toon" in call]


@pytest.mark.parametrize("refusal,replaced", [
    ("RTK checkout does not match pinned commit b1c0dc0; run just clean-rtk and retry", True),
    ("fatal: unable to access 'https://github.com/rtk-ai/rtk.git/'", False),
])
def test_setup_replaces_an_rtk_checkout_only_when_the_recipe_asks(
    tmp_path: Path, refusal: str, replaced: bool,
) -> None:
    work, tools = tmp_path / "work", tmp_path / "tools"
    (work / "logs").mkdir(parents=True)
    (work / "tokenless" / "third_party" / "rtk").mkdir(parents=True)
    (work / "tokenless" / "third_party" / "rtk" / "Cargo.toml").write_text("[package]\n")
    tools.mkdir()
    (tools / "cargo").write_text('#!/bin/sh\nmkdir -p target/release && : > target/release/tokenless\n')
    # The first setup-rtk refuses; a clean-rtk makes the next one succeed.
    (tools / "just").write_text(
        '#!/bin/sh\nprintf "%s\\n" "$1" >> "$WORK/just-calls"\n'
        'case $1 in clean-rtk) : > "$WORK/cleaned";;\n'
        'setup-rtk) [ -e "$WORK/cleaned" ] || { echo "ERROR: $REFUSAL" >&2; exit 1; };; esac\n'
    )
    for stub in (tools / "cargo", tools / "just"):
        stub.chmod(0o755)
    source = (SCRIPTS / "remote_setup.sh").read_text()
    begin = source.index('step "tokenless"')
    end = source.index("# --- 8. headroom venv", begin)
    env = dict(os.environ, WORK=str(work), LOG=str(work / "logs"), REFUSAL=refusal, CARGO_HOME=str(tools.parent),
               PATH=f"{tools}:{os.environ['PATH']}")
    result = subprocess.run(["bash", "-c", 'step() { :; }; fail() { echo "FAIL:$1"; exit 1; };\n' + source[begin:end]],
                            env=env, check=False, capture_output=True, text=True)
    calls = (work / "just-calls").read_text().split()
    if replaced:
        assert result.returncode == 0, result.stdout + result.stderr
        assert calls == ["setup-rtk", "clean-rtk", "setup-rtk"]
    else:
        assert result.returncode != 0 and "FAIL:setup_rtk" in result.stdout
        assert calls == ["setup-rtk"]


def test_evaluation_completion_tracks_predictions(tmp_path: Path) -> None:
    work = tmp_path / "work"
    preds = work / "out" / "repeat-1" / "arm-a" / "run" / "preds.json"
    preds.parent.mkdir(parents=True)
    preds.write_text('{"revision":1}')
    (preds.parent / "run_metadata.json").write_text('{"instance_ids":["fixture"]}')
    runner = work / "venv-swe" / "bin" / "swe-runner"
    runner.parent.mkdir(parents=True)
    runner.write_text("""#!/bin/sh
while [ "$#" -gt 0 ]; do
    case "$1" in
        --predictions) predictions=$2; shift 2 ;;
        --output) output=$2; shift 2 ;;
        --run-id) run_id=$2; shift 2 ;;
        *) shift ;;
    esac
done
cat "$predictions" > "$output/scored.json"
mkdir -p "$output/evaluate"
printf '{"submitted_instances":1,"resolved_ids":["fixture"]}\n' > "$output/evaluate/fixture.$run_id.json"
printf '%s\n' called >> "$L4_REMOTE_WORK/evaluation-calls"
exit "${STUB_EVAL_RC:-0}"
""")
    runner.chmod(0o755)
    env = dict(
        os.environ, L4_REMOTE_WORK=str(work), L4_EVAL_REPEATS="1", L4_EVAL_ARMS="a"
    )

    def evaluate(rc: str = "0") -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(SCRIPTS / "evaluate_all.sh")],
            env=dict(env, STUB_EVAL_RC=rc),
            check=False,
            capture_output=True,
            text=True,
        )

    assert evaluate().returncode == 0
    assert evaluate().returncode == 0
    assert (work / "evaluation-calls").read_text().splitlines() == ["called"]
    preds.write_text('{"revision":2}')
    assert evaluate().returncode == 0
    assert json.loads(
        (work / "eval" / "repeat-1-arm-a" / "scored.json").read_text()
    ) == {"revision": 2}
    assert len((work / "evaluation-calls").read_text().splitlines()) == 2
    spec = importlib.util.spec_from_file_location(
        "l4_solve_review", SCRIPTS / "analyze_solve_rate.py"
    )
    solve = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(solve)
    assert len(solve.load_reports(str(work / "eval"))) == 1
    preds.write_text('{"revision":3}')
    assert evaluate("1").returncode != 0
    assert solve.load_reports(str(work / "eval")) == []
    assert evaluate().returncode == 0
    assert len(solve.load_reports(str(work / "eval"))) == 1
    assert len((work / "evaluation-calls").read_text().splitlines()) == 4


@pytest.mark.parametrize("mode", ["unreadable", "mismatch", "success"])
def test_report_retry_cannot_reuse_old_report(tmp_path: Path, mode: str) -> None:
    out = tmp_path / "arm-b"
    out.mkdir()
    stale = {
        "totals": {"instances_with_disagreeing_aggregate": ["stale"]},
        "revision": "old",
    }
    report = out / "report.json"
    report.write_text(json.dumps(stale))
    runner = tmp_path / "runner"
    runner.write_text("""#!/bin/sh
while [ "$#" -gt 0 ]; do
    case "$1" in --output) output=$2; shift 2 ;; *) shift ;; esac
done
[ "$REPORT_MODE" = unreadable ] && exit 1
printf '{"totals":{"instances_with_disagreeing_aggregate":["current"]},"revision":"new"}' > "$output"
[ "$REPORT_MODE" = mismatch ] && exit 1
exit 0
""")
    runner.chmod(0o755)
    source = (SCRIPTS / "remote_run.sh").read_text()
    begin = re.search(r"    local report_(?:started|candidate)\n", source).start()
    end = source.index("\n}", begin)
    harness = (
        'note() { printf "%s\\n" "$*"; }; run_report() { local ok=1 arm=b;\n'
        + source[begin:end]
        + "\n}; run_report"
    )
    result = subprocess.run(
        ["bash", "-c", harness],
        check=False,
        capture_output=True,
        text=True,
        env=dict(
            os.environ,
            out=str(out),
            RUNNER=str(runner),
            SUMMARY=str(tmp_path / "summary"),
            REPORT_MODE=mode,
        ),
    )
    assert result.returncode == (1 if mode == "unreadable" else 0), result.stderr
    if mode == "unreadable":
        assert json.loads(report.read_text()) == stale
        assert "token-report refused arm b" in result.stdout
    else:
        assert json.loads(report.read_text())["revision"] == "new"


@pytest.mark.parametrize("script", ["remote_sync.sh", "remote_setup.sh", "remote_verify.sh", "remote_fleet.sh"])
@pytest.mark.parametrize("explicit", [False, True])
def test_ssh_uses_default_identity_unless_key_is_explicit(
    tmp_path: Path, script: str, explicit: bool
) -> None:
    source = (SCRIPTS / script).read_text()
    start = source.index("SSH_OPTS=(")
    end = source.index("\nfi", start) + len("\nfi")
    key = tmp_path / "explicit-key"
    key.write_text("fixture-not-a-real-private-key")
    env = dict(os.environ)
    env.pop("L4_SSH_KEY", None)
    if explicit:
        env["L4_SSH_KEY"] = str(key)
    result = subprocess.run(
        ["bash", "-c", source[start:end] + '\nprintf "%s\\n" "${SSH_OPTS[@]}"; printf "pw_count=%s\\n" "${#SSH_PW[@]}"'],
        env=env, check=False, capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr
    assert "StrictHostKeyChecking=yes" in result.stdout
    assert "BatchMode=yes" in result.stdout
    assert "pw_count=0" in result.stdout
    assert "UserKnownHostsFile=/dev/null" not in result.stdout
    assert ("IdentitiesOnly=yes" in result.stdout) == explicit
    assert (str(key) in result.stdout) == explicit


def test_model_snapshot_and_memory_isolation_are_shared_by_every_arm() -> None:
    configs = SCRIPTS.parent / "configs"
    for path in configs.glob("arm-*.json"):
        document = json.loads(path.read_text())
        assert document["agents"]["defaults"]["model"]["primary"] == "bench-qwen/qwen3.8-max-0902"
        provider = document["models"]["providers"]["bench-qwen"]
        assert provider["apiKey"] == "${DASHSCOPE_API_KEY}"
        assert provider["models"][0]["id"] == "qwen3.8-max-0902"
        assert "cost" not in provider["models"][0]
        assert document["plugins"]["slots"]["memory"] == "none"
    assert len(list(configs.glob("arm-*.json"))) == 6


@pytest.mark.parametrize("channel,expected", [("stable", 1), ("canary", 0), ("dev", 0)])
def test_interceptor_arm_does_not_silently_run_on_a_disabled_rollout(
    channel: str, expected: int
) -> None:
    source = (SCRIPTS / "remote_run.sh").read_text()
    start = source.index("arm_spec() {")
    end = source.index("\n}", start) + len("\n}")
    result = subprocess.run(
        ["bash", "-c", 'die() { printf "%s\\n" "$*" >&2; exit 1; };\n' + source[start:end] + '\narm_spec d; printf "%s\\n" "${PFLAGS[@]}"'],
        env=dict(os.environ, HEADROOM_ROLLOUT_CHANNEL=channel),
        check=False, capture_output=True, text=True,
    )
    assert result.returncode == expected, result.stderr
    if expected == 0:
        assert "--intercept-tool-results" in result.stdout


def test_proxy_default_mode_expectation_matches_current_cache_default() -> None:
    source = (SCRIPTS / "remote_run.sh").read_text()
    harness = ""
    for name in ("arm_expect_present", "arm_expect_absent"):
        start = source.index(f"{name}() {{")
        harness += source[start:source.index("\n}", start) + len("\n}")] + "\n"
    for arm in "cdef":
        result = subprocess.run(["bash", "-c", harness + f"arm_expect_present {arm}; echo ---; arm_expect_absent {arm}"],
                                check=False, capture_output=True, text=True)
        assert result.returncode == 0
        present, absent = (part.split() for part in result.stdout.split("---"))
        # The refreshed proxy defaults to cache mode with code-aware compression on.
        assert "Mode:[[:space:]]+token" in absent and "Code-Aware:[[:space:]]+DISABLED" in absent
        assert not set(present) & set(absent)
        if arm == "c":
            assert set(present) == {"Mode:[[:space:]]+cache", "Code-Aware:[[:space:]]+ENABLED"}
