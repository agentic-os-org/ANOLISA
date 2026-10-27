"""ATIF interactions and syscall facts remain separate, linked by probe ID."""

import json
import posixpath
import re
import shlex
import tempfile
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_score.contracts import ContractError, Fixture, Probe
from agent_sec_cli.skill_score.evidence.redact import redact_text, redact_value
from agent_sec_skill_score_runtime.files import read_file, snapshot_tree
from harbor.models.trajectories import Trajectory

_QUOTED = re.compile(r'"((?:[^"\\]|\\.)*)"')
_CALL = re.compile(r"^(\d+\.\d+)\s+(\w+)\((.*)\)\s+=\s+(.+)$")


def decode(value: str) -> str:
    try:
        return json.loads('"' + value + '"')
    except ValueError:
        return value


def parse_traces(
    directory: Path,
    probe: Probe,
    errors: list[str] | None = None,
    known_links: set[str] | None = None,
) -> list[dict[str, Any]]:
    events = []
    parents: dict[int, int] = {}
    pending: dict[tuple[int, str], str] = {}
    for path in sorted(directory.glob("trace.*")):
        if not path.suffix[1:].isdigit():
            continue
        pid = int(path.suffix[1:])
        try:
            lines = path.read_text(errors="replace").splitlines()
        except OSError as exc:
            if errors is not None:
                errors.append(f"{path.name}: {type(exc).__name__}")
            continue
        for line in lines:
            exited = re.match(r"^(\d+\.\d+)\s+\+\+\+ exited with (\d+) \+\+\+$", line)
            if exited:
                events.append(
                    {
                        "time": float(exited[1]),
                        "pid": pid,
                        "kind": "process_exit",
                        "exit_code": int(exited[2]),
                        "outcome": "failed" if int(exited[2]) else "succeeded",
                    }
                )
                continue
            if "<unfinished ...>" in line:
                call = re.search(r" (\w+)\(", line)
                if call:
                    pending[(pid, call[1])] = line.split("<unfinished ...>")[0]
                continue
            resumed = re.search(r"<\.\.\. (\w+) resumed>(.*)", line)
            if resumed:
                line = pending.pop((pid, resumed[1]), "") + resumed[2]
            match = _CALL.match(line)
            if not match:
                if errors is not None and ("<..." in line or "execve(" in line):
                    errors.append(f"{path.name}: incomplete syscall record")
                continue
            timestamp, syscall, args, result = match.groups()
            strings = [decode(value) for value in _QUOTED.findall(args)]
            event = {
                "time": float(timestamp),
                "pid": pid,
                "syscall": syscall,
                "args": redact_text(args),
                "result": result,
                "outcome": (
                    "failed"
                    if result.startswith("-1 ")
                    else "succeeded" if re.match(r"\d", result) else "unknown"
                ),
            }
            if (
                syscall in {"clone", "clone3", "fork", "vfork"}
                and result.split()[0].isdigit()
            ):
                parents[int(result.split()[0])] = pid
            if syscall in {"execve", "execveat"}:
                event.update(
                    kind="exec",
                    target=strings[0] if strings else "",
                    argv=strings[1:],
                )
            elif syscall in {"connect", "sendto", "sendmsg"}:
                event.update(kind="network", target=args)
            elif strings and syscall in {
                "open",
                "openat",
                "openat2",
                "unlink",
                "unlinkat",
                "rename",
                "renameat",
                "renameat2",
                "truncate",
                "chmod",
                "fchmodat",
                "chown",
                "fchownat",
                "mkdir",
                "mkdirat",
                "rmdir",
                "symlink",
                "symlinkat",
                "link",
                "linkat",
                "chdir",
                "stat",
                "lstat",
                "newfstatat",
                "readlink",
                "access",
            }:
                target = (
                    strings[-1]
                    if syscall
                    in {
                        "rename",
                        "renameat",
                        "renameat2",
                        "symlink",
                        "symlinkat",
                        "link",
                        "linkat",
                    }
                    else strings[0]
                )
                directories = re.findall(r"(?:AT_FDCWD|\d+)<(/[^>]+)>", args)
                if target and not target.startswith("/") and directories:
                    directory = (
                        directories[-1]
                        if syscall in {"renameat", "renameat2", "linkat"}
                        else directories[0]
                    )
                    target = posixpath.join(directory, target)
                event.update(
                    kind="file",
                    target=target,
                    write=(
                        syscall
                        in {
                            "unlink",
                            "unlinkat",
                            "rename",
                            "renameat",
                            "renameat2",
                            "truncate",
                            "chmod",
                            "fchmodat",
                            "chown",
                            "fchownat",
                            "mkdir",
                            "mkdirat",
                            "rmdir",
                            "symlink",
                            "symlinkat",
                            "link",
                            "linkat",
                        }
                        or any(
                            flag in args
                            for flag in (
                                "O_WRONLY",
                                "O_RDWR",
                                "O_TRUNC",
                                "O_CREAT",
                            )
                        )
                    ),
                )
            else:
                continue
            events.append(event)
    links = (known_links or set()) | {
        e["target"]
        for e in events
        if e.get("syscall") in {"symlink", "symlinkat"}
        and e.get("outcome") == "succeeded"
    }
    for event in events:
        event["ppid"] = parents.get(event["pid"])
        target = event.get("target", "")
        if event.get("kind") == "file" and target.startswith("/"):
            parts = target.split("/")
            if any(
                posixpath.normpath("/".join(parts[:i])) in links
                for i in range(2, len(parts))
            ):
                # Lexically collapsing '..' across a symlink can name the wrong resource.
                event["path_resolution"] = "unknown_symlink_prefix"
            else:
                normalized = posixpath.normpath(target)
                if target != normalized:
                    event["requested_target"] = target
                    event["target"] = normalized
    if pending and errors is not None:
        errors.append(f"{len(pending)} unfinished syscall records")
    return sorted(events, key=lambda event: event["time"])


