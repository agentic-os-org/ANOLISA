"""Shared utilities for tokenless Python hooks."""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time

# -- Binary fallback paths ----------------------------------------------------
#
# Tokenless has three supported installers whose layouts differ:
#
# - Makefile:     ~/.local/{bin,libexec} or /usr/{bin,libexec}
# - Anolisa CLI:  ~/.local/{bin,lib/anolisa/libexec} or /usr/local/{bin,libexec}
# - RPM:          /usr/{bin,libexec}
#
# Keep the legacy lib/share paths until installations made by older releases
# have aged out.
#
# KEEP IN SYNC with tool_ready_hook.sh::resolve_binary,
# env_check.rs::binary_fallback_paths, OpenClaw's fallback constants, and the
# Codex standalone scripts. Makefile and the Anolisa component manifest define
# the supported layouts; the canonical order is user, /usr/local, /usr, legacy.

_USER_HOME = os.path.expanduser("~")
if not _USER_HOME or not os.path.isabs(_USER_HOME):
    _USER_HOME = ""


def _user_path(*parts: str) -> str:
    return os.path.join(_USER_HOME, *parts) if _USER_HOME else ""


_TOKENLESS_FALLBACK = "/usr/bin/tokenless"
_TOKENLESS_LOCAL_SHARE = _user_path(".local", "share", "anolisa", "tokenless", "tokenless")
_TOKENLESS_LOCAL_LIB = _user_path(".local", "lib", "anolisa", "tokenless", "tokenless")
_RTK_FALLBACK = "/usr/libexec/anolisa/tokenless/rtk"
_RTK_LOCAL_SHARE = _user_path(".local", "share", "anolisa", "tokenless", "rtk")
_RTK_LOCAL_LIB = _user_path(".local", "lib", "anolisa", "tokenless", "rtk")

_TOKENLESS_HELPER_BINARIES = frozenset({"rtk"})


def _known_binary_paths(name: str, home: str | None = None) -> tuple[str, ...]:
    """Return install-layout fallbacks for a binary outside ``PATH``."""
    if not name or os.path.basename(name) != name or name in {".", ".."}:
        return ()
    home = os.path.expanduser("~") if home is None else home
    user_home = home if home and os.path.isabs(home) else None
    paths = []
    if user_home:
        paths.append(os.path.join(user_home, ".local", "bin", name))
    if name in _TOKENLESS_HELPER_BINARIES:
        if user_home:
            paths.extend(
                [
                    # Anolisa CLI user mode.
                    os.path.join(
                        user_home,
                        ".local",
                        "lib",
                        "anolisa",
                        "libexec",
                        "tokenless",
                        name,
                    ),
                    # Makefile user mode.
                    os.path.join(
                        user_home,
                        ".local",
                        "libexec",
                        "anolisa",
                        "tokenless",
                        name,
                    ),
                ]
            )
    paths.append(os.path.join("/usr/local/bin", name))
    if name in _TOKENLESS_HELPER_BINARIES:
        # Anolisa CLI system mode.
        paths.append(os.path.join("/usr/local/libexec/anolisa/tokenless", name))
    paths.append(os.path.join("/usr/bin", name))
    if name in _TOKENLESS_HELPER_BINARIES:
        paths.extend(
            [
                # Makefile system mode and RPM.
                os.path.join("/usr/libexec/anolisa/tokenless", name),
                # Debian and pre-layout-migration compatibility.
                os.path.join("/usr/lib/anolisa/tokenless", name),
            ]
        )
        if user_home:
            paths.extend(
                [
                    os.path.join(user_home, ".local", "share", "anolisa", "tokenless", name),
                    os.path.join(user_home, ".local", "lib", "anolisa", "tokenless", name),
                ]
            )
    return tuple(paths)


# -- Unified tool categorization ----------------------------------------------

# Tool categories are loaded from tool_categories.json, which serves as the
# single source of truth for both tool-ready and compression strategies.
# This fixes inconsistencies like Grep being classified as Shell in some places
# and Read in others.

_TOOL_CATEGORIES_PATH = os.path.join(os.path.dirname(__file__), "tool_categories.json")

