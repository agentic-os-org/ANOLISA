"""Shared utilities for tokenless Python hooks."""

from __future__ import annotations

import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path

# -- Binary fallback paths ----------------------------------------------------
#
# Tokenless has three supported installers whose binary directories differ:
#
# - Makefile:     ~/.local/bin or /usr/bin
# - Anolisa CLI:  ~/.local/bin or /usr/local/bin
# - RPM:          /usr/bin
#
# Callers keep passing the legacy lib/share paths as explicit fallbacks until
# installations made by older releases have aged out.
#
# KEEP IN SYNC with tool_ready_hook.sh::resolve_binary,
# env_check.rs::binary_fallback_paths, OpenClaw's fallback constants, and the
# Codex standalone scripts. Makefile and the Anolisa component manifest define
# the supported layouts; the canonical order is user, /usr/local, then /usr.

_USER_HOME = os.path.expanduser("~")
if not _USER_HOME or not os.path.isabs(_USER_HOME):
    _USER_HOME = ""


def _user_path(*parts: str) -> str:
    return os.path.join(_USER_HOME, *parts) if _USER_HOME else ""


_TOKENLESS_FALLBACK = "/usr/bin/tokenless"
_TOKENLESS_LOCAL_SHARE = _user_path(".local", "share", "anolisa", "tokenless", "tokenless")
_TOKENLESS_LOCAL_LIB = _user_path(".local", "lib", "anolisa", "tokenless", "tokenless")


def _known_binary_paths(name: str, home: str | None = None) -> tuple[str, ...]:
    """Return install-layout fallbacks for a binary outside ``PATH``."""
    if not name or os.path.basename(name) != name or name in {".", ".."}:
        return ()
    home = os.path.expanduser("~") if home is None else home
    user_home = home if home and os.path.isabs(home) else None
    paths = []
    if user_home:
        paths.append(os.path.join(user_home, ".local", "bin", name))
    paths.append(os.path.join("/usr/local/bin", name))
    paths.append(os.path.join("/usr/bin", name))
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
    "PowerShell",
    "powershell",
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
    "RunCommand",
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


def _bare_retrieve_hash(command: str) -> str | None:
    if not _TOKENLESS_RETRIEVE_COMMAND_RE.fullmatch(command):
        return None
    return re.search(r"[0-9a-fA-F]{24}", shlex.split(command)[2]).group().lower()


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


# -- Hook state directory --

_OPTIMIZATION_STATE_DIR = os.path.join(os.path.expanduser("~"), ".tokenless", "hook-state")
_OPTIMIZATION_STATE_TTL_SECONDS = 24 * 60 * 60
# This bound is shared by all sessions using the same state directory. Under
# pressure an in-flight marker can be evicted, losing PostTool deduplication.
_OPTIMIZATION_STATE_MAX_FILES = 1024

# -- Binary resolution (cached) -----------------------------------------------

_resolved_cache: dict[tuple, str | None] = {}


def _windows_profile_directory() -> str:
    """Resolve the process token's profile, as tokenless-stats/home.rs does."""
    import ctypes
    from ctypes import wintypes

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    security = ctypes.WinDLL("advapi32", use_last_error=True)
    profiles = ctypes.WinDLL("userenv", use_last_error=True)
    kernel.GetCurrentProcess.argtypes = []
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.CloseHandle.restype = wintypes.BOOL
    security.OpenProcessToken.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.HANDLE),
    ]
    security.OpenProcessToken.restype = wintypes.BOOL
    profiles.GetUserProfileDirectoryW.argtypes = [
        wintypes.HANDLE,
        wintypes.LPWSTR,
        ctypes.POINTER(wintypes.DWORD),
    ]
    profiles.GetUserProfileDirectoryW.restype = wintypes.BOOL
    token = wintypes.HANDLE()
    if not security.OpenProcessToken(kernel.GetCurrentProcess(), 0x0008, ctypes.byref(token)):
        raise OSError("cannot query the current process token")
    try:
        size = wintypes.DWORD()
        profiles.GetUserProfileDirectoryW(token, None, ctypes.byref(size))
        if not size.value:
            raise OSError("no profile directory for the current process token")
        buffer = ctypes.create_unicode_buffer(size.value)
        if not profiles.GetUserProfileDirectoryW(token, buffer, ctypes.byref(size)):
            raise OSError("cannot resolve the current process profile directory")
        return buffer.value
    finally:
        kernel.CloseHandle(token)


def resolve_data_directory() -> str:
    """Resolve runtime state without writes or environment-based home fallbacks.

    Keep the default and validation aligned with tokenless-stats/home.rs and
    path_policy.rs, which the CLI and patched RTK also share.
    """
    configured = os.environ.get("TOKENLESS_DATA_DIR")
    if configured:
        path = Path(configured)
    else:
        try:
            if os.name == "nt":
                home = _windows_profile_directory()
            else:
                import pwd

                home = pwd.getpwuid(os.getuid()).pw_dir
        except (ImportError, KeyError, OSError) as error:
            raise ValueError("no trusted home directory; set TOKENLESS_DATA_DIR") from error
        anchor = Path(home)
        if not home or not anchor.is_absolute() or anchor == anchor.parent:
            raise ValueError("no trusted home directory; set TOKENLESS_DATA_DIR")
        path = anchor / ".tokenless"

    if not path.is_absolute():
        raise ValueError("TOKENLESS_DATA_DIR must be an absolute path")
    if ".." in path.parts:
        raise ValueError("TOKENLESS_DATA_DIR must not contain parent traversal")
    # Resolve existing prefixes strictly: dangling links, loops and inaccessible
    # ancestors must not silently select a different directory from the CLI.
    ancestor = path
    missing = []
    while True:
        try:
            ancestor.lstat()
            break
        except FileNotFoundError:
            # Missing drives and UNC shares have no existing ancestor to find.
            if ancestor == ancestor.parent:
                raise
            missing.append(ancestor.name)
            ancestor = ancestor.parent
    if not ancestor.is_dir():
        raise ValueError("TOKENLESS_DATA_DIR must resolve to a directory")
    normalized = ancestor.resolve(strict=True).joinpath(*reversed(missing))
    if normalized == normalized.parent:
        raise ValueError("TOKENLESS_DATA_DIR must not be the filesystem root")
    return str(normalized)


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
    command: str | None = None,
) -> dict:
    """Build a Protocol v2 PostTool transport request.

    ``command`` is the shell command line behind a ``command_output`` result;
    Core reports a plain print of local files as ``file_read``.
    """
    request = {
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
    if command is not None:
        request["input"]["command"] = command
    return request


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


def parse_version(version_str: str) -> tuple | None:
    """Parse a version string like '0.35.0' into a (major, minor, patch) tuple."""
    m = re.search(r"(\d+)\.(\d+)\.(\d+)", version_str)
    if m:
        return (int(m.group(1)), int(m.group(2)), int(m.group(3)))
    return None
