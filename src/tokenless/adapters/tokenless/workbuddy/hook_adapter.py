"""workbuddy hook protocol: host inputs, capabilities and output envelopes.

WorkBuddy (Tencent CodeBuddy) ships three hosts that share the ``workbuddy``
agent id and the ``~/.codebuddy`` settings.json hook registration: the
CodeBuddy Code CLI, WorkBuddy desktop (IDE) and WorkBuddy Enterprise. Their
hook contracts differ, so this adapter declares a per-event capability
instead of one static envelope.

PreToolUse
    The official contract applies ``modifiedInput`` only together with
    ``permissionDecision: "allow"`` (and its troubleshooting Q5: with any
    other decision the tool keeps the original parameters). Protocol v2
    routes the rtk run through the Tokenless Core, which deliberately
    reports Allow and Ask/Default rewrites alike — the hook never learns
    whether rtk's permission rules attested the command. Emitting "allow"
    unconditionally would therefore bypass the host permission gate for
    unattested commands, so the default passes the original command through
    and keeps the host's normal permission flow; users who accept the
    bypass opt in with ``TOKENLESS_WORKBUDDY_AUTO_ALLOW=1`` (documented in
    the user guide).

    The documented HookInput (CLI, IDE and Enterprise references alike)
    carries no tool-call identifier — only session_id, transcript_path,
    cwd, permission_mode, hook_event_name and the event fields — so this
    host opts out of the shared call-id requirement. The
    PreTool->PostTool optimization state is keyed by that id and can never
    be consumed without one, so an id-less call simply never marks it.

PostToolUse
  - CodeBuddy Code CLI host: the compressed payload *replaces* the
    model-visible tool result via ``hookSpecificOutput.updatedToolOutput``
    — the CLI Hooks contract (v1.16.0+, the first release with hook
    support) defines it as a full replacement for built-in and MCP tools,
    made for compressing long tool outputs. Using the additive
    ``additionalContext`` here would keep the original result and append,
    duplicating the payload. ``additionalContext`` stays reserved for
    additive environment attribution.

    Recognizing the CLI host is multi-signal and every signal fails safe to
    the non-CLI path, because no single indicator spans the whole declared
    support range:

    - ``CODEBUDDY_FORCE_HEADLESS_BUNDLE`` (host launcher marker): the
      official ``bin/codebuddy`` entry documents that a WorkBuddy host sets
      it before spawning ``cbc`` for its sidecar / prewarm pool, and the
      hook inherits the bundle environment. The marker only exists from CLI
      2.136.0 on, so its absence proves nothing by itself.
    - daemon session kind: the Daemon Mode reference documents
      ``CODEBUDDY_SESSION_KIND`` as the worker type (interactive / bg /
      daemon); the resident daemon worker declares ``daemon`` and is
      excluded from compression. Every other value is standalone evidence
      for the CLI's own sessions.
    - hosted argv shapes: a CLI-binary ancestor carrying ``--prewarm`` /
      ``--prewarm-force`` / ``--teammate-mode`` is a spawned headless
      sidecar — these modes exist in artifacts that predate the launcher
      marker. ``--serve`` is deliberately NOT a hosted signal: the Web UI
      reference documents users starting ``codebuddy --serve`` directly,
      and the resident daemon that ``daemon start`` forks with ``--serve``
      prepended is separated by the daemon session kind.
    - standalone CLI: a CLI-binary ancestor free of every hosted signal
      above. The controlling terminal is deliberately NOT required — the
      supported headless shapes (``-p`` / ``--print`` for CI/CD and stdin
      pipelines, ``--acp``, ``--bg``, and the user-started ``--serve`` Web
      UI) legitimately run without a TTY, and the CLI Hooks contract still
      honors ``updatedToolOutput`` there.

    ``CODEBUDDY_PROJECT_DIR`` cannot discriminate either: the IDE Hooks
    reference lists it for IDE hook scripts as well.
  - IDE / Enterprise / unknown host: those hook references document only
    the additive ``additionalContext`` for PostToolUse, which keeps the
    original tool result. Emitting the compressed payload there would grow
    the context instead of shrinking it, so non-CLI hosts fail open: they
    declare no replacement capability, compression is disabled, and only
    genuinely additive environment attribution is delivered.
"""

from __future__ import annotations

import os
import subprocess
import sys

from hook_utils import (
    SHELL_TOOLS,
    is_skill_file,
    is_tokenless_retrieve_command,
    skip,
    tokenless_retrieve_command_available,
    warn,
)
from result_utils import (
    ToolResult,
    content_origin,
    process_result,
    result_status,
    select_text_field,
)

# -- PreToolUse --------------------------------------------------------------

# Opt-in for the documented ``modifiedInput`` + ``permissionDecision`` bypass.
# Any value other than ``1`` keeps the host's own permission flow.
_AUTO_ALLOW_ENV = "TOKENLESS_WORKBUDDY_AUTO_ALLOW"