# Hardcoded fallback sets — used only when tool_categories.json is missing or
# invalid. Matches the minimum safe classification from before the JSON was
# introduced, ensuring content-retrieval tools are never accidentally compressed.
_FALLBACK_SKIP_TOOLS = [
    "Read",
    "read",
    "read_file",
    "read_many_files",
    "Glob",
    "glob",
    "search_file",
    "list_directory",
    "list_dir",
    "Grep",
    "grep",
    "grep_code",
    "grep_search",
    "search_files",
    "Lsp",
    "lsp",
    "NotebookRead",
    "notebook_read",
    "notebookread",
]
_FALLBACK_SHELL_TOOLS = [
    "Bash",
    "bash",
    "Shell",
    "shell",
    "exec",
    "terminal",
    "run_shell_command",
    "run_in_terminal",
    "get_terminal_output",
    "execute_command",
    "process",
]


def _load_tool_categories() -> dict:
    """Load tool categories from unified JSON file with validation."""
    try:
        with open(_TOOL_CATEGORIES_PATH, "r") as f:
            data = json.load(f)

        # Validate required structure
        required_layers = ["layer_1_skip", "layer_2_shell", "layer_3_api"]
        for layer in required_layers:
            if layer not in data:
                raise ValueError(f"Missing required layer: {layer}")
            if not isinstance(data[layer], dict):
                raise ValueError(f"Layer {layer} must be a dict")

        # layer_1 and layer_2 require a "tools" list; layer_3 is implicit
        for layer in ("layer_1_skip", "layer_2_shell"):
            if "tools" not in data[layer]:
                raise ValueError(f"Layer {layer} missing 'tools' field")
            if not isinstance(data[layer]["tools"], list):
                raise ValueError(f"Layer {layer}.tools must be a list")

        return data
    except (FileNotFoundError, json.JSONDecodeError, ValueError) as e:
        print(f"Warning: Failed to load tool_categories.json: {e}", file=sys.stderr)
        # Fallback to hardcoded safe sets so content-retrieval tools are
        # never accidentally compressed even if the JSON is unavailable.
        return {
            "layer_1_skip": {"tools": list(_FALLBACK_SKIP_TOOLS)},
            "layer_2_shell": {"tools": list(_FALLBACK_SHELL_TOOLS)},
            "layer_3_api": {},
        }


_tool_categories = _load_tool_categories()

# 3-layer Compression strategy:
#   Layer 1: Content retrieval (Read/Glob/Grep) → skip all compression
#   Layer 2: Shell/exec (Bash/Shell/exec) → moderate truncation (64K strings)
#   Layer 3: API/structured (all other) → zero-truncation cleanup (1M strings)

# Layer 1: Content retrieval tools — skip all compression (preserve integrity).
# These tools return file content or search results that should not be truncated.
SKIP_TOOLS: set[str] = set(_tool_categories.get("layer_1_skip", {}).get("tools", []))

# Layer 2: Shell/exec tools (moderate truncation).
# These tools produce text output that can be safely truncated if too long.
SHELL_TOOLS: set[str] = set(_tool_categories.get("layer_2_shell", {}).get("tools", []))

_TOKENLESS_RETRIEVE_COMMAND_RE = re.compile(
    r"^[ \t]*(?:\"tokenless\"|'tokenless'|tokenless)[ \t]+retrieve[ \t]+"
    r"(?:\"(?:[0-9a-f]{24}|<<tokenless:[0-9a-f]{24}>>)\"|"
    r"'(?:[0-9a-f]{24}|<<tokenless:[0-9a-f]{24}>>)'|[0-9a-f]{24})[ \t]*$",
    re.IGNORECASE,
)


def tokenless_retrieve_command_available() -> bool:
    """Return whether a Marker command can invoke bare ``tokenless``."""
    return shutil.which("tokenless") is not None


def is_tokenless_retrieve_command(tool_name: str, arguments: object) -> bool:
    """Recognize the exact local recovery command emitted by Tokenless markers."""
    if tool_name not in SHELL_TOOLS or not isinstance(arguments, dict):
        return False
    command = arguments.get("command")
    if not isinstance(command, str):
        return False
    return _TOKENLESS_RETRIEVE_COMMAND_RE.fullmatch(command) is not None


