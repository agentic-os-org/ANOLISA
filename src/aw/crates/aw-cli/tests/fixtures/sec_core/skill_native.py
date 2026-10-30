"""Real Ledger and stock framework acceptance through installed AW entrypoints."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "qwenpaw"))
from agents import configure_qwenpaw
from native import configuration
from native_app_config import ready
from runtime import Processes, await_condition, save
from skill_backend import SkillBackend
from skill_qwen import turn

CASES = ("unsigned", "signed", "drifted", "tampered", "shadow", "ambiguous", "backend-denied", "unavailable", "timeout")
QODER_CASES = ("case-shadow", "project-only", "workspace-enabled", "empty-shadow", "model-only", "workflow-case", "workflow-empty", "dynamic-read")
QWEN_CASES = ("bom",)
ALLOWED = ("signed", "workspace-enabled", "model-only")


def fixtures(root, backend, agent):
    base = root / ("work/.qoder/skills" if agent == "qoder-sdk" else "work/skills")
    names = {}
    for case in CASES + (QODER_CASES if agent == "qoder-sdk" else QWEN_CASES):
        directory = base / case
        if case == "case-shadow":
            directory = base / "Case-shadow"
        elif case == "workspace-enabled":
            directory = root / "work/.agents/skills" / case
        elif case.startswith("workflow-") or case == "dynamic-read":
            directory = root / "config/skills" / case
        directory.mkdir(parents=True)
        (directory / "SKILL.md").write_text(
            f"---\nname: {case}\ndescription: Synthetic Skill acceptance.\n---\nReply ONLY SKILL_BODY_{case.upper().replace('-', '_')}.\n")
        if case == "model-only":
            manifest = directory / "SKILL.md"
            manifest.write_text(manifest.read_text().replace("description:", "user-invocable: false\ndescription:"))
        if case == "workflow-empty":
            (directory / "SKILL.md").write_text("---\n\n---\n")
        if case == "bom":
            manifest = directory / "SKILL.md"
            manifest.write_bytes(b"\xef\xbb\xbf" + manifest.read_bytes())
        names[case] = directory
    # Qoder refuses competing local assets, including native-unloadable shadows.
    # QwenPaw retains its native Toolkit's actual duplicate-object choice.
    if agent == "qoder-sdk":
        shadow = root / "config/skills/shadow"
        shadow.mkdir(parents=True)
        (shadow / "SKILL.md").write_text("---\nname: shadow\ndescription: Unsigned user Skill.\n---\nReply SHADOW_USER_INSTRUCTIONS.\n")
        duplicate = base / "nested/ambiguous"
        duplicate.mkdir(parents=True)
        shutil.copy(names["ambiguous"] / "SKILL.md", duplicate)
        for case in ("case-shadow", "project-only"):
            user = root / "config/skills" / case
            user.mkdir(parents=True)
            (user / "SKILL.md").write_text(
                f"---\nname: {case}\ndescription: Signed user Skill.\n---\nSIGNED_USER_INSTRUCTIONS.\n")
        empty = root / "config/skills/empty-shadow"
        empty.mkdir(parents=True)
        (empty / "SKILL.md").write_text("---\n\n---\n")
        save(root / "work/.qoder/settings.json", {"skills":{"loadFromAgentsDirectory":True}})
    # A readable source outside the daemon's authorized roots is a backend
    # error. It must never be recorded as an unsigned verdict.
    outside = root / "work/backend-denied-source"
    names["backend-denied"].rename(outside)
    names["backend-denied"].symlink_to(outside, target_is_directory=True)
    init = backend.cli("skill-ledger", "init", "--no-baseline")
    assert init["exit_code"] == 0, init
    findings = root / "findings.json"
    save(findings, [])
    for case in ("signed", "drifted", "tampered", "shadow", "unavailable", "timeout") + (QWEN_CASES if agent == "qwenpaw" else ()):
        signed = backend.cli("skill-ledger", "certify", names[case], "--findings", findings)
        assert signed["exit_code"] == 0, signed
    if agent == "qoder-sdk":
        for directory in (names["workspace-enabled"], names["model-only"], root / "config/skills/case-shadow",
                root / "config/skills/project-only", root / "config/skills/empty-shadow",
                names["workflow-case"], names["workflow-empty"], names["dynamic-read"]):
            signed = backend.cli("skill-ledger", "certify", directory, "--findings", findings)
            assert signed["exit_code"] == 0, signed
        empty_check = backend.cli("skill-ledger", "check", root / "config/skills/empty-shadow")
        assert json.loads(empty_check["stdout"])["status"] == "pass", empty_check
        save(backend.output / "empty-shadow-user-check.json", empty_check)
    (names["drifted"] / "SKILL.md").write_text((names["drifted"] / "SKILL.md").read_text() + "\nCHANGED_AFTER_SIGNING\n")
    backend.tamper(names["tampered"])
    if agent == "qwenpaw":
        enabled = list(names)
        for case in ("shadow", "ambiguous"):
            duplicate = base / ("zz-" + case)
            duplicate.mkdir()
            (duplicate / "SKILL.md").write_text(
                f"---\nname: {case}\ndescription: Unsigned duplicate.\n---\nReply DUPLICATE_UNCHECKED_INSTRUCTIONS.\n")
            enabled.append(duplicate.name)
        save(root / "work/skill.json", {"skills":{name:{"enabled":True,"channels":["all"]} for name in enabled}})
    return names


def provider_calls(state):
    return [json.loads(line)["record"] for path in (state / "journal").glob("*.jsonl")
        for line in path.read_text().splitlines()
        if json.loads(line)["record"].get("kind") == "provider"
        and json.loads(line)["record"].get("call", {}).get("step_id") == "check-skill"]


def policy(args, root, argv, socket):
    config = configuration(args.agent, argv, args.bin, root / "state", socket)
    config["spec"]["providers"]["security"]["config"]["skill"] = True
    config["spec"]["providers"]["security"]["config"]["tools"]["bash"] = {"language":"bash","input_pointer":"/command"}
    config["spec"]["providers"]["security"]["timeout_ms"] = 1500
    config["spec"]["events"]["tool.before"]["steps"] = [{"id":"check-skill", "provider":"security",
        "operation":"check_skill", "effects":["observe","block"], "on_error":"block",
        "match":{"tools":[f"native:{args.agent}:Skill"]}}]
    # Keep code scanning in the same policy; only its tool selectors differ.
    tool = "Bash" if args.agent == "qoder-sdk" else "execute_shell_command"
    config["spec"]["events"]["tool.before"]["steps"].append({"id":"scan-code", "provider":"security",
        "operation":"scan_code", "effects":["observe","block"], "on_error":"block",
        "match":{"tools":[f"native:{args.agent}:{tool}"]}})
    return config


def run(args):
    args.output.mkdir(parents=True, exist_ok=True)
    reports = []
    with tempfile.TemporaryDirectory(prefix="aw-skill-") as temporary:
        root = Path(temporary)
        for path in ("work", "home", "config", "state"):
            (root / path).mkdir(mode=0o700)
        environment = {key:value for key,value in os.environ.items()
            if not key.startswith(("AW_", "QODER_", "QODERCN_", "QWENPAW_", "AGENT_SEC_"))}
        # Loopback model traffic uses the stock native client without an HTTP proxy.
        environment = {key:value for key,value in environment.items() if not key.lower().endswith("_proxy")}
        environment.update(HOME=str(root / "home"), QODER_CONFIG_DIR=str(root / "config"))
        if args.agent == "qoder-sdk":
            environment["QODER_PERSONAL_ACCESS_TOKEN"] = os.environ["QODER_PERSONAL_ACCESS_TOKEN"]
        processes = Processes(root, args.output, environment)
        backend = SkillBackend(root, args.output, args.bin, args.image)
        try:
            backend.start()
            names = fixtures(root, backend, args.agent)
            checks = {case:backend.cli("skill-ledger", "check", directory.resolve()) for case,directory in names.items()}
            save(args.output / "backend-checks.json", checks)
            if args.agent == "qwenpaw":
                from qwenpaw.agents.skill_system.runtime_cache import RuntimeSkillCache
                assert json.loads(checks["bom"]["stdout"])["status"] == "pass", checks["bom"]
                # The stock App cache differs from AgentScope's BOM-aware loader.
                assert RuntimeSkillCache().load([names["bom"]]) == []
                save(args.output / "bom-native-cache.json", {"skipped":True})
            for case,status in [("unsigned","none"), ("signed","pass"), ("drifted","drifted"), ("tampered","tampered")]:
                assert json.loads(checks[case]["stdout"])["status"] == status, checks[case]
            assert checks["backend-denied"]["exit_code"] != 0
            if args.agent == "qoder-sdk":
                for case in ("workflow-case", "workflow-empty", "dynamic-read"):
                    assert json.loads(checks[case]["stdout"])["status"] == "pass", checks[case]
            if args.agent == "qoder-sdk":
                modules = args.npm / "node_modules"
                (root / "work/node_modules").symlink_to(modules, target_is_directory=True)
                shutil.copy(HERE / "skill_qoder.mjs", root / "work/app.mjs")
                argv = [shutil.which("node"), str(root / "work/app.mjs")]
                ports = []
            else:
                argv, ports = configure_qwenpaw(root, args.agent_bin, environment, processes)
                baseline = processes.start("native-baseline", argv, ports[:1])
                await_condition(lambda:ready(ports[0]), 45, "native QwenPaw initialization")
                processes.stop(baseline)
            native_config = (root / ("config/settings.json" if args.agent == "qoder-sdk" else "home/config.json"))
            if args.agent == "qoder-sdk":
                save(native_config, {"permissions":{"deny":["Bash(rm *)"]}})
            originals = {native_config:native_config.read_bytes()}
            if args.agent == "qoder-sdk":
                project_config = root / "work/.qoder/settings.json"
                originals[project_config] = project_config.read_bytes()
            cases = CASES + (QODER_CASES if args.agent == "qoder-sdk" else QWEN_CASES)
            for index,case in enumerate(cases):
                # Each case gets a new policy state and stock process. This tests
                # admission and native initialization with the exact failure mode.
                state = root / ("s" + str(index))
                target_socket = root / "absent.sock" if case == "unavailable" else backend.socket
                config = policy(args, root, argv, target_socket)
                config["spec"]["daemon"]["state_dir"] = str(state)
                save(root / "aw.json", config)
                save(root / "work/case.json", {"skill":case})
                start = len(processes.items)
                workflow = root / "work/.qoder/workflows/unrelated-filename.js"
                try:
                    if case == "dynamic-read":
                        dynamic = root / "work/nested/.qoder/skills/dynamic-read"
                        dynamic.mkdir(parents=True)
                        (dynamic / "SKILL.md").write_text("---\ndescription: DYNAMIC_CACHED_SKILL\n---\nUNCHECKED_DYNAMIC_INSTRUCTIONS\n")
                        (root / "work/nested/readme.txt").write_text("ordinary nested file\n")
                    if case.startswith("workflow-"):
                        workflow.parent.mkdir(parents=True, exist_ok=True)
                        name = "Workflow-case" if case == "workflow-case" else case
                        workflow.write_text(f'export const meta = {{name: "{name}", description: "UNCHECKED_WORKFLOW_INSTRUCTIONS"}}; return {{}};')
                    service = processes.start("service-" + case, [args.bin / "aw", "serve", "--config", root / "aw.json", "--state-dir", state])
                    await_condition(lambda:(state / "aw.sock").exists() or processes.exited(service), 10, "AW service")
                    if case == "timeout":
                        backend.pause()
                    child = processes.start("native-" + case, [args.bin / "aw", "run", "--config", root / "aw.json", "--agent", args.agent], ports[:1])
                    if args.agent == "qoder-sdk":
                        assert processes.wait(child, 120) == 0, "Qoder SDK driver failed"
                        messages = json.loads((root / "work/messages.json").read_text())
                        save(args.output / (case + "-messages.json"), messages)
                        bodies = [message for message in messages if message.get("type") == "user"]
                        delivered = json.dumps(bodies)
                        if case == "model-only":
                            catalog = json.loads((root / "work/skill-catalog.json").read_text())
                            assert all(skill["name"] != case for skill in catalog["skills"]), catalog
                            save(args.output / "model-only-catalog.json", catalog)
                        if case.startswith("workflow-"):
                            catalog = json.loads((root / "work/skill-catalog.json").read_text())
                            assert any(skill["name"] == name and skill["description"] == "UNCHECKED_WORKFLOW_INSTRUCTIONS"
                                for skill in catalog["skills"]), catalog
                            save(args.output / (case + "-catalog.json"), catalog)
                        if case == "dynamic-read":
                            catalogs = json.loads((root / "work/skill-catalog.json").read_text())
                            assert len(catalogs) == 2, catalogs
                            for catalog in catalogs:
                                assert any(skill["name"] == case and skill["description"] == "DYNAMIC_CACHED_SKILL"
                                    for skill in catalog["skills"]), catalog
                            assert not (dynamic / "SKILL.md").exists(), "cached source was not removed"
                            assert any(message.get("type") == "assistant" and any(block.get("name") == "Read"
                                for block in message.get("message", {}).get("content", [])) for message in messages)
                            save(args.output / "dynamic-read-catalogs.json", catalogs)
                        assert any(message.get("type") == "assistant" and any(
                            block.get("type") == "tool_use" and block.get("name") == "Skill"
                            for block in message.get("message", {}).get("content", [])) for message in messages), "native Skill was never called"
                    else:
                        await_condition(lambda:ready(ports[0]), 45, "QwenPaw ready")
                        result, requests = turn(root, args.output, ports[0], case)
                        delivered = json.dumps(requests)
                        assert result["state"] == ("success" if case in ALLOWED else "denied"), result
                    calls = provider_calls(state)
                    assert len(calls) == 1, calls
                    checked = calls[0]
                    error = case in ("backend-denied", "unavailable", "timeout", "bom") or (case in ("shadow", "ambiguous", "case-shadow", "empty-shadow", "workflow-case", "workflow-empty", "dynamic-read") and args.agent == "qoder-sdk")
                    assert checked["status"] == ("error" if error else "ok"), checked
                    assert checked["policy_block"] == (not error and case not in ALLOWED), checked
                    if error:
                        assert checked["failure_action"] == "block", checked
                    if case == "timeout":
                        assert checked["call"]["elapsed_ms"] >= 1000, checked
                    marker = "SKILL_BODY_" + case.upper().replace("-", "_")
                    assert (marker in delivered) == (case in ALLOWED), "unexpected Skill instruction delivery"
                    assert "SHADOW_USER_INSTRUCTIONS" not in delivered
                    assert "DUPLICATE_UNCHECKED_INSTRUCTIONS" not in delivered
                    assert "SIGNED_USER_INSTRUCTIONS" not in delivered
                    assert "UNCHECKED_WORKFLOW_INSTRUCTIONS" not in delivered
                    assert "UNCHECKED_DYNAMIC_INSTRUCTIONS" not in delivered
                    assert all(path.read_bytes() == original for path,original in originals.items()), "native configuration changed"
                    report = {"case":case, "passed":True, "native_skill_called":True,
                        "instructions_delivered":case in ALLOWED, "provider_status":checked["status"],
                        "policy_block":checked["policy_block"], "config_preserved":True}
                    reports.append(report)
                    save(args.output / "results.json", {"passed":False,"cases":reports})
                finally:
                    backend.resume()
                    for proc, _, _ in reversed(processes.items[start:]):
                        if proc:
                            processes.stop(proc)
                    workflow.unlink(missing_ok=True)
                    if (state / "journal").exists():
                        shutil.copytree(state / "journal", args.output / (case + "-journal"))
            save(args.output / "versions.json", {"agent":args.agent, "artifacts":{
                name:hashlib.sha256((args.bin / name).read_bytes()).hexdigest()
                for name in ("aw","aw-provider-sec-core","agent-sec-cli","agent-sec-daemon")}})
        finally:
            try:
                processes.close()
            finally:
                backend.close()
    save(args.output / "results.json", {"passed":True,"cases":reports})


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--agent", choices=("qoder-sdk","qwenpaw"), required=True)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--npm", type=Path)
    parser.add_argument("--agent-bin", type=Path)
    args = parser.parse_args()
    args.bin, args.output = args.bin.resolve(), args.output.resolve()
    if args.agent == "qoder-sdk":
        if not args.npm or not os.environ.get("QODER_PERSONAL_ACCESS_TOKEN"):
            parser.error("Qoder requires --npm and QODER_PERSONAL_ACCESS_TOKEN")
        args.npm = args.npm.resolve()
    elif not args.agent_bin:
        parser.error("QwenPaw requires --agent-bin")
    try:
        run(args)
    except BaseException as error:
        args.output.mkdir(parents=True, exist_ok=True)
        previous = args.output / "results.json"
        result = json.loads(previous.read_text()) if previous.exists() else {"cases":[]}
        result.update(passed=False, error=type(error).__name__)
        save(previous, result)
        raise


if __name__ == "__main__":
    main()