# The documented workbuddy HookInput carries no tool-call identifier, so the
# shared runtime must not require one for this host.
call_id_required = False

# -- PostToolUse: CodeBuddy Code CLI host discriminator ----------------------

# Basenames of the CodeBuddy Code CLI binary. The published CLI package's
# ``bin`` entries all point at the same entry script: ``codebuddy``,
# ``codebuddy-code`` and ``cbc``. WorkBuddy desktop (IDE), WorkBuddy
# Enterprise and the CLI share the ``workbuddy`` agent id and the
# ``~/.codebuddy`` settings.json hook registration, and both the CLI and
# IDE hook scripts receive ``CODEBUDDY_PROJECT_DIR`` (the IDE Hooks
# reference lists it under the hook environment variables), so neither the
# agent id nor that variable can discriminate the hosts.
_CODEBUDDY_CLI_BASENAMES = frozenset({"codebuddy", "codebuddy-code", "cbc"})

# Host-launcher contract: the published CLI package's ``bin/codebuddy``
# entry documents that a host process (WorkBuddy desktop spawning ``cbc``
# for its sidecar / prewarm pool) sets this variable before starting the
# headless bundle. The bundle's hook executor merges its own process
# environment into the hook environment, so the variable reaches the hook
# exactly when the CLI runs embedded in a WorkBuddy host instead of
# standalone in a terminal. Values follow the entry script's parsing:
# ``1`` / ``true`` (case-insensitive) count as set, anything else is
# ignored.
_WORKBUDDY_HEADLESS_LAUNCH_ENV = "CODEBUDDY_FORCE_HEADLESS_BUNDLE"

# Hosted / headless process shapes: flags carried by cbc processes that a
# host (WorkBuddy desktop prewarm pool) or the CLI's own detached team
# backends spawn instead of a user session. These modes exist in
# artifacts that predate the launcher marker, so they are checked on
# every version. ``--serve`` is deliberately NOT hosted evidence: the
# official Web UI reference documents users starting ``codebuddy --serve``
# directly, and the CLI Hooks contract honors updatedToolOutput in that
# host. Daemon workers (``daemon start`` forks the resident child with
# ``--serve`` prepended) are excluded through the documented session-kind
# environment variable instead (see _workbuddy_cli_host).
_WORKBUDDY_HOSTED_ARGV_FLAGS = frozenset(
    {"--prewarm", "--prewarm-force", "--teammate-mode"}
)

# Daemon worker evidence: the Daemon Mode reference documents
# CODEBUDDY_SESSION_KIND as the worker type (interactive / bg / daemon),
# and the daemon child inherits it into the hook environment. It is the
# contract-backed signal that separates the resident daemon (which is not
# a standalone replacement-capable CLI) from a user-started
# ``codebuddy --serve`` Web UI session (kind interactive), whose argv
# shapes are otherwise identical. Matching is case-insensitive; every
# other value (interactive / bg / unset) stays standalone.
_WORKBUDDY_DAEMON_SESSION_KIND = "daemon"

# Interpreter basenames that can front a script-style CLI launch; for these
# the script path is the first argument (shebang exec and `env` re-exec both
# settle into this shape).
_CODEBUDDY_CLI_INTERPRETERS = frozenset(
    {"sh", "bash", "dash", "zsh", "ksh", "node", "nodejs", "bun", "deno"}
)

# How many ancestor levels to walk: codebuddy -> shell -> hook is two,
# with headroom for terminals, tmux and login shells in between.
_WORKBUDDY_ANCESTOR_DEPTH = 24


def _argv_is_codebuddy_cli(argv: list) -> bool:
    """Whether one process' argv belongs to the CodeBuddy Code CLI."""
    if not argv:
        return False
    base = os.path.basename(argv[0])
    if base in _CODEBUDDY_CLI_BASENAMES:
        return True
    if base.startswith("python") or base in _CODEBUDDY_CLI_INTERPRETERS:
        # Script launch: the script path is the first argument.
        return (
            len(argv) > 1
            and os.path.basename(argv[1]) in _CODEBUDDY_CLI_BASENAMES
        )
    return False


def _ancestor_procs_from_proc(max_depth: int):
    """Yield the argv list of each ancestor by walking /proc."""
    pid = os.getppid()
    seen = set()
    for _ in range(max_depth):
        if pid <= 1 or pid in seen:
            return
        seen.add(pid)
        try:
            with open(f"/proc/{pid}/stat", encoding="ascii",
                      errors="replace") as f:
                stat_line = f.read()
        except OSError:
            return
        # comm (field 2) may contain spaces and parens; it ends at the
        # last ')'. The ppid is the second field after it (state is first).
        rparen = stat_line.rfind(")")
        fields = stat_line[rparen + 2:].split() if rparen != -1 else []
        if len(fields) < 2:
            return
        try:
            with open(f"/proc/{pid}/cmdline", "rb") as f:
                argv = [a.decode("utf-8", "replace")
                        for a in f.read().split(b"\0") if a]
        except OSError:
            argv = []
        yield argv
        try:
            pid = int(fields[1])
        except ValueError:
            return


