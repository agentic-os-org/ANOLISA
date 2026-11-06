# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.

import importlib.util
import json
import os
import re
import sqlite3
import subprocess
import sys
from pathlib import Path
from types import ModuleType

import pytest

SCRIPTS = Path(__file__).parents[1] / "assets" / "scripts"
TOKENLESS = Path(__file__).parents[3]
SWE_RUNNER_SRC = Path(__file__).parents[5] / "benchmark" / "swe-runner" / "src"
# Scripts that execute on the shared benchmark host. remote_sync.sh runs on the
# operator's machine and is checked separately.
HOST_SCRIPTS = [
    "remote_setup.sh",
    "remote_verify.sh",
    "remote_run.sh",
    "evaluate_all.sh",
    "clean_between_repeats.sh",
    "run_pilot.sh",
]


def load_analysis(name: str = "analyze_runs") -> ModuleType:
    spec = importlib.util.spec_from_file_location(
        f"l4_{name}_review", SCRIPTS / f"{name}.py"
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


@pytest.mark.parametrize(
    "refusal,replaced",
    [
        (
            "RTK checkout does not match pinned commit b1c0dc0; run just clean-rtk and retry",
            True,
        ),
        ("fatal: unable to access 'https://github.com/rtk-ai/rtk.git/'", False),
    ],
)
def test_setup_replaces_an_rtk_checkout_only_when_the_recipe_asks(
    tmp_path: Path,
    refusal: str,
    replaced: bool,
) -> None:
    work, tools = tmp_path / "work", tmp_path / "tools"
    (work / "logs").mkdir(parents=True)
    (work / "tokenless" / "third_party" / "rtk").mkdir(parents=True)
    (work / "tokenless" / "third_party" / "rtk" / "Cargo.toml").write_text(
        "[package]\n"
    )
    tools.mkdir()
    (tools / "cargo").write_text(
        "#!/bin/sh\nmkdir -p target/release && : > target/release/tokenless\n"
    )
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
    env = dict(
        os.environ,
        WORK=str(work),
        LOG=str(work / "logs"),
        REFUSAL=refusal,
        CARGO_HOME=str(tools.parent),
        PATH=f"{tools}:{os.environ['PATH']}",
    )
    result = subprocess.run(
        [
            "bash",
            "-c",
            'step() { :; }; fail() { echo "FAIL:$1"; exit 1; };\n' + source[begin:end],
        ],
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
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
    assert solve.load_reports(str(work / "eval")) == []
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


@pytest.mark.parametrize(
    "script",
    ["remote_sync.sh", "remote_setup.sh", "remote_verify.sh", "remote_fleet.sh"],
)
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
        [
            "bash",
            "-c",
            source[start:end]
            + '\nprintf "%s\\n" "${SSH_OPTS[@]}"; printf "pw_count=%s\\n" "${#SSH_PW[@]}"',
        ],
        env=env,
        check=False,
        capture_output=True,
        text=True,
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
        assert (
            document["agents"]["defaults"]["model"]["primary"]
            == "bench-qwen/qwen3.8-max-0902"
        )
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
        [
            "bash",
            "-c",
            'die() { printf "%s\\n" "$*" >&2; exit 1; };\n'
            + source[start:end]
            + '\narm_spec d; printf "%s\\n" "${PFLAGS[@]}"',
        ],
        env=dict(os.environ, HEADROOM_ROLLOUT_CHANNEL=channel),
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == expected, result.stderr
    if expected == 0:
        assert "--intercept-tool-results" in result.stdout


def test_proxy_default_mode_expectation_matches_current_cache_default() -> None:
    source = (SCRIPTS / "remote_run.sh").read_text()
    harness = ""
    for name in ("arm_expect_present", "arm_expect_absent"):
        start = source.index(f"{name}() {{")
        harness += source[start : source.index("\n}", start) + len("\n}")] + "\n"
    for arm in "cdef":
        result = subprocess.run(
            [
                "bash",
                "-c",
                harness
                + f"arm_expect_present {arm}; echo ---; arm_expect_absent {arm}",
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0
        present, absent = (part.split() for part in result.stdout.split("---"))
        # The refreshed proxy defaults to cache mode with code-aware compression on.
        assert (
            "Mode:[[:space:]]+token" in absent
            and "Code-Aware:[[:space:]]+DISABLED" in absent
        )
        assert not set(present) & set(absent)
        if arm == "c":
            assert set(present) == {
                "Mode:[[:space:]]+cache",
                "Code-Aware:[[:space:]]+ENABLED",
            }


def code_lines(script: str) -> list[str]:
    return [
        line
        for line in (SCRIPTS / script).read_text().splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]


@pytest.mark.parametrize("script", HOST_SCRIPTS)
def test_host_scripts_never_touch_the_host_users_installation(script: str) -> None:
    # The benchmark shares its host with a user's own OpenClaw and tokenless.
    # Comments may name these locations (to say they are left alone); code may not.
    forbidden = (
        "/root/.openclaw",
        "/root/.tokenless",
        "/root/.cargo",
        "/root/.bashrc",
        "/root/.local",
        "/opt/node",
        "/usr/local/bin/",
        "dnf install",
        "yum install",
        "npm install -g --prefix /usr",
    )
    offending = [
        line for line in code_lines(script) for item in forbidden if item in line
    ]
    assert offending == []
    assert any("l4_env.sh" in line for line in code_lines(script)), (
        "must source the shared environment"
    )


def test_shared_environment_resolves_only_workspace_binaries_and_state(
    tmp_path: Path,
) -> None:
    work = tmp_path / "w"
    result = subprocess.run(
        ["bash", "-c", f'. "{SCRIPTS / "l4_env.sh"}" "{work}"; env'],
        env={
            "PATH": "/root/.local/bin:/root/.nvm/versions/node/v26.1.0/bin:/usr/bin:/bin",
            "HOME": "/root",
        },
        check=True,
        capture_output=True,
        text=True,
    )
    env = dict(line.split("=", 1) for line in result.stdout.splitlines() if "=" in line)
    home = f"{work}/home"
    assert env["HOME"] == home
    path = env["PATH"].split(":")
    assert path[:2] == [f"{home}/.local/bin", f"{work}/node/bin"]
    assert not any(entry.startswith("/root") for entry in path)
    assert env["TOKENLESS_DATA_DIR"] == f"{home}/.tokenless"
    share = f"{home}/.local/share/anolisa/adapters/tokenless"
    assert env["TOKENLESS_TOOL_READY_SPEC"] == f"{share}/common/tool-ready-spec.json"
    assert env["TOKENLESS_ENV_FIX_SCRIPT"] == f"{share}/common/tokenless-env-fix.sh"
    assert (
        env["TOKENLESS_COMPRESSION_ENABLED"],
        env["TOKENLESS_STATS_ENABLED"],
        env["TOKENLESS_SLS_ENABLED"],
    ) == ("1", "1", "0")
    assert (env["CARGO_HOME"], env["RUSTUP_HOME"]) == (
        f"{work}/cargo",
        f"{work}/rustup",
    )
    assert env["L4_OPENCLAW_VERSION"] == "2026.8.2"


@pytest.mark.parametrize("makeflags", ["", "w"])
def test_shared_environment_matches_where_the_component_installs(
    tmp_path: Path,
    makeflags: str,
) -> None:
    # l4_env.sh hard-codes the user-profile layout; this ties it to the Makefile
    # the setup actually runs, so a layout change cannot silently send the
    # plugin to a host-wide binary.
    home = tmp_path / "home"
    home.mkdir()
    printer = tmp_path / "print.mk"
    printer.write_text("print-layout:\n\t@echo $(SHARE_DIR) $(BINDIR)\n")
    # 嵌套 make 会继承 -w；机器可读 stdout 必须显式关闭目录提示。
    result = subprocess.run(
        [
            "make",
            "-s",
            "-f",
            "Makefile",
            "-f",
            str(printer),
            "--no-print-directory",
            "print-layout",
            f"HOME={home}",
            "INSTALL_PROFILE=user",
        ],
        cwd=TOKENLESS,
        env=dict(os.environ, MAKEFLAGS=makeflags),
        check=True,
        capture_output=True,
        text=True,
    )
    share, bindir = result.stdout.split()
    assert share == f"{home}/.local/share/anolisa/adapters/tokenless"
    assert bindir == f"{home}/.local/bin"
    for name in ("tool-ready-spec.json", "tokenless-env-fix.sh"):
        assert (TOKENLESS / "adapters" / "tokenless" / "common" / name).is_file()


@pytest.mark.parametrize("switch,expected", [("1", ["--log-messages"]), ("0", [])])
def test_proxy_message_logging_follows_the_capture_switch(
    switch: str, expected: list[str]
) -> None:
    source = (SCRIPTS / "remote_run.sh").read_text()
    start = source.index("L4_CAPTURE_TOOL_RESULTS=${")
    end = source.index("SWEEP=$W/out/repeat-", start)
    result = subprocess.run(
        [
            "bash",
            "-c",
            source[start:end]
            + 'printf "%s\\n" ${CAPTURE_FLAGS[@]+"${CAPTURE_FLAGS[@]}"}',
        ],
        env=dict(os.environ, L4_CAPTURE_TOOL_RESULTS=switch),
        check=True,
        capture_output=True,
        text=True,
    )
    assert result.stdout.split() == expected


def run_capture(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPTS / "extract_tool_results.py"), *args],
        check=False,
        capture_output=True,
        text=True,
    )


def test_tokenless_capture_keeps_changed_results_from_this_arm_only(
    tmp_path: Path,
) -> None:
    db = tmp_path / "stats.db"
    connection = sqlite3.connect(db)
    connection.execute(
        "CREATE TABLE stats (id INTEGER PRIMARY KEY, timestamp_ns INTEGER, operation TEXT, session_id TEXT, "
        "tool_use_id TEXT, before_text TEXT, after_text TEXT, content_type TEXT, recoverability TEXT, "
        "applied_operations TEXT, before_tokens INTEGER, after_tokens INTEGER)"
    )
    long = "x" * 300
    since_ns = 1_800_000_000 * 10**9
    rows = [
        (
            since_ns - 1,
            "compress-response",
            "django__django-13158-1a2b3c4d",
            "call_old",
            long,
            "short",
        ),
        (
            since_ns + 1,
            "compress-response",
            "django__django-13158-12ab34cd",
            "call_new",
            long,
            "short",
        ),
        (
            since_ns + 2,
            "rewrite-command",
            "django__django-13158-12ab34cd",
            "call_cmd",
            long,
            "short",
        ),
        (
            since_ns + 3,
            "compress-response",
            "django__django-13158-12ab34cd",
            "call_tiny",
            "tiny",
            "t",
        ),
    ]
    connection.executemany(
        "INSERT INTO stats (timestamp_ns, operation, session_id, tool_use_id, before_text, after_text, "
        "content_type, recoverability, applied_operations, before_tokens, after_tokens) "
        "VALUES (?, ?, ?, ?, ?, ?, 'text', 'recoverable', '[]', 75, 1)",
        rows,
    )
    connection.commit()
    instances = tmp_path / "instances.txt"
    instances.write_text("django__django-1315\ndjango__django-13158\n")
    output = tmp_path / "tool-results.json"
    result = run_capture(
        "--arm",
        "b",
        "--since",
        "2027-01-15T08:00:00Z",
        "--output",
        str(output),
        "--stats-db",
        str(db),
        "--instance-list",
        str(instances),
    )
    assert result.returncode == 0, result.stderr
    document = json.loads(output.read_text())
    assert document["changed_results"] == 2
    assert [(row["call_id"], row["instance"]) for row in document["rows"]] == [
        ("call_new", "django__django-13158")
    ]
    assert document["rows"][0]["original"] == long


def test_proxy_capture_pairs_tool_results_by_call_id(tmp_path: Path) -> None:
    original = "line\n" * 80
    request = [
        {"role": "user", "content": "fix the bug"},
        {
            "role": "assistant",
            "tool_calls": [
                {"id": "call_keep", "function": {"name": "exec"}},
                {"id": "call_same", "function": {"name": "read"}},
            ],
        },
        {
            "role": "tool",
            "tool_call_id": "call_keep",
            "content": [{"type": "text", "text": original}],
        },
        {"role": "tool", "tool_call_id": "call_same", "content": original},
    ]
    # The proxy dropped the user message, so pairing by position would misalign.
    compressed = [
        request[1],
        {
            "role": "tool",
            "tool_call_id": "call_keep",
            "content": "line\n[78 lines omitted, hash=abc]",
        },
        request[3],
    ]
    log = tmp_path / "proxy.jsonl"
    log.write_text(
        "\n".join(
            json.dumps(entry)
            for entry in (
                {
                    "timestamp": "2027-01-15T07:59:00+00:00",
                    "request_messages": request,
                    "compressed_messages": compressed,
                },
                {
                    "timestamp": "2027-01-15T08:01:00+00:00",
                    "request_messages": request,
                    "compressed_messages": compressed,
                    "transforms_applied": ["smart_crusher"],
                },
                "not json",
            )
        )
        + "\nnot json\n"
    )
    store = (
        tmp_path
        / "profiles"
        / "astropy__astropy-12907"
        / "agents"
        / "main"
        / "agent"
        / "openclaw-agent.sqlite"
    )
    store.parent.mkdir(parents=True)
    connection = sqlite3.connect(store)
    connection.execute(
        "CREATE TABLE transcript_events (session_id TEXT, seq INTEGER, event_json TEXT)"
    )
    connection.execute(
        "INSERT INTO transcript_events VALUES ('s', 1, ?)",
        (json.dumps({"message": {"role": "toolResult", "toolCallId": "call_keep"}}),),
    )
    connection.commit()
    instances = tmp_path / "instances.txt"
    instances.write_text("astropy__astropy-12907\n")
    output = tmp_path / "tool-results.json"
    result = run_capture(
        "--arm",
        "c",
        "--since",
        "2027-01-15T08:00:00Z",
        "--output",
        str(output),
        "--proxy-log",
        str(log),
        "--instance-list",
        str(instances),
        "--profiles-dir",
        str(tmp_path / "profiles"),
    )
    assert result.returncode == 0, result.stderr
    rows = json.loads(output.read_text())["rows"]
    assert [(row["call_id"], row["tool_name"], row["instance"]) for row in rows] == [
        ("call_keep", "exec", "astropy__astropy-12907")
    ]
    assert rows[0]["original"] == original and "omitted" in rows[0]["compressed"]
    assert rows[0]["transforms_applied"] == ["smart_crusher"]


def test_capture_reports_a_missing_source_instead_of_an_empty_sample(
    tmp_path: Path,
) -> None:
    result = run_capture(
        "--arm",
        "b",
        "--since",
        "2027-01-15T08:00:00Z",
        "--output",
        str(tmp_path / "out.json"),
        "--stats-db",
        str(tmp_path / "absent.db"),
    )
    assert result.returncode == 1
    assert not (tmp_path / "out.json").exists()


def write_agent_store(profile: Path, turns: list[tuple[int, int, int]]) -> None:
    store = profile / "agents" / "main" / "agent" / "openclaw-agent.sqlite"
    store.parent.mkdir(parents=True)
    connection = sqlite3.connect(store)
    connection.execute(
        "CREATE TABLE transcript_events (session_id TEXT, seq INTEGER, event_json TEXT)"
    )
    connection.execute("CREATE TABLE trajectory_runtime_events (event_json TEXT)")
    for seq, (fresh, cached, out) in enumerate(turns):
        usage = {"input": fresh, "cacheRead": cached, "output": out}
        connection.execute(
            "INSERT INTO transcript_events VALUES ('s', ?, ?)",
            (seq, json.dumps({"message": {"role": "assistant", "usage": usage}})),
        )
    connection.commit()


@pytest.mark.parametrize("cap,expected_rc", [("100", 0), ("6.4", 3)])
def test_pilot_ledger_prices_live_stores_and_stops_below_the_cap(
    tmp_path: Path, cap: str, expected_rc: int
) -> None:
    assert (SWE_RUNNER_SRC / "swe_runner").is_dir(), (
        "billing regression needs the real runner"
    )
    sys.path.insert(0, str(SWE_RUNNER_SRC))
    importlib.import_module("swe_runner.trace_extraction.openclaw_sqlite")
    sweep = tmp_path / "out" / "repeat-1"
    profiles = sweep / "arm-a" / "run" / "openclaw-profiles"
    write_agent_store(
        profiles / "django__django-13158",
        [(60_000, 240_000, 4_000), (40_000, 160_000, 6_000)],
    )
    # An instance that has only just started has a profile but no store yet.
    (sweep / "arm-b" / "run" / "openclaw-profiles" / "django__django-12284").mkdir(
        parents=True
    )
    source = (SCRIPTS / "run_pilot.sh").read_text()
    start = source.index("ledger() {")
    end = source.index("\nPY\n}", start) + len("\nPY\n}")
    result = subprocess.run(
        ["bash", "-c", source[start:end] + '\nledger; echo "rc=$?"'],
        env=dict(
            os.environ,
            SWEEP=str(sweep),
            CAP=cap,
            MARGIN="0.1",
            L4_ARMS="a,b,c",
            PY_SWE=sys.executable,
            PYTHONPATH=str(SWE_RUNNER_SRC),
        ),
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.stdout.splitlines()[-1] == f"rc={expected_rc}", result.stderr
    ledger = json.loads((sweep / "pilot-cost.json").read_text())
    arm_a = ledger["arms"]["a"]
    # (100k fresh + 400k cached) at 12/M plus 10k output at 36/M; cached at 1.5/M in the discounted view.
    assert arm_a["cost_upper_bound"] == pytest.approx(6.36)
    assert arm_a["cost_with_published_cache_price"] == pytest.approx(2.16)
    assert ledger["arms"]["b"]["instances_started"] == 0
    assert "c" not in ledger["arms"]
    # Arms not reached are estimated at the mean of the measured ones: 6.36 x 3 arms x 48 x 5.
    assert ledger["full_scale_estimate"]["upper_bound"] == pytest.approx(
        6.36 * 3 * 48 * 5
    )


def test_headroom_clone_on_the_host_is_pinned_and_refuses_foreign_trees(
    tmp_path: Path,
) -> None:
    source = (SCRIPTS / "remote_sync.sh").read_text()
    start = source.index("clone_script=$(cat <<'SH'\n") + len(
        "clone_script=$(cat <<'SH'\n"
    )
    script = source[start : source.index("\nSH\n)", start)]
    repo = tmp_path / "headroom-src"
    git = [
        "git",
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-C",
        str(repo),
    ]
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    (repo / "rust-toolchain.toml").write_text('channel = "1.95.0"\n')
    subprocess.run([*git, "add", "."], check=True)
    subprocess.run([*git, "commit", "-qm", "pin"], check=True)
    pinned = subprocess.run(
        [*git, "rev-parse", "HEAD"], check=True, capture_output=True, text=True
    ).stdout.strip()
    (repo / "later.txt").write_text("later\n")
    subprocess.run([*git, "add", "."], check=True)
    subprocess.run([*git, "commit", "-qm", "later"], check=True)

    def clone(dest: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", "-c", script, "clone", str(repo), pinned, str(dest)],
            check=False,
            capture_output=True,
            text=True,
        )

    dest = tmp_path / "work" / "headroom"
    first = clone(dest)
    assert first.returncode == 0, first.stderr
    assert pinned in first.stdout and not (dest / "later.txt").exists()
    assert clone(dest).returncode == 0
    (dest / "rust-toolchain.toml").write_text('channel = "stable"\n')
    assert clone(dest).returncode != 0
    foreign = tmp_path / "work" / "foreign"
    foreign.mkdir()
    assert clone(foreign).returncode != 0


@pytest.mark.parametrize("constrained", [False, True])
@pytest.mark.parametrize("revision_known", [False, True])
def test_headroom_venv_honours_the_reference_constraints(
    tmp_path: Path, constrained: bool, revision_known: bool
) -> None:
    work, log, tools, stubs = (
        tmp_path / "work",
        tmp_path / "work" / "logs",
        tmp_path / "tools",
        tmp_path / "stubs",
    )
    log.mkdir(parents=True)
    tools.mkdir()
    stubs.mkdir()
    # `python3 -m venv DIR` yields a venv whose pip records its arguments and whose
    # python fails every import probe, so each install step runs.
    (stubs / "pip").write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$CALLS"\n')
    (stubs / "python").write_text("#!/bin/sh\nexit 1\n")
    (tools / "python3").write_text(
        '#!/bin/sh\nmkdir -p "$3/bin" && cp "$STUBS/pip" "$STUBS/python" "$3/bin/"\n'
    )
    for path in (stubs / "pip", stubs / "python", tools / "python3"):
        path.chmod(0o755)
    (tools / "git").write_text(
        "#!/bin/sh\necho fixture-revision\n"
        if revision_known
        else "#!/bin/sh\nexit 1\n"
    )
    (tools / "git").chmod(0o755)
    constraints = tmp_path / "reference-freeze.txt"
    constraints.write_text("litellm==1.103.10\n")
    source = (SCRIPTS / "remote_setup.sh").read_text()
    begin = source.index('step "headroom venv"')
    end = source.index("# --- 9. swe-runner venv", begin)
    harness = (
        'step() { :; }; fail() { echo "FAIL:$1"; exit 1; };\n'
        'pip_bootstrap() { "$1/bin/pip" install --no-cache-dir --upgrade pip setuptools wheel; };\n'
        + source[begin:end]
    )
    env = dict(
        os.environ,
        WORK=str(work),
        LOG=str(log),
        CALLS=str(tmp_path / "calls"),
        STUBS=str(stubs),
        PATH=f"{tools}:{os.environ['PATH']}",
    )
    env.pop("L4_HEADROOM_CONSTRAINTS", None)
    if constrained:
        env["L4_HEADROOM_CONSTRAINTS"] = str(constraints)
    result = subprocess.run(
        ["bash", "-c", harness], env=env, check=False, capture_output=True, text=True
    )
    assert result.returncode == 0, result.stdout + result.stderr
    calls = (tmp_path / "calls").read_text().splitlines()
    installs = [
        call
        for call in calls
        if "headroom[proxy]" in call or "tree-sitter-language-pack" in call
    ]
    assert len(installs) == 2
    flag = f"-c {log}/headroom_constraints.txt"
    assert all((flag in call) == constrained for call in installs)
    assert (log / "headroom_constraints.txt").exists() == constrained
    assert calls[-1] == "freeze"
    # 已有 native 模块也不能覆盖新的约束；第二次变更必须重新安装。
    (work / "venv-headroom" / "bin" / "python").write_text("#!/bin/sh\nexit 0\n")
    if constrained:
        constraints.write_text("litellm==1.103.11\n")
    else:
        (work / "venv-headroom" / ".l4-reference").write_text("other revision\n")
    retry = subprocess.run(
        ["bash", "-c", harness], env=env, check=False, capture_output=True, text=True
    )
    assert retry.returncode == 0, retry.stdout + retry.stderr
    assert (tmp_path / "calls").read_text().count("headroom[proxy]") == 2
    (work / "venv-headroom" / "bin" / "python").write_text("#!/bin/sh\nexit 0\n")
    stable = subprocess.run(
        ["bash", "-c", harness], env=env, capture_output=True, text=True
    )
    assert stable.returncode == 0, stable.stdout + stable.stderr
    assert (tmp_path / "calls").read_text().count("headroom[proxy]") == (
        2 if revision_known else 3
    )


@pytest.mark.parametrize(
    "installed,config_stamp,npm_spec,failure",
    [
        ("2026.11.1", None, "openclaw@2026.8.2", None),
        ("2026.8.2", "2026.8.2", None, None),
        ("2026.8.2", "2026.11.1", None, "FAIL:openclaw_config_from_other_release"),
    ],
)
def test_setup_installs_the_pinned_openclaw_and_refuses_a_foreign_config(
    tmp_path: Path,
    installed: str,
    config_stamp: str | None,
    npm_spec: str | None,
    failure: str | None,
) -> None:
    node_bin, log, home = (
        tmp_path / "node" / "bin",
        tmp_path / "logs",
        tmp_path / "home",
    )
    for path in (node_bin, log, home / ".openclaw"):
        path.mkdir(parents=True)
    release = tmp_path / "release"
    release.write_text(installed)
    # The stub npm installs whatever release it is asked for; the stub openclaw
    # reports it in the real banner format.
    (node_bin / "openclaw").write_text(
        '#!/bin/sh\necho "OpenClaw $(cat "$RELEASE") (0965053)"\n'
    )
    (node_bin / "npm").write_text(
        '#!/bin/sh\nprintf "%s\\n" "$*" >> "$CALLS"\n'
        'for arg; do case $arg in openclaw@*) printf "%s" "${arg#openclaw@}" > "$RELEASE";; esac; done\n'
    )
    for stub in (node_bin / "openclaw", node_bin / "npm"):
        stub.chmod(0o755)
    if config_stamp:
        (home / ".openclaw" / "openclaw.json").write_text(
            json.dumps({"meta": {"lastTouchedVersion": config_stamp}})
        )
    source = (SCRIPTS / "remote_setup.sh").read_text()
    begin = source.index('step "openclaw $L4_OPENCLAW_VERSION"')
    end = source.index("# headroom's openclaw plugin keeps", begin)
    harness = 'step() { :; }; fail() { echo "FAIL:$1"; exit 1; };\n' + source[begin:end]
    env = dict(
        os.environ,
        NODE_DIR=str(node_bin.parent),
        LOG=str(log),
        HOME=str(home),
        RELEASE=str(release),
        CALLS=str(tmp_path / "calls"),
        L4_OPENCLAW_VERSION="2026.8.2",
        PATH=f"{node_bin}:{os.environ['PATH']}",
    )
    result = subprocess.run(
        ["bash", "-c", harness], env=env, check=False, capture_output=True, text=True
    )
    calls = (
        (tmp_path / "calls").read_text().split()
        if (tmp_path / "calls").exists()
        else []
    )
    assert (npm_spec in calls) if npm_spec else calls == []
    if failure:
        assert result.returncode != 0 and failure in result.stdout
    else:
        assert result.returncode == 0, result.stdout + result.stderr
        assert (log / "openclaw_version.txt").read_text().split()[1] == "2026.8.2"


def test_headroom_clone_requires_an_explicit_revision(tmp_path: Path) -> None:
    result = subprocess.run(
        ["bash", str(SCRIPTS / "remote_sync.sh")],
        env=dict(
            os.environ,
            L4_SSH_HOST="host.invalid",
            HEADROOM_REMOTE_CLONE="/opt/headroom",
            HEADROOM_REV="",
        ),
        check=False,
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert result.returncode == 1
    assert "HEADROOM_REV" in result.stderr


@pytest.mark.parametrize("partial", ["b", "a,b"])
def test_interrupted_sweep_revokes_previous_clean_verdict(
    tmp_path: Path, partial: str
) -> None:
    analysis = load_analysis()
    analysis.ARMS = ["a", "b"]
    repeat = tmp_path / "host1" / "out" / "repeat-1"
    repeat.mkdir(parents=True)
    (repeat / "sweep-summary.txt").write_text(
        "[sweep] sweep repeat=1 arms=a,b instances=1\n"
        "[sweep] all arms produced evidence-backed results\n"
        f"[sweep] sweep repeat=1 arms={partial} instances=1\n"
        "[sweep] --- arm b config=arm-b.json\n"
    )
    with pytest.raises(SystemExit, match="no verdict"):
        analysis.guard_clean(str(tmp_path), {"host1": "host1"}, 1)
    base = repeat / "arm-b"
    (base / "run" / "results").mkdir(parents=True)
    (base / "report.json").write_text(
        json.dumps({"instances": {"case": {"prompt_tokens": 50}}})
    )
    (base / "run" / "results" / "case.json").write_text(
        json.dumps({"instance_id": "case", "success": True})
    )
    repro = load_analysis("analyze_reproducibility")
    assert repro.read_cell(str(tmp_path), "host1", 1, "b") is None


@pytest.mark.parametrize("broken", ["mismatch", "unreadable"])
def test_pilot_ledger_refuses_unreliable_usage(tmp_path: Path, broken: str) -> None:
    sweep = tmp_path / "out" / "repeat-1"
    profile = sweep / "arm-a" / "run" / "openclaw-profiles" / "case"
    write_agent_store(profile, [(1_000_000, 0, 0)])
    store = profile / "agents" / "main" / "agent" / "openclaw-agent.sqlite"
    with sqlite3.connect(store) as connection:
        if broken == "mismatch":
            event = {
                "type": "model.completed",
                "data": {"usage": {"input": 5_000_000, "output": 0, "cacheRead": 0}},
            }
            connection.execute(
                "INSERT INTO trajectory_runtime_events VALUES (?)", (json.dumps(event),)
            )
        else:
            connection.execute("DROP TABLE transcript_events")
    source = (SCRIPTS / "run_pilot.sh").read_text()
    begin = source.index("ledger() {")
    end = source.index("\nPY\n}", begin) + len("\nPY\n}")
    result = subprocess.run(
        ["bash", "-c", source[begin:end] + "\nledger"],
        env=dict(
            os.environ,
            SWEEP=str(sweep),
            CAP="300",
            MARGIN="20",
            L4_ARMS="a",
            PY_SWE=sys.executable,
            PYTHONPATH=str(SWE_RUNNER_SRC),
        ),
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode not in (0, 3), result.stdout + result.stderr


@pytest.mark.parametrize("dry", ["0", "1"])
def test_proxy_readiness_never_sends_unmetered_completion(dry: str) -> None:
    source = (SCRIPTS / "remote_run.sh").read_text()
    begin = source.index("start_proxy() {")
    end = source.index("\nstop_proxy()", begin)
    block = source[begin:end]
    assert "/chat/completions" not in block
    assert ': > "$jsonl"' not in block


@pytest.mark.parametrize("rc", [0, 7])
def test_pilot_preserves_sweep_exit_status(tmp_path: Path, rc: int) -> None:
    source = (SCRIPTS / "run_pilot.sh").read_text()
    begin = source.index('\nwait "$SWEEP_PID"')
    ledger_stub = 'ledger() { printf "{}\\n" > "$SWEEP/pilot-cost.json"; echo 1; };\n'
    harness = (
        ledger_stub
        + f"(exit {rc}) &\nSWEEP_PID=$!\nstop_reason=completed\n"
        + source[begin:]
    )
    result = subprocess.run(
        ["bash", "-c", harness],
        env=dict(os.environ, SWEEP=str(tmp_path), PY_SWE=sys.executable),
        check=False,
        capture_output=True,
        text=True,
    )
    # 使用桩账本避免任何真实模型或主机操作。
    assert result.returncode == rc, result.stdout + result.stderr


def test_sync_never_installs_system_packages() -> None:
    code = "\n".join(code_lines("remote_sync.sh"))
    assert "dnf install" not in code and "yum install" not in code


def test_l4_default_workers_preserve_per_instance_proxy_attribution() -> None:
    source = (SCRIPTS / "remote_run.sh").read_text()
    assert "L4_WORKERS=${L4_WORKERS:-1}" in source
    assert "proxy attribution requires L4_WORKERS=1" in source


@pytest.mark.parametrize("phase", ["pre-clean", "run", "post-clean"])
def test_repeats_propagate_failure_without_starting_after_failed_cleanup(
    tmp_path: Path, phase: str
) -> None:
    script = tmp_path / "run_repeats.sh"
    script.write_text((SCRIPTS / "run_repeats.sh").read_text())
    cleaner = tmp_path / "clean_between_repeats.sh"
    cleaner.write_text("""#!/bin/bash
count=0; [ -e "$ROOT/count" ] && count=$(< "$ROOT/count")
count=$((count + 1)); echo "$count" > "$ROOT/count"
[ "$FAIL_PHASE" = pre-clean ] && [ "$count" -eq 1 ] && exit 5
[ "$FAIL_PHASE" = post-clean ] && [ "$count" -eq 2 ] && exit 5
exit 0
""")
    (tmp_path / "remote_run.sh").write_text("""#!/bin/bash
: > "$ROOT/called"
[ "$FAIL_PHASE" = run ] && exit 7
exit 0
""")
    result = subprocess.run(
        ["bash", str(script), "1"],
        env=dict(
            os.environ,
            ROOT=str(tmp_path),
            FAIL_PHASE=phase,
            DASHSCOPE_API_KEY="fixture",
        ),
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0
    assert (tmp_path / "called").exists() == (phase != "pre-clean")


def test_capture_includes_toon_and_marks_missing_recovery_as_unobservable(
    tmp_path: Path,
) -> None:
    capture = load_analysis("extract_tool_results")
    db = tmp_path / "stats.db"
    with sqlite3.connect(db) as connection:
        connection.execute(
            "CREATE TABLE stats (id INTEGER, session_id TEXT, tool_use_id TEXT, "
            "before_text TEXT, after_text TEXT, content_type TEXT, recoverability TEXT, "
            "applied_operations TEXT, before_tokens INTEGER, after_tokens INTEGER, "
            "operation TEXT, timestamp_ns INTEGER)"
        )
        connection.execute(
            "INSERT INTO stats VALUES (1, 'case', 'call-1', 'original', 'compact', "
            "'json', 'retrievable', '[\"toon_encoding\"]', 100, 10, 'compress-toon', 20)"
        )
    rows = capture.from_stats(db, 0, "b", capture.Attribution(["case"], None))
    assert len(rows) == 1
    assert rows[0]["original"] == "original"
    assert rows[0]["retrieval"] is None
    assert rows[0]["recovery_observation"] == "unobservable"
    with sqlite3.connect(db) as connection:
        connection.execute(
            "CREATE TABLE retrieve_events (hash TEXT, outcome TEXT, tool_use_id TEXT, "
            "session_id TEXT, timestamp TEXT)"
        )
        connection.execute(
            "INSERT INTO retrieve_events VALUES ('key-1', 'hit', 'retrieve-1', 'case', "
            "'2027-01-15T08:00:00+00:00')"
        )
        connection.execute(
            "INSERT INTO retrieve_events VALUES ('key-2', 'hit', 'retrieve-2', 'other', "
            "'2027-01-15T08:00:00+00:00')"
        )
    rows = capture.from_stats(db, 0, "b", capture.Attribution(["case"], None))
    assert rows[0]["recovery_observation"] == "events_only"
    assert rows[0]["recovery_events"] == [
        {"key": "key-1", "outcome": "hit", "call_id": "retrieve-1"}
    ]
    assert rows[0]["retrieval"] is None


@pytest.mark.parametrize("launched", [True, False])
def test_fleet_reports_launch_failure_to_its_caller(launched: bool) -> None:
    source = (SCRIPTS / "remote_fleet.sh").read_text()
    begin = source.index("phase_run() {")
    end = source.index("\nphase_status()", begin)
    stubs = (
        "HOSTS=(host.invalid); require_keys() { :; }; key_for() { echo fixture; }; "
        "sleep() { :; }; say() { printf '%s\\n' \"$*\"; };\n"
    )
    stubs += (
        "launch_one() { echo LAUNCHED; };\n"
        if launched
        else "launch_one() { return 1; };\n"
    )
    result = subprocess.run(
        ["bash", "-c", stubs + source[begin:end] + "\nphase_run"],
        check=False,
        capture_output=True,
        text=True,
    )
    assert (result.returncode == 0) == launched


@pytest.mark.parametrize("log_failure", [False, True])
def test_log_collection_failure_cannot_be_hidden(
    tmp_path: Path, log_failure: bool
) -> None:
    source = (SCRIPTS / "remote_fleet.sh").read_text()
    begin = source.index("phase_collect() {")
    end = source.index(
        "\n# ---------------------------------------------------------------------------",
        begin,
    )
    harness = 'HOSTS=(fixture); SSH_PW=(); SSH_OPTS=(); say() { echo "$*"; }; rsh() { return 1; }; scp() { :; };\n'
    harness += (
        'rsync() { case "$*" in *logs/*) return "$LOG_RC";; *) return 0;; esac; };\n'
    )
    result = subprocess.run(
        ["bash", "-c", harness + source[begin:end] + "\nphase_collect"],
        env=dict(
            os.environ,
            WORKSPACE=str(tmp_path),
            L4_REMOTE_WORK="fixture",
            L4_SSH_USER="fixture",
            LOG_RC="1" if log_failure else "0",
        ),
        capture_output=True,
        text=True,
    )
    assert (result.returncode != 0) is log_failure
    assert ("provenance is incomplete" in result.stdout) is log_failure


@pytest.mark.parametrize(
    "arms,workers,expected", [("a,b,c", "4", 1), ("a,b,c", "1", 0), ("a,b", "4", 0)]
)
def test_parallel_counter_attribution_is_rejected_before_any_request(
    arms: str, workers: str, expected: int
) -> None:
    source = (SCRIPTS / "remote_run.sh").read_text()
    begin = source.index('case ",$L4_ARMS," in')
    end = source.index('[ -n "${DASHSCOPE_API_KEY:-}" ]', begin)
    result = subprocess.run(
        ["bash", "-c", "die() { exit 1; };\n" + source[begin:end]],
        env=dict(os.environ, L4_ARMS=arms, L4_WORKERS=workers),
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == expected


def test_capture_maximum_one_never_divides_by_zero() -> None:
    capture = load_analysis("extract_tool_results")
    rows = [
        {"original": "first", "compressed": "a"},
        {"original": "second", "compressed": "b"},
    ]
    assert capture.thin(rows, 1, 0) == rows[:1]


@pytest.mark.parametrize("dry", ["0", "1"])
def test_dry_run_never_creates_an_experiment_success_verdict(dry: str) -> None:
    source = (SCRIPTS / "remote_run.sh").read_text()
    begin = source.index('[ "$drift_ok" -eq 1 ] || exit 1')
    result = subprocess.run(
        ["bash", "-c", 'note() { printf "%s\\n" "$*"; };\n' + source[begin:]],
        env=dict(os.environ, drift_ok="1", L4_DRY_RUN=dry, SWEEP="fixture"),
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0
    assert ("all arms produced evidence-backed results" in result.stdout) == (
        dry == "0"
    )


def test_report_shared_config_matches_the_snapshot_and_rejects_drift(
    tmp_path: Path,
) -> None:
    analysis = load_analysis()
    config_dir = SCRIPTS.parents[0] / "configs"
    shared = analysis.shared_configuration(str(config_dir))
    assert shared["model"] == "qwen3.8-max-0902"
    assert "cost" not in shared
    assert shared["provider_timeout_seconds"] == 300
    for path in config_dir.glob("arm-*.json"):
        (tmp_path / path.name).write_bytes(path.read_bytes())
    baseline = tmp_path / "arm-a-baseline.json"
    config = json.loads(baseline.read_text())
    config["models"]["providers"]["bench-qwen"]["timeoutSeconds"] = 1
    baseline.write_text(json.dumps(config))
    with pytest.raises(SystemExit, match="disagree"):
        analysis.shared_configuration(str(tmp_path))


def test_report_runtime_facts_do_not_inherit_authored_defaults(tmp_path: Path) -> None:
    analysis = load_analysis()
    summary = tmp_path / "host1" / "out" / "repeat-1" / "sweep-summary.txt"
    summary.parent.mkdir(parents=True)
    summary.write_text("[sweep] sweep repeat=1 arms=a,b\n")
    assert analysis.invocation_facts(str(tmp_path), {"host1": "host1"})["incomplete"]
    summary.write_text(
        "[sweep] runtime workers=1 timeout=30 pull_registry=registry.invalid\n"
    )
    facts = analysis.invocation_facts(str(tmp_path), {"host1": "host1"})
    assert facts["observed_by_host"]["host1"] == [
        {"workers": 1, "timeout_seconds": 30, "pull_registry": "registry.invalid"}
    ]


def test_collected_evaluation_keeps_completion_and_predictions(tmp_path: Path) -> None:
    solve = load_analysis("analyze_solve_rate")
    host = tmp_path / "host1"
    preds = host / "out" / "repeat-1" / "arm-a" / "run" / "preds.json"
    preds.parent.mkdir(parents=True)
    preds.write_text("fixture predictions")
    import hashlib

    digest = hashlib.sha256(preds.read_bytes()).hexdigest()
    evaluation = host / "eval" / "repeat-1-arm-a"
    (evaluation / "evaluate").mkdir(parents=True)
    (evaluation / ".eval-complete").write_text(f"{digest} run-1\n")
    (preds.parent / "run_metadata.json").write_text('{"instance_ids":["case"]}')
    (evaluation / "evaluate" / "model.run-1.json").write_text(
        '{"submitted_instances":1,"resolved_ids":["case"]}'
    )
    assert len(solve.load_reports(str(tmp_path), str(tmp_path))) == 1
    preds.write_text("new predictions")
    assert solve.load_reports(str(tmp_path), str(tmp_path)) == []
    source = (SCRIPTS / "remote_fleet.sh").read_text()
    assert "$L4_REMOTE_WORK/eval/" in source and '"$dest/eval/"' in source


@pytest.mark.parametrize("changed", [True, False])
def test_preflight_verifies_deployed_plugin_bundle(
    tmp_path: Path, changed: bool
) -> None:
    source = (SCRIPTS / "remote_verify.sh").read_text()
    begin = source.index("plugin_built=$(sha256sum")
    end = source.index("# RTK is linked", begin)
    built = (
        tmp_path
        / "tokenless"
        / "adapters"
        / "tokenless"
        / "openclaw"
        / "dist"
        / "index.js"
    )
    deployed = (
        tmp_path
        / "home"
        / ".openclaw"
        / "extensions"
        / "tokenless"
        / "dist"
        / "index.js"
    )
    for path in (built, deployed):
        path.parent.mkdir(parents=True)
        path.write_text("current bundle")
    if changed:
        deployed.write_text("old bundle")
    result = subprocess.run(
        ["bash", "-c", "pass() { :; }; fail() { exit 1; };\n" + source[begin:end]],
        env=dict(os.environ, WORK=str(tmp_path), HOME=str(tmp_path / "home")),
        check=False,
        capture_output=True,
        text=True,
    )
    assert (result.returncode != 0) == changed


def test_repeat_cleanup_refuses_dirty_containers_without_opt_in(tmp_path: Path) -> None:
    source = (SCRIPTS / "clean_between_repeats.sh").read_text()
    begin = source.index("cids=$(docker ps")
    end = source.index("# Record the volumes", begin)
    result = subprocess.run(
        ["bash", "-c", "docker() { echo existing-container; };\n" + source[begin:end]],
        env=dict(os.environ, W=str(tmp_path), L4_CLEAN_CONTAINERS="0"),
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0
    assert "cleanup_enabled=0" in (tmp_path / "logs" / "cleanup.provenance").read_text()


def test_l4_regressions_have_an_explicit_ci_entry() -> None:
    makefile = (TOKENLESS / "Makefile").read_text()
    workflow = (TOKENLESS.parents[1] / ".github" / "workflows" / "ci.yaml").read_text()
    assert "test-benchmark-l4:" in makefile
    assert "make -C src/tokenless test-benchmark-l4" in workflow


def test_solve_rate_counts_selected_tasks_without_predictions(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    import hashlib

    solve = load_analysis("analyze_solve_rate")
    run = tmp_path / "host1" / "out" / "repeat-1" / "arm-a" / "run"
    run.mkdir(parents=True)
    preds = run / "preds.json"
    preds.write_text('{"solved":{"instance_id":"solved","model_patch":"patch"}}')
    (run / "run_metadata.json").write_text('{"instance_ids":["solved","timeout"]}')
    evaluation = tmp_path / "host1" / "eval" / "repeat-1-arm-a"
    (evaluation / "evaluate").mkdir(parents=True)
    digest = hashlib.sha256(preds.read_bytes()).hexdigest()
    (evaluation / ".eval-complete").write_text(f"{digest} run-1\n")
    (evaluation / "evaluate" / "model.run-1.json").write_text(
        '{"submitted_instances":1,"resolved_ids":["solved"]}'
    )
    output = tmp_path / "solve.json"
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "solve",
            "--evals",
            str(tmp_path),
            "--runs",
            str(tmp_path),
            "--out",
            str(output),
        ],
    )
    solve.main()
    result = json.loads(output.read_text())["pooled_by_arm_over_repeats"]["a"]
    assert result["selected"] == 2
    assert result["solve_rate_pct"] == 50.0
    cell = json.loads(output.read_text())["per_repeat_arm"]["repeat1-arma"]
    assert "timeout" in cell["unresolved_ids"]


def test_retry_archives_the_previous_prediction_tree(tmp_path: Path) -> None:
    source = (SCRIPTS / "remote_run.sh").read_text()
    retry = source[
        source.index("run_arm() {") : source.index(
            '    note "--- arm', source.index("run_arm() {")
        )
    ]
    arm_dir = tmp_path / "out" / "repeat-1" / "arm-a"
    prior = arm_dir / "run"
    (prior / "results").mkdir(parents=True)
    (prior / "openclaw-profiles").mkdir()
    (prior / "preds.json").write_text('{"case":{"model_patch":"old-success"}}')
    (prior / "results" / "case.json").write_text('{"success":true}')
    harness = "arm_spec() { CFGNAME=fixture; };\n" + retry + "\n}; run_arm a"
    result = subprocess.run(
        ["bash", "-c", harness],
        capture_output=True,
        text=True,
        env=dict(
            os.environ,
            SWEEP=str(arm_dir.parent),
            W=str(tmp_path),
            LOG=str(tmp_path),
            CFGDIR=str(tmp_path),
            L4_REPEAT="1",
            INSTANCE_IDS="case",
            L4_DRY_RUN="0",
        ),
    )
    assert result.returncode == 0, result.stdout + result.stderr
    archives = list((arm_dir / "previous-attempts").iterdir())
    assert len(archives) == 1
    assert "old-success" in (archives[0] / "preds.json").read_text()
    assert (archives[0] / "results" / "case.json").exists()
    assert (archives[0] / "openclaw-profiles").exists()
    (prior / "results").mkdir(parents=True)
    (prior / "results" / "case.json").write_text(
        '{"success":false,"error":"preparation failed"}'
    )
    assert not (prior / "preds.json").exists()
    assert not (prior / "openclaw-profiles").exists()
    assert (
        tmp_path / "eval" / "repeat-1-arm-a" / ".eval-complete"
    ).read_text().strip() == "pending"
    assert (
        json.loads((arm_dir / "selection.json").read_text())["attempt_complete"]
        is False
    )


@pytest.mark.parametrize("signal_name", ["SIGTERM", "SIGINT"])
def test_cancelled_pilot_reaps_its_detached_sweep(
    tmp_path: Path, signal_name: str
) -> None:
    import signal
    import time

    source = (SCRIPTS / "run_pilot.sh").read_text()
    block = source[source.index('echo "[pilot] $(date -u +%FT%TZ) arms=') :]
    setup = 'ledger() { echo 0; printf "{}" > "$SWEEP/pilot-cost.json"; };\n'
    setup += 'setsid() { exec "$PY_SWE" -c \'import os,sys; os.setsid(); os.execvp(sys.argv[1],sys.argv[1:])\' "$@"; };\n'
    stub = tmp_path / "remote_run.sh"
    stub.write_text("""#!/bin/bash
trap 'exit 0' TERM INT
sleep 60 & child=$!
echo "$BASHPID $child" > "$HERE/pids"
wait "$child"
""")
    (tmp_path / "logs").mkdir()
    instance_list = tmp_path / "instances"
    instance_list.write_text("case\n")
    process = subprocess.Popen(
        ["bash", "-c", setup + block],
        env=dict(
            os.environ,
            HERE=str(tmp_path),
            W=str(tmp_path),
            SWEEP=str(tmp_path),
            POLL="1",
            CAP="100",
            MARGIN="1",
            L4_ARMS="a",
            L4_INSTANCE_LIST=str(instance_list),
            PY_SWE=sys.executable,
            L4_STOP_GRACE_SECS="0.1",
        ),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    pids = []
    try:
        deadline = time.monotonic() + 5
        while not (tmp_path / "pids").exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        assert (tmp_path / "pids").exists()
        pids = [int(value) for value in (tmp_path / "pids").read_text().split()]
        process.send_signal(getattr(signal, signal_name))
        assert process.wait(timeout=5) != 0
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            try:
                os.killpg(pids[0], 0)
            except ProcessLookupError:
                break
            time.sleep(0.05)
        else:
            pytest.fail("sweep process group survived pilot cancellation")
    finally:
        if process.poll() is None:
            process.kill()
        if pids:
            try:
                os.killpg(pids[0], signal.SIGKILL)
            except ProcessLookupError:
                pass


@pytest.mark.parametrize(
    "finished,predictions,expected",
    [
        (True, None, 0.0),
        (True, {}, 0.0),
        (False, None, None),
        (True, {"case": {}}, None),
    ],
)
def test_zero_prediction_and_unscored_cells_do_not_disappear(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    finished: bool,
    predictions: object,
    expected: object,
) -> None:
    solve = load_analysis("analyze_solve_rate")
    arm = tmp_path / "host1" / "out" / "repeat-1" / "arm-a"
    (arm / "run").mkdir(parents=True)
    (arm / "selection.json").write_text(
        json.dumps({"instance_ids": ["case", "timeout"], "attempt_complete": finished})
    )
    if predictions is not None:
        (arm / "run" / "preds.json").write_text(json.dumps(predictions))
    output = tmp_path / "solve.json"
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "solve",
            "--evals",
            str(tmp_path),
            "--runs",
            str(tmp_path),
            "--out",
            str(output),
        ],
    )
    solve.main()
    doc = json.loads(output.read_text())
    assert doc["pooled_by_arm_over_repeats"]["a"]["selected"] == 2
    assert doc["pooled_by_arm_over_repeats"]["a"]["solve_rate_pct"] == expected
    cell = doc["per_repeat_arm"]["repeat1-arma"]
    assert cell["hosts_attempted"] == 1 and cell["hosts_scored"] == 0
    assert cell["unscored_ids"] == (["case", "timeout"] if expected is None else [])


@pytest.mark.parametrize("phase", ["missing", "prior_repeat", "started", "completed"])
def test_cleanup_provenance_is_bound_to_the_current_repeat(
    tmp_path: Path, phase: str
) -> None:
    analysis = load_analysis()
    analysis.REPEAT = 2
    log = tmp_path / "host1" / "logs" / "cleanup.provenance"
    log.parent.mkdir(parents=True)
    if phase != "missing":
        repeat = 1 if phase == "prior_repeat" else 2
        outcome = "started" if phase == "started" else "completed"
        log.write_text(
            f"fixture repeat={repeat} phase=pre outcome=completed\nfixture repeat={repeat} phase=post outcome={outcome}\n"
        )
    prov = analysis.provenance(str(tmp_path), {"host1": "host1"}, {})
    assert prov["cleanup_incomplete"] is (phase != "completed")


def test_config_snapshot_requires_all_executed_arms_and_marks_authored_fields(
    tmp_path: Path,
) -> None:
    analysis = load_analysis()
    analysis.ARMS = ["a", "b"]
    configs = SCRIPTS.parent / "configs"
    baseline = tmp_path / "arm-a-baseline.json"
    baseline.write_bytes((configs / baseline.name).read_bytes())
    with pytest.raises(SystemExit, match="missing executed arms"):
        analysis.shared_configuration(str(tmp_path))
    other = tmp_path / "arm-b-tokenless.json"
    other.write_bytes((configs / other.name).read_bytes())
    for path in (baseline, other):
        doc = json.loads(path.read_text())
        doc["models"]["providers"]["bench-qwen"]["models"][0].pop("contextWindow", None)
        path.write_text(json.dumps(doc))
    shared = analysis.shared_configuration(str(tmp_path))
    assert shared["context_window"] is None
    assert shared["context_window_authored"] is False


def test_disclosure_audit_uses_the_cached_runner_dataset(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    audit = load_analysis("audit_prompt_disclosure")
    registry_spec = importlib.util.spec_from_file_location(
        "dataset_registry_test",
        SWE_RUNNER_SRC / "swe_runner/common/dataset_registry.py",
    )
    registry = importlib.util.module_from_spec(registry_spec)
    registry_spec.loader.exec_module(registry)
    dataset_stub = ModuleType("datasets")
    requested = []

    def load_dataset(name: str, split: str):
        assert name == registry.get_dataset_name("lite")
        requested.append(name)
        return [
            {
                "instance_id": "case",
                "patch": "diff --git a/file.py b/file.py\n+++ b/file.py\n@@ -0,0 +1 @@\n+fixed",
            }
        ]

    dataset_stub.load_dataset = load_dataset
    monkeypatch.setitem(sys.modules, "datasets", dataset_stub)
    (tmp_path / "prompts").mkdir()
    (tmp_path / "prompts" / "case").write_text("file.py fixed")
    (tmp_path / "instances").write_text("case\n")
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "audit",
            "--instances",
            str(tmp_path / "instances"),
            "--prompts",
            str(tmp_path / "prompts"),
        ],
    )
    audit.main()
    assert requested == ["SWE-bench/SWE-bench_Lite"]