# Layer 3: API tools (zero-truncation).
# These tools return structured data or API responses that should not be truncated.
# No explicit set needed; tools not in SKIP_TOOLS or SHELL_TOOLS are Layer 3.

# Thresholds are read from tool_categories.json (single source of truth).
# Hardcoded fallbacks match the JSON defaults; used only if the JSON field
# is missing or the file failed to load.

# Layer 2 thresholds: moderate truncation for shell/exec output.
# Restores the old JSON compression defaults for shell commands (git log, ls,
# cat, etc.) where truncation is acceptable.
# 64K strings: 95% of real shell output (git diff ~63K, git log ~34K) preserved.
# 128 arrays: 95% of result sets (test results, audit reports) preserved.
_layer2_thr = _tool_categories.get("layer_2_shell", {}).get("thresholds", {})
_SHELL_TRUNCATE_STRINGS_AT = _layer2_thr.get("truncate_strings_at", 65_536)
_SHELL_TRUNCATE_ARRAYS_AT = _layer2_thr.get("truncate_arrays_at", 128)
_SHELL_MAX_DEPTH = _layer2_thr.get("max_depth", 8)

# Layer 3 thresholds: zero-truncation for API/structured data.
_layer3_thr = _tool_categories.get("layer_3_api", {}).get("thresholds", {})
_TRUNCATE_STRINGS_AT = _layer3_thr.get("truncate_strings_at", 1_048_576)
_TRUNCATE_ARRAYS_AT = _layer3_thr.get("truncate_arrays_at", 65_536)
_MAX_DEPTH = _layer3_thr.get("max_depth", 32)


def get_thresholds(tool_name: str) -> tuple[int, int, int]:
    """Return (truncate_strings_at, truncate_arrays_at, max_depth) for a tool.

    Layer 2 (shell/exec) tools use moderate truncation; all others use
    Layer 3 zero-truncation thresholds. Single dispatch point used by all
    adapters (codex, hermes, openclaw, compress_response_hook).
    """
    if tool_name in SHELL_TOOLS:
        return (_SHELL_TRUNCATE_STRINGS_AT, _SHELL_TRUNCATE_ARRAYS_AT, _SHELL_MAX_DEPTH)
    return (_TRUNCATE_STRINGS_AT, _TRUNCATE_ARRAYS_AT, _MAX_DEPTH)


# -- Shared environment error patterns ----------------------------------------
#
# Superset of patterns from both the Codex diagnostics and shared hook adapters. Uses regex
# matching (case-insensitive) so patterns like "/bin/sh:.*: not found" work
# correctly. Both codex/scripts/response-diagnostics and compress_response_hook
# import this list and the classify_env_error() function below.

ENV_PATTERNS: list[tuple[list[str], str, str]] = [
    (
        [
            "command not found",
            "not installed",
            "which: no",
            r"No command\s",
            "cannot execute",
            "is not recognized",
            "Could not find",
            "unable to locate",
            "Package not found",
            r"/bin/sh:.*: not found",
            "command not found:",
        ],
        "ENV_DEPENDENCY_MISSING",
        "Missing dependency detected. Install it or ask the user for guidance.",
    ),
    (
        [
            "Permission denied",
            "permission denied",
            "Operation not permitted",
            "EACCES",
            "Access denied",
            r"cannot open .* for writing",
        ],
        "ENV_PERMISSION",
        "Permission denied. Check file/directory permissions or run with appropriate access.",
    ),
    (
        [
            "No such file or directory",
            "ENOENT",
            "cannot find",
            "File not found",
            "does not exist",
        ],
        "ENV_FILE_MISSING",
        "Required file or directory not found. Verify the path or create it.",
    ),
    (
        [
            "Connection refused",
            "Could not resolve host",
            "Network is unreachable",
            r"curl: \(7\)",
            r"curl: \(6\)",
            "Failed to connect",
            "Name or service not known",
            "Couldn't resolve host",
            "Temporary failure in name resolution",
            "ECONNREFUSED",
            "ETIMEDOUT",
            "Connection timed out",
        ],
        "ENV_NETWORK",
        "Network connectivity issue. Check DNS, proxy, and firewall settings.",
    ),
    (
        [
            "ModuleNotFoundError",
            "ImportError",
            "No module named",
            "cannot import name",
            "npm ERR! 404",
        ],
        "ENV_PACKAGE_MISSING",
        "Required package or module is missing. Install the needed dependency.",
    ),
]