def _ancestor_procs_from_ps(max_depth: int):
    """Yield the argv list of each ancestor via one ``ps`` scan."""
    cmd = ["ps", "-ax", "-o", "pid=", "-o", "ppid=", "-o", "args="]
    if sys.platform == "darwin":
        # BSD ps truncates arguments to terminal width without -ww.
        cmd.insert(1, "-ww")
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=3)
    except Exception:
        return
    if proc.returncode != 0:
        return
    table = {}
    for line in proc.stdout.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) < 3:
            continue
        try:
            table[int(parts[0])] = (int(parts[1]), parts[2].split())
        except ValueError:
            continue
    pid = os.getppid()
    seen = set()
    for _ in range(max_depth):
        if pid <= 1 or pid in seen or pid not in table:
            return
        seen.add(pid)
        ppid, argv = table[pid]
        yield argv
        pid = ppid


def _ancestor_procs():
    """Yield the argv list of every ancestor process, nearest first.

    Best-effort by design: on platforms where the walk is unavailable
    (e.g. Windows) nothing is yielded and callers fail safe to the
    non-CLI path.
    """
    if sys.platform.startswith("linux"):
        yield from _ancestor_procs_from_proc(_WORKBUDDY_ANCESTOR_DEPTH)
    else:
        yield from _ancestor_procs_from_ps(_WORKBUDDY_ANCESTOR_DEPTH)


def _launched_by_workbuddy_host() -> bool:
    """Whether a WorkBuddy host process spawned the running CLI bundle.

    Mirrors the ``bin/codebuddy`` entry parsing: the host sets
    ``CODEBUDDY_FORCE_HEADLESS_BUNDLE`` to ``1`` / ``true`` (any case)
    before spawning ``cbc`` for the desktop sidecar or the prewarm pool;
    every other value is ignored.
    """
    # Mirror the entry script's parsing exactly: lower-case ``1`` /
    # ``true`` count as set; no trimming, every other value is ignored.
    value = os.environ.get(_WORKBUDDY_HEADLESS_LAUNCH_ENV, "")
    return value.lower() in {"1", "true"}


def _argv_is_hosted_shape(argv: list) -> bool:
    """Whether the argv carries a hosted / headless sidecar mode flag."""
    return any(token in _WORKBUDDY_HOSTED_ARGV_FLAGS for token in argv)


def _workbuddy_cli_host() -> bool:
    """Whether the hook is executed by a standalone CodeBuddy Code CLI.

    Multi-signal (see the module doc): the host launcher marker is
    positive hosted evidence; ``CODEBUDDY_SESSION_KIND=daemon`` marks the
    resident daemon worker; a CLI-binary ancestor carrying a hosted mode
    flag (``--prewarm`` / ``--prewarm-force`` / ``--teammate-mode``) is a
    spawned sidecar even in artifacts predating the marker. A CLI-binary
    ancestor free of every hosted signal is a standalone CLI regardless of
    whether it owns a controlling terminal: the supported headless shapes
    (``-p`` / ``--print`` for CI/CD and stdin pipelines, ``--acp``,
    ``--bg``, and the user-started ``--serve`` Web UI) legitimately run
    without a TTY, and the CLI Hooks contract still honors
    ``updatedToolOutput`` there. ``--serve`` is NOT hosted evidence: the
    Web UI reference documents users launching ``codebuddy --serve``
    directly; the resident daemon that ``daemon start`` forks with
    ``--serve`` prepended is separated by the documented session kind
    instead. Session kinds other than ``daemon`` (``interactive`` / ``bg``
    / unset) are never treated as hosted evidence: the standalone CLI
    declares them for its own sessions. A missing marker is never treated
    as proof of a standalone CLI by itself — the hosted signals are what
    rule a host out, and the ancestry walk is best-effort anyway. No
    version probe is needed once the CLI is detected: hooks exist only in
    CodeBuddy Code v1.16.0+, and that same contract defines
    ``updatedToolOutput``.
    """
    if _launched_by_workbuddy_host():
        return False
    if (
        os.environ.get("CODEBUDDY_SESSION_KIND", "").strip().lower()
        == _WORKBUDDY_DAEMON_SESSION_KIND
    ):
        return False
    for argv in _ancestor_procs():
        if not _argv_is_codebuddy_cli(argv):
            continue
        if _argv_is_hosted_shape(argv):
            return False
        return True
    return False


