"""Fresh six-Skill CLI acceptance; plan before optional RPM installation, run after it."""

import argparse
import concurrent.futures
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

SKILLS = (
    "ai-application-evaluation-planner",
    "manage-firewalld-rules",
    "pwt-test",
    "agentseccore-code-scanner",
    "anolis-security-baseline-hardener",
    "project-analyzer",
)


def read(path):
    return json.loads(path.read_text()) if path.is_file() else {}


def write(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def assess(name, snapshot, output, stage, python_module=False):
    root = output / name
    root.mkdir(parents=True, exist_ok=True)
    result = root / ("task.json" if stage == "plan" else "report.json")
    if result.exists() or (root / f"{stage}-status.json").exists():
        raise ValueError(f"refusing to reuse prior {stage} result: {root}")
    command = (
        ["agent-sec-cli"]
        if not python_module and shutil.which("agent-sec-cli")
        else [sys.executable, "-m", "agent_sec_cli.cli"]
    )
    check = subprocess.run(
        command + ["skill-score", stage, "--help"],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if check.returncode:
        raise RuntimeError(
            f"assessment harness selected an incompatible CLI: {command}; skill-score {stage} unavailable"
        )
    command += ["skill-score", stage]
    command += [
        str(snapshot / "skills" / name) if stage == "plan" else str(root / "task.json")
    ]
    command += ["--output-file", str(result)]
    if stage == "run":
        command += ["--evidence-dir", str(root / "evidence")]
    started = time.monotonic()
    with (
        (root / f"{stage}.stdout").open("x") as stdout,
        (root / f"{stage}.stderr").open("x") as stderr,
    ):
        process = subprocess.Popen(command, stdout=stdout, stderr=stderr)
        try:
            code = process.wait(timeout=3300)
        except subprocess.TimeoutExpired:
            process.send_signal(signal.SIGTERM)
            try:
                process.wait(timeout=60)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            code = 1
    data = read(result)
    raw = (root / f"{stage}.stdout").read_text()
    valid_stdout = bool(data) and json.loads(raw) == data
    calls = re.findall(
        r"\[skill-score\] Judge \w+ attempt=\d+ tokens=(\d+) seconds=([\d.]+)",
        (root / f"{stage}.stderr").read_text(),
    )
    status = {
        "exit_code": code,
        "elapsed_seconds": round(time.monotonic() - started, 3),
        "stdout_matches_json": valid_stdout,
        "judge_responses": len(calls),
        "judge_tokens": sum(int(tokens) for tokens, _ in calls),
    }
    if stage == "run" and data:
        expected = [p["id"] for p in read(root / "task.json")["audit"]["probes"]]
        status["all_tasks_reported"] = expected == [
            e["task_id"] for e in data["execution"]
        ]
    write(root / f"{stage}-status.json", status)
    print(
        f"{name}: {stage} exit={code}, decision={data.get('decision', '-')}", flush=True
    )


def summarize(output, names=SKILLS):
    rows = []
    for name in names:
        root = output / name
        report = read(root / "report.json")
        internal = read(root / "evidence/internal-report.json")
        row = {
            "skill": name,
            "decision": report.get("decision", "not_completed"),
            "score": report.get("score"),
            "reason": report.get("reason")
            or "\n".join(
                (root / f"{stage}.stderr").read_text()[-1600:]
                for stage in ("plan", "run")
                if (root / f"{stage}.stderr").is_file()
            ),
            "plan": read(root / "plan-status.json"),
            "run": read(root / "run-status.json"),
            "usage": internal.get("usage", {}),
            "corrections": internal.get("corrections", []),
            "dynamic_diagnostic": internal.get("dynamic_diagnostic", {}),
            "limitations": report.get("limitations", []),
            "control_gaps": report.get("control_gaps", []),
            "risk_score": internal.get("risk_score"),
            "hygiene_penalty": internal.get("hygiene", {}).get("penalty"),
        }
        rows.append(row)
        if not report:
            (root / "report.md").write_text(
                f"# {name}\n\nNo assessment score was produced.\n\n{row['reason']}\n"
            )
        if report:
            lines = [
                f"# {name}",
                "",
                f"{row['decision']} · {row['score']}",
                "",
                row["reason"],
                "",
                "## Findings",
                "",
            ]
            lines += [
                f"- {f['id']}: {f['severity']} / {f['status']} / {f['dynamic']}, deduction {f['deduction']}. {f['reason']}\n  "
                + "\n  ".join(f["evidence"])
                for f in report["findings"]
            ]
            lines += ["", "## Tasks", ""]
            lines += [
                f"- {gap['rule_id']}: {gap['status']}, deduction {gap['deduction']}. {gap['reason']}"
                for gap in report.get("control_gaps", [])
            ]
            lines += [f"- Limitation: {note}" for note in report.get("limitations", [])]
            lines += [
                f"- {e['task_id']}: completed={e['completed']}. {e['reason']}"
                for e in report["execution"]
            ]
            (root / "report.md").write_text("\n".join(lines) + "\n")
    ranking = sorted(
        (r for r in rows if r["decision"] == "scored"),
        key=lambda r: (-r["score"], r["skill"]),
    )
    write(
        output / "ranking.json",
        [
            {"rank": i + 1, "skill": r["skill"], "score": r["score"]}
            for i, r in enumerate(ranking)
        ],
    )
    write(output / "summary.json", rows)
    lines = [
        "# Six-Skill CLI acceptance",
        "",
        "Fresh plan/run processes; no prior scoring cache. Controlled script results are separate from Agent execution.",
        "",
        "| Skill | Decision | Score | Plan/run exit | Runtime attempts |",
        "|---|---|---:|---|---:|",
    ]
    lines += [
        f"| {r['skill']} | {r['decision']} | {r['score']} | {r['plan'].get('exit_code')} / {r['run'].get('exit_code')} | {r['usage'].get('runtime_attempts', 0)} |"
        for r in rows
    ]
    for r in rows:
        lines += [
            "",
            f"## {r['skill']}",
            "",
            r["reason"],
            "",
            "```json",
            json.dumps(
                {
                    k: r[k]
                    for k in (
                        "plan",
                        "run",
                        "usage",
                        "corrections",
                        "dynamic_diagnostic",
                    )
                },
                ensure_ascii=False,
                indent=2,
            ),
            "```",
        ]
    (output / "review.md").write_text("\n".join(lines) + "\n")
    key = os.environ.get("ANTHROPIC_API_KEY", "")
    if key:
        for path in output.rglob("*"):
            if path.is_file() and not path.is_symlink():
                if key.encode() in path.read_bytes():
                    raise ValueError(f"configured credential found in {path}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("snapshot", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--stage", choices=("plan", "run"), required=True)
    parser.add_argument("--skills", nargs="+", default=SKILLS)
    parser.add_argument(
        "--python-module",
        action="store_true",
        help="Use the installed RPM's Python entry point without replacing the host CLI",
    )
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        jobs = [
            pool.submit(
                assess,
                name,
                args.snapshot.resolve(),
                args.output.resolve(),
                args.stage,
                args.python_module,
            )
            for name in args.skills
        ]
        for job in concurrent.futures.as_completed(jobs):
            job.result()
    if args.stage == "run":
        summarize(args.output, args.skills)