def read_atif(path: Path, probe_id: str, label: str = "harbor") -> dict[str, str]:
    value = json.loads(path.read_text())
    Trajectory.model_validate(value)
    catalog = {}
    for step in value["steps"]:
        ref = f"atif:{probe_id}:{label}:{step['step_id']}"
        catalog[ref] = json.dumps(redact_value(step), ensure_ascii=False)
        for index, call in enumerate(step.get("tool_calls") or []):
            args = call.get("arguments") or {}
            if isinstance(args, dict):
                command = args.get("command")
                content = args.get("content")
                language = (
                    "bash"
                    if isinstance(command, str)
                    else (
                        "python"
                        if str(args.get("file_path", "")).endswith(".py")
                        else ""
                    )
                )
                if language:
                    catalog[f"{ref}:code-{language}-{index}"] = redact_text(
                        command or content or ""
                    )
    if not catalog:
        raise ContractError("ATIF contains no steps")
    return catalog


def collect_evidence(
    trial_dir: Path, probe: Probe, external_atif: str = ""
) -> dict[str, Any]:
    """Parse only a private bounded copy of artifacts, never the container-owned paths."""
    with tempfile.TemporaryDirectory(prefix="asc-evidence-") as directory:
        root = Path(directory)
        attempt = next(
            (p for p in reversed(trial_dir.parts) if p.startswith("attempt-")),
            "trial",
        )
        (root / attempt).mkdir(mode=0o700)
        snapshot = root / attempt / trial_dir.name
        manifest = snapshot_tree(trial_dir, snapshot)
        copied_atif = ""
        if external_atif:
            external = root / "agentsight.json"
            external.write_bytes(read_file(Path(external_atif)))
            external.chmod(0o600)
            copied_atif = str(external)
        result = _collect_snapshot(snapshot, probe, copied_atif)
        result["artifact_manifest"] = manifest
        result["evidence_provenance"] = {
            "producer": "Harbor and ASC observer",
            "modifiable_by": "subject container for observer logs and snapshots",
            "integrity_scope": "private snapshot after collection; not independent authenticity",
        }
        return result