# -- adapter contract --------------------------------------------------------

recovery_available = tokenless_retrieve_command_available
is_retrieve_result = is_tokenless_retrieve_command
schema_input = None  # This host has no BeforeModel schema replacement contract.


def prepare_input(data: dict) -> dict:
    return data


def tool_call_id(data: dict) -> str:
    """The tool-call id when the payload carries one, else ``""``.

    No documented workbuddy HookInput shape declares a call id; the field
    reads stay in place so a host that adds one links PreTool to PostTool
    like every other adapter.
    """
    return data.get("tool_use_id") or data.get("toolCallId", "")


def pre_tool_input(data: dict) -> tuple[str, dict | None]:
    # Gate before Core is invoked: without the opt-in the rewrite cannot be
    # emitted at all (``modifiedInput`` needs ``permissionDecision: allow``,
    # which Protocol v2 cannot attest), so the host's own permission flow
    # keeps the original command. Skipping here also leaves the rtk
    # optimization ledger untouched and spawns no subprocess.
    if os.environ.get(_AUTO_ALLOW_ENV) != "1":
        warn(
            "WorkBuddy rewrite requires permissionDecision allow, which "
            "would bypass the host permission gate for a command Protocol "
            "v2 cannot attest; passing the original command through "
            "(TOKENLESS_WORKBUDDY_AUTO_ALLOW=1 opts into the bypass)."
        )
        skip()
    return data.get("tool_name", ""), None


def pre_tool_output(arguments: dict) -> dict:
    # Reaching this point means the user opted into the bypass with
    # TOKENLESS_WORKBUDDY_AUTO_ALLOW=1 (see the gate in pre_tool_input);
    # Protocol v2 never reports rtk's verdict, so the reason records the
    # opt-in. ``modifiedInput`` is the partial field override, beside the
    # ``tool_input`` partial patch and the ``updatedInput`` full
    # replacement the other hosts read.
    return {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "tool_input": {"command": arguments["command"]},
            "updatedInput": arguments,
            "modifiedInput": {"command": arguments["command"]},
            "permissionDecision": "allow",
            "permissionDecisionReason": (
                "Tokenless: rtk rewrite auto-allowed via "
                "TOKENLESS_WORKBUDDY_AUTO_ALLOW (host confirmation bypassed)"
            ),
        }
    }


def diagnostic_output(message: str) -> dict:
    if not message:
        return {}
    return {
        "suppressOutput": True,
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": message,
        },
    }


def response(data: dict) -> ToolResult:
    tool = data.get("tool_name", "unknown")
    raw = data.get("tool_response", "")
    value = raw
    if isinstance(value, str) and is_skill_file(value):
        skip()
    envelope = value
    minimum = 200
    field = None
    if tool in SHELL_TOOLS:
        if (
            tool == "Bash"
            and isinstance(envelope, dict)
            and isinstance(envelope.get("stdout"), str)
            and envelope["stdout"].startswith("diff --git ")
        ):
            field = ("stdout", envelope["stdout"])
        selected = select_text_field(envelope, ("stdout", "stderr"), minimum)
        if selected is not None and (field is None or len(selected[1]) > len(field[1])):
            field = selected
    arguments = data.get("tool_input", {})
    origin = content_origin(tool)
    # The CodeBuddy Code CLI contract (v1.16.0+) defines updatedToolOutput;
    # IDE / Enterprise / unknown workbuddy hosts document only the additive
    # additionalContext, so they declare no replacement and stay on the
    # Core-owned passthrough path. Host classification is multi-signal and
    # every signal fails safe to the non-CLI path (see the module doc).
    can_replace = _workbuddy_cli_host()
    if not can_replace:
        warn(
            "WorkBuddy host is not a standalone CodeBuddy Code CLI: "
            "updatedToolOutput is not part of its PostToolUse contract, "
            "response compression disabled."
        )
    command = (
        arguments.get("command")
        if origin == "command_output" and isinstance(arguments, dict)
        else None
    )
    status = result_status(
        data.get("status", ""),
        data.get("is_error") is True
        or (isinstance(raw, dict) and raw.get("isError") is True),
        process_result(value) if tool in SHELL_TOOLS else None,
    )
    return ToolResult(
        value,
        envelope,
        field,
        origin,
        command,
        status,
        can_replace,
        field is not None or not isinstance(raw, (dict, list)),
    )


def response_output(data: dict, result: ToolResult, output: str, message: str) -> dict:
    updated = result.replacement(output)
    if updated is None:
        warn("tokenless compress returned non-JSON for a structured slot.")
        return diagnostic_output(message)
    specific = {"hookEventName": "PostToolUse", "updatedToolOutput": updated}
    if message:
        specific["additionalContext"] = message
    return {"suppressOutput": True, "hookSpecificOutput": specific}