def classify_env_error(tool_response) -> tuple[str | None, str | None]:
    """Detect environment errors in tool output.

    Accepts either a parsed dict (with stderr/error/exit_code fields) or a
    plain string. Returns (category_tag, fix_hint) or (None, None).

    Shared by codex/scripts/response-diagnostics and compress_response_hook.
    """
    if isinstance(tool_response, dict):
        text = str(tool_response.get("stderr", "")) + str(tool_response.get("error", ""))
        # Use `is None` — `or` would treat exit_code=0 (success) as falsy and
        # incorrectly fall through to exitCode.
        exit_code = tool_response.get("exit_code")
        if exit_code is None:
            exit_code = tool_response.get("exitCode")
        if exit_code is not None and exit_code == 0 and not text:
            return None, None
    elif isinstance(tool_response, str):
        text = tool_response
    else:
        return None, None

    if not text:
        return None, None

    for patterns, category, hint in ENV_PATTERNS:
        for pat in patterns:
            if re.search(pat, text, re.IGNORECASE):
                return category, hint

    return None, None


# -- Context file for rewrite session tracking --

_CONTEXT_DIR = os.path.join(os.path.expanduser("~"), ".tokenless")
_CONTEXT_FILE = os.path.join(_CONTEXT_DIR, ".rewrite-context")
_OPTIMIZATION_STATE_DIR = os.path.join(_CONTEXT_DIR, "hook-state")
_OPTIMIZATION_STATE_TTL_SECONDS = 24 * 60 * 60
_OPTIMIZATION_STATE_MAX_FILES = 1024

# -- Binary resolution (cached) -----------------------------------------------

_resolved_cache: dict[tuple, str | None] = {}


def resolve_binary(name: str, *fallback_paths: str) -> str | None:
    """Locate a binary by PATH search, install layouts, then explicit paths.

    Results are cached per name, explicit fallbacks, and home directory so
    callers with distinct install contexts get independent entries.
    """
    home = os.path.expanduser("~")
    cache_key = (name, fallback_paths, home)
    if cache_key in _resolved_cache:
        return _resolved_cache[cache_key]

    result: str | None = None
    path = shutil.which(name)
    if path:
        result = path
    else:
        candidates = dict.fromkeys((*_known_binary_paths(name, home), *fallback_paths))
        for fp in candidates:
            if fp and os.path.isfile(fp) and os.access(fp, os.X_OK):
                result = fp
                break

    _resolved_cache[cache_key] = result
    return result


def skip() -> None:
    print(json.dumps({}))
    sys.exit(0)


def skip_silent() -> None:
    """Exit silently with empty stdout (codex protocol: empty stdout = passthrough)."""
    sys.exit(0)


def warn(msg: str) -> None:
    print(f"[tokenless] WARNING: {msg}", file=sys.stderr)


def try_parse_json(data: str) -> object | None:
    try:
        return json.loads(data)
    except (json.JSONDecodeError, ValueError):
        return None


def unwrap_string_json(raw: str) -> str | None:
    """If raw is a JSON-encoded string whose inner content is valid JSON,
    unwrap it into the inner JSON string. Returns None for plain text."""
    if not raw.startswith('"'):
        return raw
    inner = try_parse_json(raw)
    if isinstance(inner, str):
        inner_obj = try_parse_json(inner)
        if inner_obj is not None and isinstance(inner_obj, (dict, list)):
            # ensure_ascii=False: downstream size gates count Unicode
            # characters (code points), not \uXXXX escape sequences, so
            # string-wrapped payloads are measured the same way as the
            # dict/list branch and the OpenClaw adapter.
            return json.dumps(inner_obj, separators=(",", ":"), ensure_ascii=False)
        return None
    return raw


