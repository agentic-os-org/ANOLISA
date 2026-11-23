#!/usr/bin/env python3
"""Desktop integration only; components retain their public CLI contracts."""

import argparse
import getpass
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import warnings
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import agent_installation as agents
import i18n
import setup
from i18n import tr

APP_ID = "community.anolisa"
SERVICE = "anolisa-omarchy-security.service"
SERVICE_UNIT = Path("/etc/systemd/system") / SERVICE
SOCKET = "/run/anolisa-omarchy/sec-core.sock"
COSH_SETTINGS = tr("ANOLISA in the top bar → Settings → Set up cosh")
QODER_SETTINGS = tr("ANOLISA in the top bar → Settings → Set up Qoder")
COMPONENT_HELP = tr(
    """ANOLISA - AI Assistant · Component help

Qoder      Read files, run commands and edit code. Home → Use Qoder.
cosh       An AI work terminal. Home → Open work terminal. Use /auth for models and /agent for questions.
Tokenless  Shorten supported Qoder tool outputs to reduce tokens sent to the model. Home → Tokenless dashboard.
sec-core   Check Qoder Bash commands before execution and record risks. Home → sec-core dashboard.

status shows component status; doctor checks installation and runtime issues."""
)


def locations() -> dict[str, Path]:
    """Keep credentials separate from the immutable application payload."""
    home = Path.home()
    return {
        "prefix": Path(__file__).absolute().parent,
        "config": Path(os.environ.get("XDG_CONFIG_HOME", home / ".config")) / "anolisa-omarchy",
        "state": Path(os.environ.get("XDG_STATE_HOME", home / ".local/state")) / "anolisa-omarchy",
        "run": Path(os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
        / "anolisa-omarchy",
    }


def invoke(command: list[str], *, env: dict[str, str] | None = None) -> dict | list | None:
    """Return JSON only on success; never put component stderr in the bar process."""
    try:
        result = subprocess.run(
            command, capture_output=True, text=True, timeout=6, env=env, check=False
        )
        if result.returncode == 0:
            value = json.loads(result.stdout)
            return value if isinstance(value, (dict, list)) else None
    except (OSError, ValueError, subprocess.TimeoutExpired):
        pass
    return None


def environment(paths: dict[str, Path]) -> dict[str, str]:
    result = dict(os.environ)
    result["PATH"] = str(paths["prefix"] / "bin") + ":" + result.get("PATH", "")
    result["AGENT_SEC_DAEMON_SOCKET"] = SOCKET
    result["TOKENLESS_DATA_DIR"] = str(paths["state"] / "tokenless")
    # AW's provider and the dashboard must resolve the same store.
    result.pop("TOKENLESS_STATS_DB", None)
    result.pop("DSH_TOKENLESS_STATS_DB", None)
    return result


def credential_path(paths: dict[str, Path]) -> Path:
    return paths["config"] / "qoder-auth.json"


def read_credential(paths: dict[str, Path]) -> str | None:
    value = (setup.read_json(credential_path(paths)) or {}).get("token")
    return value if isinstance(value, str) and value.strip() else None


def authenticate(paths: dict[str, Path]) -> None:
    """Use a hidden prompt, not argv or a shell profile, for the Qoder PAT."""
    if not sys.stdin.isatty():
        raise ValueError(tr("Configure your Qoder PAT in an interactive terminal."))
    print(tr("Qoder account: enter your personal access token (hidden input; Ctrl+C to cancel)."))
    setup.private_read(credential_path(paths))
    with warnings.catch_warnings():
        warnings.simplefilter("error", getpass.GetPassWarning)
        try:
            token = getpass.getpass("Qoder PAT: ").strip()
        except getpass.GetPassWarning:
            raise ValueError(
                tr("This terminal cannot hide input. Use another interactive terminal.")
            ) from None
    if not token:
        raise ValueError(
            tr("No token entered. Your existing account configuration was not changed.")
        )
    setup.private_directory(paths["config"])
    setup.atomic_write(credential_path(paths), setup.json_bytes({"token": token}))
    print(tr("Token saved in your private configuration. The connection has not been verified."))


def aw_document(paths: dict[str, Path], qoder: Path | None = None) -> dict[str, Any]:
    prefix, state = paths["prefix"], paths["state"]
    security_argv = [
        str(prefix / "bin/aw-provider-sec-core"),
        "--cli",
        str(prefix / "bin/agent-sec-cli"),
        "--socket",
        "/run/anolisa-omarchy/sec-core.sock",
    ]
    token_argv = [
        str(prefix / "bin/aw-provider-tokenless"),
        "--cli",
        str(prefix / "bin/tokenless"),
        "--data-dir",
        str(state / "tokenless"),
    ]
    return {
        "apiVersion": "aw/v1alpha1",
        "kind": "AWConfiguration",
        "metadata": {"name": "omarchy-assistant"},
        "spec": {
            "daemon": {"startup": "on_demand", "endpoint": "auto", "state_dir": "auto"},
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 5000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {
                "qoder": {"adapter": "qoder", "argv": [str(qoder or prefix / "bin/qodercli")]}
            },
            "providers": {
                "security": {
                    "protocol": "aw-provider/v1alpha1",
                    "transport": {"type": "stdio", "location": "agent", "argv": security_argv},
                    "timeout_ms": 2000,
                    "max_output_bytes": 4096,
                    "config": {
                        "version": 1,
                        "mode": "block",
                        "unmapped_tool": "error",
                        "tools": {"Bash": {"language": "bash", "input_pointer": "/command"}},
                    },
                },
                "tokenless": {
                    "protocol": "aw-provider/v1alpha1",
                    "transport": {"type": "stdio", "location": "agent", "argv": token_argv},
                    "timeout_ms": 2000,
                    "max_output_bytes": 1048576,
                    "config": {"version": 1},
                    "view": {
                        "protocol": "aw-provider-view/v1alpha1",
                        "implementation": "tokenless",
                        "transport": {
                            "type": "stdio",
                            "location": "agent",
                            "argv": [token_argv[0], "--view", *token_argv[1:]],
                        },
                    },
                },
            },
            "events": {
                "tool.before": {
                    "enabled": True,
                    "required": True,
                    "steps": [
                        {
                            "id": "scan-code",
                            "match": {"tools": ["native:qoder:Bash"]},
                            "provider": "security",
                            "operation": "scan_code",
                            "effects": ["observe", "block"],
                            "on_error": "block",
                        },
                        {
                            "id": "rewrite-command",
                            "provider": "tokenless",
                            "operation": "pre_tool",
                            "effects": ["block"],
                            "on_error": "report",
                        },
                    ],
                },
                "tool.after": {
                    "enabled": True,
                    "required": True,
                    "steps": [
                        {
                            "id": "observe-completion",
                            "match": {"tools": ["native:qoder:Bash"]},
                            "provider": "security",
                            "operation": "observe_tool",
                            "effects": ["observe"],
                            "on_error": "report",
                        },
                        {
                            "id": "compress-result",
                            "provider": "tokenless",
                            "operation": "post_tool",
                            "effects": ["replace_result"],
                            "on_error": "report",
                        },
                    ],
                },
            },
        },
    }


def config_bytes(paths: dict[str, Path], selected: dict | None = None) -> bytes:
    return setup.json_bytes(aw_document(paths, agents.executable(paths["prefix"], selected)))


def previous_config_matches(paths: dict[str, Path], current: bytes, checksum: str) -> bool:
    """Recognize a receipted template before its Qoder path was changed."""
    if hashlib.sha256(current).hexdigest() != checksum:
        return False
    try:
        argv = json.loads(current)["spec"]["agents"]["qoder"]["argv"]
    except (ValueError, KeyError, TypeError):
        return False
    if not (
        isinstance(argv, list)
        and len(argv) == 1
        and isinstance(argv[0], str)
        and Path(argv[0]).is_absolute()
    ):
        return False
    # The old path is data used only for comparison, never a command to execute.
    # A matching digest alone must not authorize overwriting a custom document.
    return current == setup.json_bytes(aw_document(paths, Path(argv[0])))


def check_managed_config(
    paths: dict[str, Path], selected: dict | None, *, recover: bool = False
) -> bytes | None:
    config = paths["config"]
    current = setup.private_read(config / "aw.json")
    receipt = setup.read_json(config / "managed-config.json")
    expected = (
        config_bytes(paths, selected)
        if receipt is not None
        else (json.dumps(aw_document(paths), indent=2) + "\n").encode()
    )
    # Selection, config and receipt are published separately. Explicit recovery
    # may reconcile the verified old template or the exact intended new one.
    if recover and current is not None:
        if receipt is None and current == config_bytes(paths, selected):
            return current
        if (
            receipt is not None
            and receipt.get("schema") == 1
            and receipt.get("template") == "demo.3"
            and isinstance(receipt.get("sha256"), str)
            and re.fullmatch(r"[0-9a-f]{64}", receipt["sha256"])
            and (current == expected or previous_config_matches(paths, current, receipt["sha256"]))
        ):
            return current
    if current is not None and (
        current != expected
        or (
            receipt is not None
            and (
                receipt.get("schema") != 1
                or receipt.get("template") != "demo.3"
                or receipt.get("sha256") != hashlib.sha256(current).hexdigest()
            )
        )
    ):
        raise ValueError("Existing configuration differs (aw.json). Review before upgrading.")
    if current is None and receipt is not None:
        raise ValueError("Existing configuration differs (aw.json is missing).")
    return current


def configuration_plan(
    paths: dict[str, Path],
    probe_prefix: Path,
    choice: dict | None = None,
    *,
    recover: bool = False,
) -> dict[Path, bytes]:
    config = paths["config"]
    selected = agents.read_selection(config)
    previous = check_managed_config(paths, selected, recover=recover)
    state = setup.read_state(config)
    plan = {}
    receipt = setup.read_json(config / "managed-config.json")
    if previous is not None and not receipt:
        backup = setup.private_read(config / "aw.demo2.json")
        if backup is not None and backup != previous:
            raise ValueError("Existing configuration differs (aw.demo2.json).")
        plan[config / "aw.demo2.json"] = previous
    old_selection = (
        selected is not None
        and selected["source"] != "downloaded"
        and selected["version"] != agents.VERSION
    )
    if choice is None and (selected is None or old_selection):
        candidate = next(
            (
                item
                for item in agents.candidates(probe_prefix)
                if item["installation"] == "compatible"
            ),
            None,
        )
        choice = agents.selection(candidate) if candidate else None
        # Missing optional Qoder must not prevent upgrading or using cosh.
    selected = choice or selected
    if selected is not None:
        plan[config / "agent.json"] = setup.json_bytes(selected)
    if setup.private_read(config / "setup.json") is None:
        if credential_path(paths).exists() or credential_path(paths).is_symlink():
            state["qoder"]["auth_mode"] = "pat"
        plan[config / "setup.json"] = setup.json_bytes(state)
    document = config_bytes(paths, selected)
    plan[config / "aw.json"] = document
    if not receipt or receipt.get("sha256") != hashlib.sha256(document).hexdigest():
        plan[config / "managed-config.json"] = setup.json_bytes(
            {
                "schema": 1,
                "template": "demo.3",
                "sha256": hashlib.sha256(document).hexdigest(),
                "previous_sha256": hashlib.sha256(previous).hexdigest() if previous else None,
            }
        )
    return plan


def write_plan(plan: dict[Path, bytes]) -> None:
    snapshots = {path: setup.private_read(path) for path in plan}
    changed = []
    try:
        for path, value in plan.items():
            if value == snapshots[path]:
                continue
            setup.atomic_write(path, value)
            changed.append(path)
    except BaseException:
        for path in reversed(changed):
            if snapshots[path] is None:
                path.unlink(missing_ok=True)
            else:
                setup.atomic_write(path, snapshots[path])
        raise


def configure(paths: dict[str, Path]) -> None:
    with setup.lock(paths["run"]):
        plan = configuration_plan(paths, paths["prefix"])
        for directory in ("config", "state"):
            setup.private_directory(paths[directory])
        write_plan(plan)


def auth_mode(paths: dict[str, Path], state: dict) -> str:
    return state["qoder"].get("auth_mode", "pat" if credential_path(paths).is_file() else "native")


def health(paths: dict[str, Path]) -> None:
    subprocess.run(["systemctl", "is-active", "--quiet", SERVICE], check=True, timeout=4)
    subprocess.run(
        [str(paths["prefix"] / "bin/agent-sec-daemon-health")],
        env=environment(paths),
        capture_output=True,
        check=True,
        timeout=4,
    )


def healthy(paths: dict[str, Path]) -> bool:
    try:
        health(paths)
    except (OSError, subprocess.SubprocessError):
        return False
    return True


def timestamp() -> str:
    return datetime.now(UTC).isoformat(timespec="seconds")


def verification(choice: dict) -> dict:
    record = choice.get("verification")
    if isinstance(record, dict) and record.get("result") in ("passed", "failed"):
        try:
            checked_at = datetime.fromisoformat(record["checked_at"]).isoformat(timespec="seconds")
        except (KeyError, ValueError, TypeError):
            pass
        else:
            return {"result": record["result"], "checked_at": checked_at, "method": "user_reported"}
    return {"result": "not_run", "checked_at": None, "method": None}


def dashboards(paths: dict[str, Path], now: str) -> tuple[dict, dict]:
    prefix = paths["prefix"]
    env = environment(paths)
    events = invoke(
        [
            str(prefix / "bin/agent-sec-cli"),
            "events",
            "--output",
            "json",
            "--limit",
            "200",
            "--category",
            "code_scan",
        ],
        env=env,
    )
    security = {
        "available": isinstance(events, list) and all(isinstance(item, dict) for item in events),
        "checked": None,
        "risks": None,
        "blocked": None,
        "scope": "recent_service_code_scans",
        "limit": 200,
        "sampled_at": now,
    }
    if security["available"]:
        findings = []
        for item in events[:200]:
            if item.get("result") != "succeeded":
                continue
            details = item.get("details")
            result = details.get("result") if isinstance(details, dict) else None
            if isinstance(result, dict) and result.get("verdict") in ("warn", "deny"):
                recorded = item.get("timestamp")
                try:
                    recorded = datetime.fromisoformat(recorded).isoformat(timespec="seconds")
                except (ValueError, TypeError):
                    recorded = None
                findings.append({"verdict": result["verdict"], "timestamp": recorded})
        security.update(
            checked=min(len(events), 200), risks=len(findings), recent_risks=findings[:5]
        )
    store = paths["state"] / "tokenless/stats.db"
    tokens = {
        "available": False,
        "saved": None,
        "before": None,
        "after": None,
        "records": None,
        "scope": "recent_managed_user_records",
        "limit": 200,
        "sampled_at": now,
        "state": "empty" if not store.is_file() else "unavailable",
    }
    if store.is_file():
        response = invoke(
            [str(prefix / "bin/tokenless"), "stats", "summary", "--json", "--limit", "200"], env=env
        )
        if isinstance(response, dict) and response.get("schema_version") == "1.1":
            total = response.get("total")
            fields = {
                "saved": "tokens_saved",
                "before": "before_tokens",
                "after": "after_tokens",
                "records": "records",
            }
            if isinstance(total, dict) and all(
                type(total.get(field)) is int and total[field] >= 0 for field in fields.values()
            ):
                tokens.update({key: total[field] for key, field in fields.items()})
                tokens.update(available=True, state="observed" if tokens["records"] else "empty")
    return security, tokens


def status(paths: dict[str, Path], *, diagnostic: bool = False) -> dict[str, Any]:
    issues = []
    config = paths["config"]
    state = setup.read_state(config)
    selected = None
    try:
        selected = agents.read_selection(config)
        qoder = agents.validate(paths["prefix"], selected, full=diagnostic)
    except (OSError, ValueError):
        qoder = {"source": "unknown", "installation": "unknown", "version": None}
        issues.append("qoder_selection")
    mode = auth_mode(paths, state)
    pat = False
    try:
        pat = bool(os.environ.get(agents.PAT) or read_credential(paths))
    except (OSError, ValueError):
        issues.append("qoder_credential")
    try:
        configured = check_managed_config(paths, selected) is not None
    except (OSError, ValueError):
        configured = False
    if not configured:
        issues.append("configuration")
    service_ready = healthy(paths)
    if not service_ready:
        issues.append("security_service")
    if qoder["installation"] != "compatible":
        issues.append("qoder_installation")
    security, tokens = dashboards(paths, timestamp())
    if not security["available"]:
        issues.append("security_records")
    if tokens["state"] == "unavailable":
        issues.append("tokenless_records")
    cosh_path = paths["prefix"] / "bin/cosh"
    cosh_present = cosh_path.is_file() and os.access(cosh_path, os.X_OK)
    if not cosh_present:
        issues.append("cosh_installation")
    qoder.update(
        choice=state["qoder"]["choice"],
        auth_mode=mode,
        verification=verification(state["qoder"]),
        configuration=("present" if pat else "missing") if mode == "pat" else "unknown",
        enhancement="configured" if service_ready and configured else "unavailable",
    )
    if not diagnostic:
        qoder.pop("path", None)
        qoder.pop("sha256", None)
    else:
        qoder["candidates"] = agents.candidates(paths["prefix"])
    report = {
        "schema_version": 2,
        "ok": True,
        "ui": i18n.ui_state(),
        "authConfigured": pat,
        "setup": {
            "needs_prompt": not state["deferred"]
            and any(state[key]["choice"] == "pending" for key in ("cosh", "qoder")),
            "deferred": state["deferred"],
            "running": setup.running(paths["run"]),
        },
        "cosh": {
            "installation": "compatible" if cosh_present else "missing",
            "choice": state["cosh"]["choice"],
            "configuration": "unknown",
            "reported_configured": state["cosh"].get("reported_configured") is True,
            "verification": verification(state["cosh"]),
        },
        "qoder": qoder,
        "security": security,
        "tokenless": tokens,
        "issues": issues,
    }
    if diagnostic:
        report["dependency_tools"] = {
            name: shutil.which(name) is not None
            for name in (
                "omarchy",
                "omarchy-shell",
                "omarchy-launch-terminal",
                "systemctl",
                "sudo",
                "ldd",
            )
        }
        try:
            manifest = json.loads((paths["prefix"] / "bundle.json").read_text())
            report["bundled_components"] = manifest["components"]
        except (OSError, ValueError, KeyError):
            report["bundled_components"] = None
    return report


def component_states(paths: dict[str, Path], report: dict[str, Any]) -> dict[str, tuple[str, str]]:
    binary = paths["prefix"] / "bin"
    issues = report["issues"]

    def executable(name: str) -> bool:
        path = binary / name
        return path.is_file() and os.access(path, os.X_OK)

    qoder = report["qoder"]
    qoder_installation = {
        "compatible": tr("Installed"),
        "incompatible": tr("Incompatible version"),
        "missing": tr("Not installed"),
    }.get(qoder["installation"], tr("Installation not verified"))
    if qoder["installation"] != "compatible":
        qoder_state = tr("Check the selected program")
    elif qoder["configuration"] == "missing":
        qoder_state = tr("PAT required")
    elif qoder["choice"] in ("pending", "skipped"):
        qoder_state = tr("Not configured")
    elif qoder["verification"]["result"] == "passed":
        qoder_state = tr("Last connection check passed")
    elif qoder["verification"]["result"] == "failed":
        qoder_state = tr("Connection check failed")
    elif qoder["configuration"] == "present":
        qoder_state = tr("PAT configured; connection not verified")
    else:
        qoder_state = tr("Account status not verified")
    if qoder.get("build_validation") == "not_verified":
        qoder_state += " · " + tr("Latest build; plugin compatibility not verified")

    cosh = report["cosh"]
    cosh_installed = cosh["installation"] == "compatible"
    cosh_state = tr("Model not configured")
    if not cosh_installed:
        cosh_state = tr("Please reinstall")
    elif cosh["verification"]["result"] == "passed":
        cosh_state = tr("Last connection check passed")
    elif cosh["verification"]["result"] == "failed":
        cosh_state = tr("Connection check failed")
    elif cosh["reported_configured"]:
        cosh_state = tr("Model configured; connection not verified")
    elif cosh["choice"] == "configure":
        cosh_state = tr("Model configuration not confirmed")

    tokens_installed = executable("tokenless")
    provider_installed = executable("aw-provider-tokenless")
    if not tokens_installed or not provider_installed:
        tokens_state = tr("Program files are incomplete; please reinstall")
    elif "configuration" in issues:
        tokens_state = tr("Integration not configured")
    else:
        tokens_state = {
            "observed": tr("Integrated; processing records available"),
            "empty": tr("Integrated; no processing records yet"),
        }.get(report["tokenless"]["state"], tr("Integrated; statistics unavailable"))

    security_installed = executable("agent-sec-cli") and executable("agent-sec-daemon")
    if not security_installed or not SERVICE_UNIT.is_file():
        security_state = tr("Service not installed")
    elif "security_service" not in issues:
        security_state = tr("Service running")
    else:
        try:
            active = (
                subprocess.run(
                    ["systemctl", "is-active", "--quiet", SERVICE], check=False, timeout=4
                ).returncode
                == 0
            )
        except (OSError, subprocess.SubprocessError):
            active = False
        security_state = tr("Service health check failed") if active else tr("Service not running")

    return {
        "Qoder": (qoder_installation, qoder_state),
        "cosh": (tr("Installed") if cosh_installed else tr("Not installed"), cosh_state),
        "Tokenless": (tr("Installed") if tokens_installed else tr("Not installed"), tokens_state),
        "sec-core": (
            tr("Installed") if security_installed else tr("Not installed"),
            security_state,
        ),
    }


def print_status(paths: dict[str, Path], report: dict[str, Any], *, diagnostic: bool) -> None:
    rows = component_states(paths, report)
    if diagnostic:
        issues = set(report["issues"])
        security_state = rows["sec-core"][1]
        checks = {
            "qoder_selection": (
                "Qoder",
                tr("Cannot read the program selection"),
                tr("Select the Qoder program again in Settings"),
            ),
            "qoder_credential": (
                "Qoder",
                tr("Cannot read the PAT configuration"),
                tr("Configure your Qoder PAT again"),
            ),
            "configuration": (
                tr("Qoder enhancements"),
                tr("Managed configuration is missing or modified"),
                tr("Run the installer again and follow its instructions"),
            ),
            "qoder_installation": (
                "Qoder",
                rows["Qoder"][0],
                tr("Select a compatible program in Settings"),
            ),
            "cosh_installation": (
                "cosh",
                tr("Program not installed"),
                tr("Run the installer again"),
            ),
            "security_service": (
                "sec-core",
                security_state,
                tr("Run the installer again and check the service logs"),
            ),
            "security_records": (
                "sec-core",
                tr("Cannot read scan records"),
                tr("Check the safety service logs"),
            ),
            "tokenless_records": (
                "Tokenless",
                tr("Cannot read processing records"),
                tr("Open the Tokenless dashboard for details"),
            ),
        }
        if "qoder_selection" in issues:
            issues.discard("qoder_installation")
        if "security_service" in issues:
            issues.discard("security_records")
        problems = [checks[issue] for issue in checks if issue in issues]
        if report["qoder"]["configuration"] == "missing" and "qoder_credential" not in issues:
            problems.append(
                (
                    "Qoder",
                    tr("PAT not provided"),
                    tr("Enter a PAT in Settings or switch to account setup in Qoder CLI"),
                )
            )
        if rows["Tokenless"][1] == tr("Program files are incomplete; please reinstall"):
            problems.append(
                ("Tokenless", tr("Program files are incomplete"), tr("Run the installer again"))
            )
        if security_state == tr("Service not installed") and "security_service" not in issues:
            problems.append(("sec-core", security_state, tr("Run the installer again")))
        if problems:
            for name, reason, next_step in problems:
                print(tr("{0}  {1}. {2}.", name, reason, next_step))
        else:
            print(tr("No installation or runtime issues found."))
        return

    try:
        metadata = json.loads((paths["prefix"] / "bundle.json").read_text())
        version = metadata.get("version", "") if isinstance(metadata, dict) else ""
    except (OSError, ValueError):
        version = ""
    print(tr("ANOLISA - AI Assistant {0}", version).rstrip())
    print(
        tr(
            """
Component   Installation   Current status"""
        )
    )
    for name, (installation, state) in rows.items():
        print(f"{name:<12}{installation}    {state}")
    if report["setup"]["needs_prompt"]:
        print(
            tr(
                """
Next: open ANOLISA in the top bar and select Start setup."""
            )
        )


def run_agent(
    paths: dict[str, Path], arguments: list[str], native_settings: Path | None = None
) -> int:
    health(paths)
    config = paths["config"]
    with setup.lock(paths["run"]):
        state = setup.read_state(config)
        selected = agents.read_selection(config)
        report = agents.validate(paths["prefix"], selected)
        if report["installation"] != "compatible":
            raise ValueError(
                tr("The Qoder program is unavailable or has changed. Select it again in Settings.")
            )
        if check_managed_config(paths, selected) is None:
            raise ValueError(tr("Installation configuration is missing. Run the installer again."))
        env = environment(paths)
        if auth_mode(paths, state) == "pat":
            token = env.get(agents.PAT) or read_credential(paths)
            if not token:
                raise ValueError(
                    tr("Enter a Qoder PAT in Settings or switch to account setup in Qoder CLI.")
                )
            env[agents.PAT] = token
        else:
            env.pop(agents.PAT, None)
        stable = dict(paths, prefix=paths["prefix"].resolve())
        document = setup.json_bytes(aw_document(stable, Path(report["path"])))
        run_config = paths["run"] / (hashlib.sha256(document).hexdigest() + ".json")
        if setup.private_read(run_config) != document:
            setup.atomic_write(run_config, document)
    command = [
        str(stable["prefix"] / "bin/aw"),
        "run",
        "--config",
        str(run_config),
        "--agent",
        "qoder",
    ]
    if report["source"] in ("bundled", "downloaded"):
        settings = json.loads(native_settings.read_text()) if native_settings else {}
        if not isinstance(settings, dict) or not isinstance(settings.get("general", {}), dict):
            raise ValueError(tr("Qoder settings must be a JSON object."))
        settings.setdefault("general", {})["enableAutoUpdate"] = False
        with tempfile.TemporaryDirectory(dir=paths["run"]) as directory:
            overlay = Path(directory) / "settings.json"
            setup.atomic_write(overlay, setup.json_bytes(settings))
            return subprocess.call(
                [*command, "--native-settings", str(overlay), "--", *arguments], env=env
            )
    if native_settings is not None:
        command += ["--native-settings", str(native_settings.resolve(strict=True))]
    return subprocess.call([*command, "--", *arguments], env=env)


def run_cosh(paths: dict[str, Path], arguments: list[str]) -> int:
    print(tr("cosh: use /auth to configure a model and /status to check its status."), flush=True)
    print(
        tr(
            "Enter /agent on its own, then type your question after ◆. Esc returns to the shell; exit closes it."
        ),
        flush=True,
    )
    env = environment(paths)
    env.pop(agents.PAT, None)
    # Mise's missing-command handler takes precedence over cosh's natural-language routing.
    env["MISE_NOT_FOUND_AUTO_INSTALL"] = "0"
    # Only this cosh process follows the plugin language; user and system locales stay intact.
    env["COSH_SHELL_LANG"] = i18n.language()
    return subprocess.call([str(paths["prefix"] / "bin/cosh"), *arguments], env=env)


def skip_qoder(paths: dict[str, Path], state: dict) -> None:
    state["qoder"]["choice"] = "skipped"
    setup.save_state(paths["config"], state)
    print(tr("Qoder setup skipped. Continue later from {0}.", QODER_SETTINGS))


def install_qoder(paths: dict[str, Path], channel: str | None = None) -> bool:
    """Keep program installation separate from account setup and preserve credentials."""
    if channel is None:
        print(tr("Qoder CLI is installed separately from the official source."))
        print(
            tr(
                "1. Recommended version {0} (default)\n2. Latest version\n3. Set up later",
                agents.VERSION,
            )
        )
        answer = input(tr("Choose an option [default 1]: ")).strip() or "1"
        if answer == "3":
            print(tr("You can install Qoder later from Settings → Qoder version."))
            return False
        if answer not in ("1", "2"):
            raise ValueError(tr("Choose 1, 2 or 3."))
        channel = "recommended" if answer == "1" else "latest"
    if channel == "latest":
        print(
            tr(
                "Latest may not be verified with this plugin. You can reinstall the recommended version if needed."
            )
        )
    report = agents.install(channel)
    setup.private_directory(paths["config"])
    write_plan(configuration_plan(paths, paths["prefix"], agents.selection(report)))
    print(
        tr("Qoder CLI {0} is installed. Next, configure your account.", report["version"]),
        flush=True,
    )
    return True


def choose_qoder(paths: dict[str, Path], state: dict) -> bool:
    candidates = agents.candidates(paths["prefix"])
    usable = [item for item in candidates if item["installation"] == "compatible"]
    if not usable:
        if any(item["source"] == "existing" for item in candidates):
            print(tr("Qoder CLI was found, but it did not pass the plugin compatibility checks."))
        print(tr("No compatible Qoder CLI found."))
        if not install_qoder(paths):
            skip_qoder(paths, state)
            return False
        usable = [
            item
            for item in agents.candidates(paths["prefix"])
            if item["installation"] == "compatible"
        ]
    else:
        print(tr("Qoder CLI detected. Next, configure your account."))
    current = agents.read_selection(paths["config"])
    selected = next((item for item in usable if agents.selection(item) == current), usable[0])
    plan = configuration_plan(paths, paths["prefix"], agents.selection(selected))
    existing_mode = auth_mode(paths, state)
    default_mode = "2" if existing_mode == "pat" else "1"
    print(
        tr(
            """
Where would you like to configure your Qoder account?

1. Configure in Qoder CLI
   Open Qoder and follow its browser or PAT authentication prompts.

2. Enter your Qoder PAT here
   Paste your token here. The plugin saves it and uses it when opening Qoder.

3. Set up later"""
        )
    )
    mode = input(tr("Choose an option [default {0}]: ", default_mode)).strip().lower()
    if mode == "3":
        skip_qoder(paths, state)
        return False
    mode = mode or default_mode
    if mode not in ("1", "2"):
        raise ValueError(tr("Invalid account setup option."))
    if mode == "2":
        if os.environ.get(agents.PAT) or read_credential(paths):
            print(tr("Your existing Qoder PAT will be used."))
        else:
            authenticate(paths)
    state["qoder"] = {"choice": "configure", "auth_mode": "native" if mode == "1" else "pat"}
    plan[paths["config"] / "setup.json"] = setup.json_bytes(state)
    setup.private_directory(paths["config"])
    write_plan(plan)
    return mode == "1"


def wizard(paths: dict[str, Path], component: str | None, *, defer: bool = False) -> None:
    with setup.lock(paths["run"]):
        state = setup.read_state(paths["config"])
        if defer:
            state["deferred"] = True
            setup.save_state(paths["config"], state)
            return
        if not sys.stdin.isatty():
            raise ValueError(tr("Run setup in an interactive terminal."))
        print(
            tr(
                """ANOLISA - AI Assistant
Set up cosh and Qoder separately. You can skip either and return later."""
            )
        )
        open_qoder = False
        for name in (component,) if component else ("cosh", "qoder"):
            if component is None and state[name]["choice"] != "pending":
                continue
            try:
                if name == "qoder":
                    if component is None:
                        cosh = state["cosh"]
                        if cosh.get("reported_configured"):
                            print(tr("cosh configuration is complete. Set up Qoder next?"))
                        elif cosh["choice"] == "skipped":
                            print(tr("cosh setup was skipped. Set up Qoder next?"))
                        else:
                            print(tr("cosh setup is incomplete. Set up Qoder next?"))
                        print(tr("Press Enter to skip. Continue later from {0}.", QODER_SETTINGS))
                        answer = input(tr("Enter 1 to configure Qoder: ")).strip()
                        if not answer:
                            skip_qoder(paths, state)
                            continue
                        if answer != "1":
                            raise ValueError(tr("Enter 1 to continue, or press Enter to skip."))
                    open_qoder = choose_qoder(paths, state)
                else:
                    print(
                        tr(
                            "Set up the cosh work terminal. You can return later from {0}.",
                            COSH_SETTINGS,
                        )
                    )
                    answer = input(
                        tr("Enter 1 to configure cosh, or press Enter to skip: ")
                    ).strip()
                    if answer not in ("", "1"):
                        raise ValueError(tr("Invalid cosh setup option."))
                    if answer:
                        print(tr("Once cosh opens, enter /auth to configure a model."))
                        print(tr("When finished, enter exit to return to this setup wizard."))
                        input(tr("Press Enter to open cosh for configuration: "))
                        if run_cosh(paths, []) != 0:
                            raise ValueError(tr("cosh exited with an error."))
                        confirmed = (
                            input(tr("Have you configured a model in cosh? [y/N]: "))
                            .strip()
                            .lower()
                            == "y"
                        )
                        state["cosh"] = {"choice": "configure", "reported_configured": confirmed}
                    else:
                        state["cosh"]["choice"] = "skipped"
                    setup.save_state(paths["config"], state)
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                location = COSH_SETTINGS if name == "cosh" else QODER_SETTINGS
                print(
                    tr(
                        """{0} setup is incomplete. {1}
Try again later from {2}.""",
                        name,
                        error,
                        location,
                    )
                )
        labels = {
            "pending": tr("Not configured"),
            "configure": tr("Configured; connection not verified"),
            "skipped": tr("Skipped"),
        }
        final = setup.read_state(paths["config"])
        for name in ("cosh", "qoder"):
            print(f"{name}  {labels[final[name]['choice']]}")
        check = (
            "2"
            if open_qoder
            else input(
                tr("Optional check: 1 opens cosh, 2 opens Qoder; press Enter to finish: ")
            ).strip()
        )
    if check in ("1", "2"):
        name = "cosh" if check == "1" else "qoder"
        if open_qoder:
            print(
                tr(
                    "Opening Qoder CLI. Use /login to configure your account, then enter exit to return to setup."
                )
            )
        else:
            print(tr("Ask a simple question, then enter exit to return to setup."))
        code = run_cosh(paths, []) if name == "cosh" else run_agent(paths, [])
        answer = (
            input(
                tr("Did you receive a real response? [y yes / n failed / Enter skips recording]: ")
            )
            .strip()
            .lower()
        )
        if answer in ("y", "n"):
            with setup.lock(paths["run"]):
                latest = setup.read_state(paths["config"])
                if latest[name] != final[name]:
                    raise ValueError(
                        tr("Configuration changed during the check. Run the check again.")
                    )
                latest[name]["verification"] = {
                    "result": "passed" if code == 0 and answer == "y" else "failed",
                    "checked_at": timestamp(),
                }
                setup.save_state(paths["config"], latest)


def main() -> int:
    parser = argparse.ArgumentParser(
        prog="anolisa-omarchy",
        description=tr(
            "ANOLISA - AI Assistant. Run anolisa-omarchy help for components and commands."
        ),
    )
    actions = parser.add_subparsers(dest="action", required=True)
    actions.add_parser("help", help=tr("Explain components and commands"))
    actions.add_parser("language", help=tr("Show or change the interface language")).add_argument(
        "value", nargs="?", choices=i18n.LANGUAGES
    )
    for action in ("cosh", "auth", "configure", "security", "tokens"):
        actions.add_parser(action)
    for action in ("status", "doctor"):
        actions.add_parser(action).add_argument("--json", action="store_true")
    for action in ("setup", "settings"):
        group = actions.add_parser(action).add_mutually_exclusive_group()
        group.add_argument("--component", choices=("cosh", "qoder"))
        group.add_argument("--defer", action="store_true")
    actions.add_parser("qoder").add_argument("--native-settings", type=Path)
    actions.add_parser("qoder-install").add_argument("--channel", choices=("recommended", "latest"))
    arguments = sys.argv[1:]
    forwarded = []
    if "--" in arguments:
        split = arguments.index("--")
        arguments, forwarded = arguments[:split], arguments[split + 1 :]
    options = parser.parse_args(arguments)
    action = options.action
    if forwarded and action not in ("cosh", "qoder"):
        parser.error("Only cosh and qoder accept forwarded arguments")
    paths = locations()
    if action == "help":
        print(COMPONENT_HELP)
        print(tr("\nLanguage: anolisa-omarchy language [en-US|zh-CN]"))
    elif action == "language":
        if options.value is not None:
            i18n.set_language(options.value)
        print(tr("Interface language: {0}", i18n.language()))
    elif action == "configure":
        configure(paths)
    elif action == "qoder-install":
        with setup.lock(paths["run"]):
            install_qoder(paths, options.channel)
        if sys.stdin.isatty():
            input(tr("Press Enter to close."))
    elif action == "auth":
        with setup.lock(paths["run"]):
            state = setup.read_state(paths["config"])
            authenticate(paths)
            state["qoder"] = {"choice": "configure", "auth_mode": "pat"}
            setup.save_state(paths["config"], state)
    elif action == "qoder":
        try:
            return run_agent(paths, forwarded, options.native_settings)
        except (OSError, subprocess.SubprocessError) as error:
            # File and process errors can occur after a successful safety check.
            print(
                tr(
                    "Could not start Qoder: {0}. Run anolisa-omarchy doctor for diagnostics.",
                    str(error),
                ),
                file=sys.stderr,
            )
            if sys.stdin.isatty():
                input(tr("Press Enter to close."))
            return 1
    elif action == "cosh":
        return run_cosh(paths, forwarded)
    elif action in ("setup", "settings"):
        wizard(paths, options.component, defer=options.defer)
    elif action in ("status", "doctor"):
        report = status(paths, diagnostic=action == "doctor")
        if options.json:
            print(json.dumps(report, ensure_ascii=False))
        else:
            print_status(paths, report, diagnostic=action == "doctor")
        if action == "doctor" and not options.json and sys.stdin.isatty():
            input(tr("Press Enter to close."))
    else:
        command = (
            [str(paths["prefix"] / "bin/agent-sec-cli"), "events", "--limit", "50"]
            if action == "security"
            else [str(paths["prefix"] / "bin/tokenless"), "stats", "summary", "--limit", "200"]
        )
        subprocess.run(command, env=environment(paths), check=False)
        if sys.stdin.isatty():
            input(tr("Press Enter to close."))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyboardInterrupt, EOFError):
        print(
            tr(
                """
Cancelled."""
            ),
            file=sys.stderr,
        )
        raise SystemExit(130) from None
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"ANOLISA: {error}", file=sys.stderr)
        raise SystemExit(1) from None