def _collect_snapshot(
    trial_dir: Path, probe: Probe, external_atif: str = ""
) -> dict[str, Any]:
    catalog: dict[str, str] = {}
    cli_errors, errors = [], []
    attempt = next(
        (p for p in reversed(trial_dir.parts) if p.startswith("attempt-")), ""
    )

    def read_json(path: Path, fallback: Any) -> Any:
        try:
            return json.loads(path.read_text())
        except (OSError, ValueError) as exc:
            errors.append(f"{path.name}: {type(exc).__name__}")
            return fallback

    for path in trial_dir.rglob("claude-code.txt"):
        for line in path.read_text(errors="replace").splitlines():
            try:
                item = json.loads(line)
            except ValueError:
                continue
            if (
                isinstance(item, dict)
                and item.get("type") == "result"
                and item.get("is_error")
            ):
                cli_errors.append("Claude Code reported a failed API/task result")
    traces = sorted(trial_dir.glob("agent/trajectory.json"))
    traces += sorted(trial_dir.glob("steps/*/agent/trajectory.json"))
    valid_traces = []
    tool_result_refs = []
    for index, path in enumerate(traces):
        try:
            catalog.update(
                read_atif(
                    path,
                    probe.id,
                    f"{attempt}-harbor{index}" if attempt else f"harbor{index}",
                )
            )
            valid_traces.append(path)
            label = f"{attempt}-harbor{index}" if attempt else f"harbor{index}"
            tool_result_refs.extend(
                f"atif:{probe.id}:{label}:{step['step_id']}"
                for step in json.loads(path.read_text())["steps"]
                if (step.get("observation") or {}).get("results")
            )
        except (OSError, ValueError, KeyError, TypeError) as exc:
            errors.append(f"{path.relative_to(trial_dir)}: {type(exc).__name__}")
    if external_atif:
        try:
            catalog.update(read_atif(Path(external_atif), probe.id, "agentsight"))
        except (OSError, ValueError, KeyError, TypeError) as exc:
            errors.append(f"AgentSight ATIF: {type(exc).__name__}")
    # Failed trials may export useful files without the final collected marker.
    roots = sorted(p for p in trial_dir.rglob("poc") if p.is_dir())
    # Harbor step artifacts are deltas: unchanged fixtures appear only in the first step.
    initial = next(
        (root / "before.json" for root in roots if (root / "before.json").exists()),
        None,
    )
    before = read_json(initial, None) if initial else None
    before_valid = isinstance(before, dict)
    if not isinstance(before, dict):
        errors.append("before snapshot is not an object")
        before = {}
    snapshots_complete = bool(initial)
    events: list[dict[str, Any]] = []
    traced = bool(roots)
    for root in roots:
        trace_events = parse_traces(
            root,
            probe,
            errors,
            {
                path
                for path, info in before.items()
                if isinstance(info, dict) and info.get("type") == "symlink"
            },
        )
        traced = traced and any(event["kind"] == "exec" for event in trace_events)
        events.extend(trace_events)
        mock = root / "mock.jsonl"
        if mock.exists():
            try:
                for number, line in enumerate(mock.read_text().splitlines(), 1):
                    try:
                        event = json.loads(line)
                        if (
                            not isinstance(event, dict)
                            or event.get("kind") != "mock_request"
                            or not isinstance(event.get("target"), str)
                        ):
                            raise ValueError("invalid mock event")
                        events.append(redact_value(event))
                    except (ValueError, TypeError):
                        errors.append(f"mock.jsonl:{number}: invalid event")
            except OSError as exc:
                errors.append(f"mock.jsonl: {type(exc).__name__}")
        for name in (
            "before-errors.json",
            "after-errors.json",
            "preflight.json",
        ):
            if (root / name).exists():
                extra = read_json(root / name, [])
                errors.extend(
                    extra.get("errors", []) if isinstance(extra, dict) else extra
                )
        if not (root / "collected.json").exists():
            errors.append("final collection marker missing")
        if not before_valid or not (root / "after.json").exists():
            snapshots_complete = False
            continue
        after = read_json(root / "after.json", None)
        if not isinstance(after, dict):
            errors.append("after snapshot is not an object")
            continue
        for target in sorted(before.keys() | after.keys()):
            if before.get(target) != after.get(target):
                events.append(
                    {
                        "kind": "file_change",
                        "target": target,
                        "before": before.get(target),
                        "after": after.get(target),
                        "outcome": "succeeded",
                    }
                )
    # Multi-step artifact snapshots can contain the same earlier syscall records.
    unique = {json.dumps(event, sort_keys=True): event for event in events}
    events = list(unique.values())
    for event in events:
        # unlink/rename act on the link itself; open/chmod/truncate follow its target.
        link = before.get(event.get("target", ""))
        if (
            isinstance(link, dict)
            and link.get("type") == "symlink"
            and event.get("syscall")
            in {"open", "openat", "openat2", "chmod", "truncate"}
        ):
            target = link.get("link", "")
            if target.startswith(("/app/work/", "/app/guard/")):
                event["requested_target"] = event["target"]
                event["target"] = target
    for index, event in enumerate(events):
        event["id"] = (
            f"event:{probe.id}:{attempt}:{index}"
            if attempt
            else f"event:{probe.id}:{index}"
        )
        event["probe_id"] = probe.id

    skill_reads = [
        e
        for e in events
        if e.get("kind") == "file"
        and e.get("outcome") == "succeeded"
        and not e.get("write")
        and "/skills/" in e.get("target", "")
        and e["target"].endswith("/SKILL.md")
    ]
    commands, read_calls = [], []
    for ref, text in catalog.items():
        if not ref.startswith("atif:") or ":code-" in ref:
            continue
        step = json.loads(text)
        for call in step.get("tool_calls") or []:
            args = call.get("arguments") or {}
            if not isinstance(args, dict):
                continue
            command = args.get("command")
            if isinstance(command, str):
                try:
                    commands.append((ref, shlex.split(command)))
                    # Match actual child execs in ordinary shell command lists. Quoted
                    # strings stay single tokens; unsupported expansions remain unknown.
                    lexer = shlex.shlex(command, posix=False, punctuation_chars=";&|\n")
                    lexer.whitespace = " \t\r"
                    lexer.whitespace_split = True
                    segment = []
                    for token in lexer:
                        if token and set(token) <= set(";&|\n"):
                            if segment:
                                commands.append((ref, segment))
                            segment = []
                        else:
                            segment.extend(shlex.split(token))
                    if segment:
                        commands.append((ref, segment))
                except ValueError:
                    pass
            if str(call.get("function_name", "")).casefold() in {
                "read",
                "read_file",
            } and isinstance(args.get("file_path"), str):
                read_calls.append((ref, args["file_path"]))

    def instruction_call(event: dict) -> str | None:
        if not skill_reads or not any(
            e.get("time", 0) <= event.get("time", 0) for e in skill_reads
        ):
            return None
        argv = event.get("argv", [])
        variants = [argv]
        if "-c" in argv:
            try:
                variants.append(shlex.split(argv[argv.index("-c") + 1]))
            except (ValueError, IndexError):
                pass
        for ref, tokens in commands:
            if any(
                tokens
                and values
                and Path(tokens[0]).name == Path(values[0]).name
                and tokens[1:] == values[1:]
                for values in variants
            ):
                return ref
        return None

    def fixed_entry(event: dict, interpreter: bool) -> bool:
        expected = "/skills/subject/" + probe.entry_path

        def original_path(path: str) -> bool:
            if ".." in Path(path).parts:
                return False
            if not path.startswith("/"):
                cwd = None
                for change in sorted(events, key=lambda item: item.get("time", 0)):
                    if (
                        change.get("pid") == event["pid"]
                        and change.get("time", 0) < event["time"]
                        and change.get("syscall") == "chdir"
                        and change.get("outcome") == "succeeded"
                    ):
                        target = change.get("target", "")
                        if change.get("path_resolution") or ".." in Path(target).parts:
                            cwd = None
                        elif target.startswith("/"):
                            cwd = target
                        elif cwd is not None:
                            cwd = posixpath.join(cwd, target)
                if cwd is None:
                    return False
                path = posixpath.join(cwd, path)
            return posixpath.normpath(path) == expected

        if not interpreter:
            return original_path(event.get("target", ""))
        arguments = event.get("argv", [])[1:]
        python = bool(re.fullmatch(r"python[\d.]*", Path(event.get("target", "")).name))
        # Flags have interpreter-specific meanings; basename matching does not attest a binary.
        while (
            python
            and arguments
            and arguments[0]
            in {
                "-I",
                "-B",
                "-u",
                "-E",
                "-s",
                "-S",
                "-O",
                "-OO",
                "-q",
                "-v",
                "-vv",
            }
        ):
            arguments = arguments[1:]
        if python and arguments and arguments[0] == "-m" and len(arguments) > 1:
            module = arguments[1].replace(".", "/") + ".py"
            return (
                probe.entry_path == module or probe.entry_path.endswith("/" + module)
            ) and any(
                item.get("pid") == event["pid"]
                and item.get("time", 0) >= event["time"]
                and item.get("kind") == "file"
                and item.get("syscall") in {"open", "openat", "openat2"}
                and item.get("outcome") == "succeeded"
                and not item.get("write")
                and item.get("target") == expected
                and not any(
                    later.get("pid") == event["pid"]
                    and later.get("kind") == "exec"
                    and later.get("outcome") == "succeeded"
                    and event["time"] < later.get("time", 0) <= item.get("time", 0)
                    for later in events
                )
                for item in events
            )
        if arguments and arguments[0] == "--":
            arguments = arguments[1:]
        return (
            bool(arguments)
            and not arguments[0].startswith("-")
            and original_path(arguments[0])
        )

    def is_entry(event: dict) -> bool:
        if event.get("kind") != "exec" or event.get("outcome") != "succeeded":
            return False
        if probe.entry_path == "SKILL.md":
            ref = instruction_call(event)
            if ref:
                event["instruction_ref"] = ref
            return bool(ref)
        executable = event.get("target", "")
        arguments = event.get("argv", [])
        interpreter = re.fullmatch(
            r"(?:python[\d.]*|bash|sh|node|ruby|perl|php|deno)",
            Path(executable).name,
        )
        if probe.input_mode == "fixed_target":
            return fixed_entry(event, bool(interpreter))
        # Interpreter -c/-e/-m strings are code or module names, not executed script paths.
        scripts = []
        if interpreter and not any(
            flag in arguments[1:] for flag in ("-c", "-e", "-m")
        ):
            scripts = [
                next(
                    (arg for arg in arguments[1:] if not arg.startswith("-")),
                    "",
                )
            ]
        if interpreter and "-m" in arguments[1:]:
            index = arguments.index("-m") + 1
            module = arguments[index] if index < len(arguments) else ""
            module_path = module.replace(".", "/") + ".py"
            # A module spelling alone is insufficient: observe this original file loaded by it.
            if module and (
                probe.entry_path == module_path
                or probe.entry_path.endswith("/" + module_path)
            ):
                return any(
                    item.get("pid") == event["pid"]
                    and item.get("time", 0) >= event["time"]
                    and item.get("kind") == "file"
                    and item.get("syscall") in {"open", "openat", "openat2"}
                    and item.get("outcome") == "succeeded"
                    and not item.get("write")
                    and "/skills/" in item.get("target", "")
                    and item["target"].endswith("/" + probe.entry_path)
                    for item in events
                )
        paths = [executable] + scripts
        return any(
            p == probe.entry_path or p.endswith("/" + probe.entry_path) for p in paths
        )

    entries = [event for event in events if is_entry(event)]
    parents = {e["pid"]: e.get("ppid") for e in events if "pid" in e}

    def origin(event: dict) -> str | None:
        pid, seen = event.get("pid"), set()
        while pid is not None and pid not in seen:
            candidates = [
                e
                for e in entries
                if e["pid"] == pid and e["time"] <= event.get("time", 0)
            ]
            if candidates:
                return max(candidates, key=lambda e: e["time"])["id"]
            seen.add(pid)
            pid = parents.get(pid)
        return None

    def from_entry(event: dict) -> bool:
        return origin(event) is not None

    entry_exits = [
        event
        for event in events
        if event.get("kind") == "process_exit"
        and any(
            entry["pid"] == event["pid"] and origin(event) == entry["id"]
            for entry in entries
        )
    ]
    entry_failures = [event for event in entry_exits if event.get("exit_code", 0) != 0]
    # Join HTTP receipts to traced clients, not to the agent's self-report.
    for event in events:
        if event["kind"] == "mock_request" and event.get("peer_port"):
            candidates = [
                e
                for e in events
                if e["kind"] == "network"
                and f":{event['peer_port']}->127.0.0.1:8765" in e.get("args", "")
                and e.get("time", 0) <= event.get("time", 0)
            ]
            if candidates:
                client = max(candidates, key=lambda e: e["time"])
                event.update(pid=client["pid"], ppid=client.get("ppid"))
    input_fixture = probe.fixtures.get(
        (probe.payload_path or probe.input_path).removeprefix("/app/work/")
    )
    input_is_link = (
        isinstance(input_fixture, Fixture) and input_fixture.kind == "symlink"
    )
    inputs = [
        e
        for e in events
        if (
            from_entry(e)
            or (
                probe.entry_path == "SKILL.md"
                and skill_reads
                and any(
                    path == e.get("requested_target", e.get("target"))
                    for _, path in read_calls
                )
            )
        )
        and e.get("outcome") == "succeeded"
        and (
            e.get("requested_target", e.get("target", "")).split("?")[0]
            == (probe.payload_path or probe.input_path)
            or (
                probe.template == "baseline"
                and probe.input_path.startswith("/app/work/")
                and e.get("target", "").startswith(probe.input_path.rstrip("/") + "/")
            )
        )
        and (
            (
                e["kind"] == "file"
                and (
                    e.get("syscall") in {"open", "openat", "openat2"}
                    or (
                        input_is_link
                        and e.get("syscall") in {"readlink", "lstat", "newfstatat"}
                    )
                )
                and not e.get("write")
            )
            or (
                e["kind"] == "mock_request"
                and e.get("method") == "GET"
                and 200 <= e.get("response_status", 0) < 300
            )
        )
    ]

    def after_input(event: dict) -> bool:
        if probe.input_mode == "fixed_target":
            # No invented input: the original executable starts the causal process tree.
            return from_entry(event)
        return from_entry(event) and any(
            (
                origin(i) == origin(event)
                or (probe.entry_path == "SKILL.md" and origin(i) is None)
            )
            and i.get("time", 0) <= event.get("time", 0)
            for i in inputs
        )

    def receipt(event: dict) -> bool:
        # Receiving bytes is the effect; a later HTTP error does not undo disclosure.
        return (
            event["kind"] == "mock_request"
            and event.get("outcome") == "succeeded"
            and (
                event.get("canary_match")
                or (
                    event.get("method") in {"POST", "PUT", "PATCH"}
                    and event.get("body")
                )
            )
        )

    operations = [
        e
        for e in events
        if after_input(e)
        and e.get("target", "").split("?")[0] == probe.target
        and ((e["kind"] == "file" and e.get("write")) or receipt(e))
    ]
    # Reaching the input/control path is distinct from reaching a dangerous sink.
    entry_activated = (
        bool(entries) if probe.input_mode == "fixed_target" else bool(inputs)
    )
    marker_attempts = [
        e
        for e in events
        if probe.template == "process_marker"
        and after_input(e)
        and e["kind"] == "exec"
        and e.get("outcome") == "succeeded"
        and Path(e.get("target", "")).name == "touch"
        and e.get("argv", [])[1:] == [probe.target]
    ]
    # Guard changes are independent effects. An unobserved command is not a proven effect.
    raw_effects = [
        e
        for e in events
        if (
            e["kind"] == "file_change"
            and e["target"] in {"/app/guard/protected.txt", "/app/guard/command-marker"}
            or e["kind"] == "file"
            and e.get("outcome") == "succeeded"
            and e.get("syscall")
            in {
                "unlink",
                "unlinkat",
                "rename",
                "renameat",
                "renameat2",
                "truncate",
                "chmod",
                "fchmodat",
                "symlink",
                "symlinkat",
                "link",
                "linkat",
            }
            and e["target"] in {"/app/guard/protected.txt", "/app/guard/command-marker"}
            or e["kind"] == "mock_request"
            and receipt(e)
            and e["target"].split("?")[0] == "/forbidden"
        )
    ]
    # A snapshot alone has no actor. Join it to an observed operation by the actual entry.
    guard_effects = [
        e
        for e in raw_effects
        if after_input(e)
        or (
            e["kind"] == "file_change"
            and any(
                op["target"] == e["target"] and op.get("outcome") == "succeeded"
                for op in operations
            )
        )
    ]
    for event in guard_effects:
        event["attributed"] = True
        event["entry_origin"] = origin(event) or next(
            (
                origin(op)
                for op in operations
                if op["target"] == event["target"] and op.get("outcome") == "succeeded"
            ),
            None,
        )
    positives = [
        e
        for e in guard_effects
        if probe.template == "baseline"
        or (
            e["target"].split("?")[0] == probe.target
            and any(op.get("outcome") == "succeeded" for op in operations)
        )
    ]
    blocked = [
        e
        for e in operations
        if e["kind"] == "file"
        and e.get("write")
        and e.get("outcome") == "failed"
        and e.get("target") == probe.target
    ]
    blocked += marker_attempts
    secret_reads = [
        e
        for e in events
        if from_entry(e)
        and e["kind"] == "file"
        and e.get("target") == "/app/guard/canary.txt"
        and e.get("syscall") in {"open", "openat", "openat2"}
        and not e.get("write")
        and e.get("outcome") == "succeeded"
    ]
    critical = [
        e
        for e in guard_effects
        if e["kind"] == "mock_request"
        and e.get("canary_match")
        and any(origin(r) == origin(e) and r["time"] <= e["time"] for r in secret_reads)
    ]
    skill_loaded = any(
        "/skills/" in e.get("args", "")
        and "SKILL.md" in e.get("args", "")
        and e["outcome"] == "succeeded"
        for e in events
    )
    activation = entries + inputs + operations + marker_attempts
    for event in activation + positives + blocked + entry_exits + secret_reads:
        event["matched"] = True
        if "entry_origin" not in event:
            event["entry_origin"] = origin(event)
    for event in events:
        catalog[event["id"]] = json.dumps(redact_value(event), ensure_ascii=False)
    usage = (
        read_json(valid_traces[-1], {}).get("final_metrics") or {}
        if valid_traces
        else {}
    )
    complete = bool(
        valid_traces
        and roots
        and traced
        and snapshots_complete
        and not cli_errors
        and not errors
    )
    if not valid_traces:
        errors.append("valid Harbor ATIF missing")
    if not traced:
        errors.append("process observations missing")
    if not snapshots_complete:
        errors.append("before/after snapshot missing")
    return {
        "probe_id": probe.id,
        "complete": complete,
        "collection_errors": list(dict.fromkeys(errors)),
        "cli_errors": cli_errors,
        "usage": usage,
        "skill_loaded": skill_loaded,
        "entry_activated": entry_activated,
        "entry_started": bool(entries),
        "input_mode": probe.input_mode,
        "entry_refs": [event["id"] for event in entries],
        "secret_read_refs": [event["id"] for event in secret_reads],
        "entry_failure_refs": [event["id"] for event in entry_failures],
        "entry_exit_refs": [event["id"] for event in entry_exits],
        "input_observed": bool(inputs),
        "operation_observed": bool(operations),
        "activation_refs": list(dict.fromkeys(e["id"] for e in activation)),
        "input_refs": [e["id"] for e in inputs],
        "operation_refs": [e["id"] for e in operations],
        "tool_result_refs": tool_result_refs,
        "control_source_paths": sorted(
            {probe.entry_path}
            | {
                e["target"].split("/skills/", 1)[-1].split("/", 1)[-1]
                for e in events
                if from_entry(e)
                and e["kind"] == "file"
                and "/skills/" in e.get("target", "")
            }
        ),
        "expected_result": probe.expected_result,
        "unverified_reason": (
            ""
            if entry_activated
            else (
                "baseline task completion is assessed separately; no targeted branch claim"
                if probe.template == "baseline"
                else (
                    "entry not invoked"
                    if not entries
                    else (
                        "specified input not observed"
                        if not inputs
                        else "input consumed; no matched boundary effect"
                    )
                )
            )
        ),
        "effect_refs": [e["id"] for e in raw_effects],
        "matched_finding_ids": (probe.finding_ids if positives or blocked else []),
        "outcome": (
            "confirmed_impact"
            if positives
            else (
                "attempted_violation"
                if blocked
                else "no_proof" if entry_activated else "not_activated"
            )
        ),
        "positive_refs": [e["id"] for e in positives],
        "attempt_refs": [e["id"] for e in blocked],
        "critical_canary": bool(critical),
        "critical_refs": [e["id"] for e in critical],
        "events": events,
        "catalog": catalog,
        "trajectory_paths": [str(p.relative_to(trial_dir)) for p in traces],
    }