def is_skill_file(text: str) -> bool:
    """Detect YAML frontmatter markdown (skill files) that must not be compressed."""
    if not text.startswith("---"):
        return False
    lines = text.split("\n", 20)
    for line in lines[1:]:
        if line.startswith("name:") or line.startswith("description:"):
            return True
    return False


def resolve_tool_call_id(agent_id: str, input_data: dict) -> str:
    """Resolve the tool call identifier to record for a hook invocation.

    Qwen Code and Kimi Code's serialized hook input carry two identifiers:
    the internal ``tool_use_id`` (``toolu_<ts>_<rand>``, always present) and
    ``tool_call_id`` (the LLM provider's original call ID, e.g. ``call_xxx``,
    snake_case — Qwen Code's TS uses camelCase ``toolCallId`` internally but
    normalizes to ``tool_call_id`` on the wire). For qwencode and kimicode
    agents, prefer the provider call ID and fall back to the internal one;
    for other agents, keep the existing priority (``tool_use_id`` first).
    """
    if agent_id in ("qwencode", "kimicode"):
        return (
            input_data.get("tool_call_id")
            or input_data.get("toolCallId")
            or input_data.get("tool_use_id", "")
        )
    return input_data.get("tool_use_id") or input_data.get("toolCallId", "")


def secure_write_text(path: str, content: str) -> None:
    """Write a private hook state file (0o600, symlink-safe).

    Shared hardening for state files under ~/.tokenless: the parent directory
    is created 0o700, symlinks are refused (unlink + O_NOFOLLOW) and the file
    stays owner-readable only, so hook state never leaks through a shared or
    mounted HOME.
    """
    os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
    if os.path.islink(path):
        os.unlink(path)
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    fd = os.open(path, flags, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(content)


def write_context(agent_id: str, session_id: str, tool_use_id: str) -> None:
    """Write context file for rtk rewrite session tracking."""
    secure_write_text(_CONTEXT_FILE, f"{agent_id}\n{session_id}\n{tool_use_id}\n")


def _optimization_state_path(agent_id: str, session_id: str, tool_use_id: str) -> str:
    identity = "\0".join((agent_id, session_id, tool_use_id)).encode()
    digest = hashlib.sha256(identity).hexdigest()
    return os.path.join(_OPTIMIZATION_STATE_DIR, digest)


def _prune_optimization_states() -> None:
    """Bound abandoned per-call state when a host omits PostToolUse."""
    try:
        entries = os.scandir(_OPTIMIZATION_STATE_DIR)
    except FileNotFoundError:
        return

    cutoff = time.time() - _OPTIMIZATION_STATE_TTL_SECONDS
    live = []
    with entries:
        for entry in entries:
            try:
                if not entry.is_file(follow_symlinks=False):
                    continue
                modified = entry.stat(follow_symlinks=False).st_mtime
                if ".consuming." in entry.name and modified > cutoff:
                    continue
                if modified <= cutoff:
                    os.unlink(entry.path)
                else:
                    live.append((modified, entry.path))
            except FileNotFoundError:
                continue

    excess = len(live) - _OPTIMIZATION_STATE_MAX_FILES + 1
    if excess > 0:
        for _, path in sorted(live)[:excess]:
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass


def mark_rtk_optimized(agent_id: str, session_id: str, tool_use_id: str) -> None:
    """Persist RTK ownership for one tool call before applying its rewrite."""
    _prune_optimization_states()
    secure_write_text(_optimization_state_path(agent_id, session_id, tool_use_id), "rtk\n")


def consume_output_optimization(agent_id: str, session_id: str, tool_use_id: str) -> str:
    """Consume one tool call's optimization state for its final result."""
    if not tool_use_id:
        return "none"
    path = _optimization_state_path(agent_id, session_id, tool_use_id)
    consuming_path = f"{path}.consuming.{os.getpid()}"
    try:
        os.rename(path, consuming_path)
    except FileNotFoundError:
        return "none"

    try:
        flags = os.O_RDONLY
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        fd = os.open(consuming_path, flags)
        with os.fdopen(fd) as state_file:
            state = state_file.read()
    finally:
        try:
            os.unlink(consuming_path)
        except FileNotFoundError:
            pass
    return "rtk" if state == "rtk\n" else "none"


def forward_stderr(proc: subprocess.CompletedProcess) -> None:
    """Forward subprocess stderr on failure (non-zero exit) via warn()."""
    if proc.returncode != 0 and proc.stderr:
        warn(proc.stderr.rstrip())


def run(args: list[str], input_data: str, timeout: int = 3) -> subprocess.CompletedProcess | None:
    """Run a subprocess with input data, returning None on failure."""
    try:
        proc = subprocess.run(
            args,
            input=input_data,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        forward_stderr(proc)
        return proc
    except Exception:
        return None


def _attribution(
    agent_id: str,
    session_id: str = "",
    tool_use_id: str = "",
) -> dict:
    """Build the shared Protocol v2 attribution object."""
    value = {"agent_id": agent_id}
    if session_id:
        value["session_id"] = session_id
    if tool_use_id:
        value["tool_use_id"] = tool_use_id
    return value


def build_before_model_request(
    tools: list,
    visible_context: object,
    agent_id: str,
    session_id: str = "",
    tool_use_id: str = "",
) -> dict:
    """Build a Protocol v2 BeforeModel transport request."""
    return {
        "protocol_version": 2,
        "operation": "before_model",
        "attribution": _attribution(agent_id, session_id, tool_use_id),
        "input": {
            "tools": tools,
            "visible_context": visible_context,
            "capabilities": {
                "replace_tools": True,
                # A local CLI is not marker-scoped Agent authorization.
                "recovery": {"kind": "none"},
            },
        },
    }


def build_pre_tool_request(
    arguments: dict,
    agent_id: str,
    tool_name: str,
    command_field: str,
    session_id: str = "",
    tool_use_id: str = "",
    replace_arguments: bool = True,
    block_and_suggest: bool = False,
) -> dict:
    """Build a Protocol v2 PreTool transport request."""
    return {
        "protocol_version": 2,
        "operation": "pre_tool",
        "attribution": _attribution(agent_id, session_id, tool_use_id),
        "input": {
            "tool_name": tool_name,
            "arguments": arguments,
            "command_field": command_field,
            "capabilities": {
                "replace_arguments": replace_arguments,
                "block_and_suggest": block_and_suggest,
            },
        },
    }


def build_post_tool_request(
    content: str,
    agent_id: str,
    tool_name: str,
    status: str,
    content_origin: str,
    output_optimization: str,
    *,
    result_kind: str,
    recovery: dict[str, str],
    session_id: str = "",
    tool_use_id: str = "",
    replace_output: bool = False,
    replace_with_text: bool = False,
) -> dict:
    """Build a Protocol v2 PostTool transport request."""
    return {
        "protocol_version": 2,
        "operation": "post_tool",
        "attribution": _attribution(agent_id, session_id, tool_use_id),
        "input": {
            "result_kind": result_kind,
            "tool_name": tool_name,
            "content": content,
            "status": status,
            "content_origin": content_origin,
            "output_optimization": output_optimization,
            "capabilities": {
                "replace_output": replace_output,
                "recovery": recovery,
                "replace_with_text": replace_with_text,
            },
        },
    }


def run_compress(
    tokenless_bin: str, request: dict, timeout: int, expected_operation: str
) -> dict | None:
    """Run ``tokenless compress`` on one request; None on any failure.

    The single Tokenless subprocess of a hook invocation (roadmap §5.6).
    Fail-open: a dead binary, non-zero exit, or malformed stdout all
    return None so the caller passes the original through.
    """
    proc = run(
        [tokenless_bin, "compress"],
        json.dumps(request, ensure_ascii=False),
        timeout=timeout,
    )
    if proc is None or proc.returncode != 0 or not proc.stdout.strip():
        if proc is not None and proc.returncode != 0:
            warn(f"tokenless compress exited {proc.returncode}")
        return None
    response = try_parse_json(proc.stdout.strip())
    if not isinstance(response, dict):
        warn("tokenless compress returned malformed output")
        return None
    if response.get("protocol_version") != 2:
        # Version-skewed binary: never trust a response whose contract
        # this adapter does not speak.
        warn("tokenless compress returned an unsupported protocol version")
        return None
    if response.get("operation") != expected_operation:
        warn("tokenless compress returned a mismatched operation")
        return None
    result = response.get("result")
    if not isinstance(result, dict):
        warn("tokenless compress returned a malformed operation result")
        return None
    return result


def detect_cosh_ng_runtime() -> tuple | None:
    """Detect if we are running under Cosh-NG and return its version.

    Returns:
        A (major, minor, patch) version tuple if Cosh-NG is detected,
        or (0, 0, 0) if Cosh-NG is detected but version is unknown
        (unsupported), or None if not running under Cosh-NG.

    Detection checks (in order):
      1. ``COSH_NG_VERSION`` environment variable — set by Cosh-NG when
         launching hook processes.
      2. ``COSH_RUNTIME`` environment variable set to ``cosh-ng``.

    The ``(0, 0, 0)`` sentinel means "Cosh-NG detected but version unknown" —
    callers should treat this as "unsupported version" and fail open (disable
    compression) rather than falling back to duplicate injection.
    """
    version_str = os.environ.get("COSH_NG_VERSION", "")
    if version_str:
        ver = parse_version(version_str)
        if ver:
            return ver
        # Detected Cosh-NG but can't parse version — unsupported
        return (0, 0, 0)

    runtime = os.environ.get("COSH_RUNTIME", "")
    if runtime == "cosh-ng":
        # Cosh-NG detected via runtime env var but no version available
        return (0, 0, 0)

    return None


def _agent_id_from_argv(argv: list[str] | None) -> str | None:
    """Return an explicit ``--agent-id`` value from hook command arguments.

    Adapters declare the agent id in ``hooks.json``. The host always honours
    the hook ``command`` string, but does not reliably propagate the declared
    ``env`` map to every hook execution context — PreToolUse rewrite and
    sub-agent task runs have been observed dropping it, collapsing attribution
    to the fallback. An id passed as a command argument is therefore the robust
    channel. Accepts both ``--agent-id VALUE`` and ``--agent-id=VALUE``;
    unrelated arguments are ignored so hooks that take no other options are
    unaffected.
    """
    args = sys.argv[1:] if argv is None else argv
    for index, arg in enumerate(args):
        if arg == "--agent-id":
            following = args[index + 1] if index + 1 < len(args) else ""
            return following or None
        if arg.startswith("--agent-id="):
            return arg[len("--agent-id=") :] or None
    return None


def resolve_agent_id(default: str = "unknown", argv: list[str] | None = None) -> str:
    """Resolve the agent ID used for stats attribution.

    Precedence, highest first:

    1. **Cosh-NG runtime detection.** The shared extension manifest sets
       ``TOKENLESS_AGENT_ID=copilot-shell``, and since Cosh-NG honours hook
       ``env`` (#1617) an unconditional read would attribute Cosh-NG sessions
       to copilot-shell. ``COSH_RUNTIME`` / ``COSH_NG_VERSION`` are injected by
       the host after the manifest env, so they win.
    2. **``--agent-id`` command argument** — the robust declared channel, since
       the host always honours the hook ``command`` string.
    3. **``TOKENLESS_AGENT_ID`` environment variable** — the legacy declared
       channel, kept for backward compatibility but not always propagated.
    4. **``default``** — a visible ``"unknown"`` sentinel rather than the tool's
       own name, so an attribution gap is observable in stats instead of
       masquerading as a real ``tokenless`` agent.

    This is attribution only. The signal is cooperative — a manifest controls
    its own ``command`` and env and could reassign these — so it must never
    gate anything security-relevant.
    """
    if detect_cosh_ng_runtime() is not None:
        return "cosh-ng"
    return _agent_id_from_argv(argv) or os.environ.get("TOKENLESS_AGENT_ID") or default


def parse_version(version_str: str) -> tuple | None:
    """Parse a version string like '0.35.0' into a (major, minor, patch) tuple."""
    m = re.search(r"(\d+)\.(\d+)\.(\d+)", version_str)
    if m:
        return (int(m.group(1)), int(m.group(2)), int(m.group(3)))
    return None
