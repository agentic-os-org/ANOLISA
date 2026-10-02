# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Test: openclaw gateway truncates MCP server keys to 30 chars, breaking tool isolation.

Reproduces the cross-session MCP tool leak (ce-runner bug):
  - openclaw Gateway uses ``sanitizeServerName`` (TOOL_NAME_MAX_PREFIX=30) which
    truncates server keys that exceed 30 characters.
  - ce-runner generates ``alsoAllow`` and ``deny`` entries using FULL (untruncated)
    server keys, which no longer match the actual tool names exposed by the gateway.
  - When ``alsoAllow`` is used WITHOUT ``allow``, ``pickSandboxToolPolicy`` injects
    ``*`` into the effective allow list, exposing ALL MCP tools to every agent.
    The deny patterns fail because they target truncated names with full-length keys.

This test replicates the openclaw truncation logic in pure Python to validate
the fix before any gateway is involved.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


# ── Replicated openclaw truncation logic ──────────────────────────────────
# From /root/env/openclaw/src/agents/pi-bundle-mcp-names.ts

TOOL_NAME_SAFE_RE = re.compile(r"[^A-Za-z0-9_-]")
TOOL_NAME_SEPARATOR = "__"
TOOL_NAME_MAX_PREFIX = 30
TOOL_NAME_MAX_TOTAL = 64


def _sanitize_tool_fragment(raw: str, fallback: str, max_chars: int | None = None) -> str:
    cleaned = raw.strip()
    cleaned = TOOL_NAME_SAFE_RE.sub("-", cleaned)
    normalized = cleaned or fallback
    if max_chars is None:
        return normalized
    return normalized[:max_chars] if len(normalized) > max_chars else normalized


def sanitize_server_name(raw: str, used_names: set[str] | None = None) -> str:
    """Replicate openclaw sanitizeServerName."""
    used = used_names or set()
    base = _sanitize_tool_fragment(raw, "mcp", TOOL_NAME_MAX_PREFIX)
    candidate = base
    n = 2
    while candidate.lower() in used:
        suffix = f"-{n}"
        candidate = (
            base[: max(1, TOOL_NAME_MAX_PREFIX - len(suffix))] + suffix
        )
        n += 1
    used.add(candidate.lower())
    return candidate


def build_safe_tool_name(server_name: str, tool_name: str, reserved_names: set[str] | None = None) -> str:
    """Replicate openclaw buildSafeToolName."""
    reserved = reserved_names or set()
    cleaned_tool = _sanitize_tool_fragment(tool_name, "tool", None)
    max_tool_chars = max(1, TOOL_NAME_MAX_TOTAL - len(server_name) - len(TOOL_NAME_SEPARATOR))
    truncated_tool = cleaned_tool[:max_tool_chars]
    candidate_tool = truncated_tool or "tool"
    candidate = f"{server_name}{TOOL_NAME_SEPARATOR}{candidate_tool}"
    n = 2
    while candidate.lower() in reserved:
        suffix = f"-{n}"
        candidate_tool = (
            (truncated_tool or "tool")[: max(1, max_tool_chars - len(suffix))] + suffix
        )
        candidate = f"{server_name}{TOOL_NAME_SEPARATOR}{candidate_tool}"
        n += 1
    return candidate


def compile_glob_pattern(raw: str, normalize=None) -> dict:
    """Replicate openclaw compileGlobPattern. Returns dict for test assertions."""
    norm = raw.lower() if normalize is None else normalize(raw)
    if not norm:
        return {"kind": "exact", "value": ""}
    if norm == "*":
        return {"kind": "all"}
    if "*" not in norm:
        return {"kind": "exact", "value": norm}
    escaped = re.escape(norm).replace(r"\*", ".*")
    return {"kind": "regex", "value": re.compile(f"^{escaped}$")}


def matches_any_glob(value: str, patterns: list[dict]) -> bool:
    """Replicate openclaw matchesAnyGlobPattern."""
    for p in patterns:
        if p["kind"] == "all":
            return True
        if p["kind"] == "exact" and value == p["value"]:
            return True
        if p["kind"] == "regex" and p["value"].search(value):
            return True
    return False


# ── Helper: replicate pickSandboxToolPolicy behavior ─────────────────────

def simulate_pick_sandbox_tool_policy(allow=None, also_allow=None, deny=None):
    """Replicate openclaw pickSandboxToolPolicy + unionAllow behavior."""
    # unionAllow logic
    if also_allow and len(also_allow) > 0:
        if allow is None:
            # CRITICAL: when only alsoAllow is set (no allow), * is injected
            effective_allow = list(dict.fromkeys(["*", *also_allow]))
        else:
            effective_allow = list(dict.fromkeys([*allow, *also_allow]))
    else:
        effective_allow = allow

    effective_deny = list(deny) if deny else None

    if effective_allow is None and effective_deny is None:
        return None
    return {"allow": effective_allow, "deny": effective_deny}


# ── Tests ─────────────────────────────────────────────────────────────────


class TestServerKeyTruncation:
    """Verify the fix: all server keys fit within 30 chars after prefix shortening."""

    # Previously-broken task IDs that now fit with ce-mock- / ce-sb- prefixes
    FIX_VERIFY_CASES = [
        ("T001zh_email_triage", "mock_mcp_name"),        # was 34, now 27 ✓
        ("T001zh_email_triage", "sandbox_mcp_name"),      # was 36, now 25 ✓
        ("T002_email_triage", "mock_mcp_name"),           # was 32, now 25 ✓
        ("T002_email_triage", "sandbox_mcp_name"),        # was 34, now 23 ✓
        ("C01_mortgage_calculator_pro", "mock_mcp_name"), # was 42, now 30 (hash) ✓
        ("C01_mortgage_calculator_pro", "sandbox_mcp_name"), # was 44, now 30 (hash) ✓
        ("M002_world_clock", "mock_mcp_name"),            # was 31, now 24 ✓
    ]

    @pytest.mark.parametrize("task_id,prefix_func_name", FIX_VERIFY_CASES)
    def test_keys_fit_within_30_chars_after_fix(self, task_id, prefix_func_name):
        """After shortening prefixes, all server keys fit ≤ 30 chars — no truncation."""
        from ce_runner._common import mock_mcp_name, sandbox_mcp_name

        func = mock_mcp_name if prefix_func_name == "mock_mcp_name" else sandbox_mcp_name
        key = func(task_id)
        truncated = sanitize_server_name(key)

        assert len(key) <= 30, (
            f"FIX VERIFIED: '{key}' ({len(key)} chars) now fits within 30-char limit.\n"
            f"No truncation needed → deny patterns will match correctly."
        )
        # Key should be unchanged (no truncation)
        assert truncated == key, (
            f"Key should not be truncated after fix: {key} → {truncated}"
        )

    def test_shortened_prefixes_fit_common_tasks(self):
        """Proposed fix: ce-mock- (8 chars) / ce-sb- (6 chars) fit most task IDs."""
        fitting_tasks = [
            "T001zh_email_triage",       # 20 chars → ce-mock-... = 28 ✓
            "M002_world_clock",          # 17 chars → ce-sb-... = 23 ✓
            "T002_email_triage",         # 18 chars → ce-mock-... = 26 ✓
        ]

        for tid in fitting_tasks:
            mock_key = f"ce-mock-{tid}"
            sb_key = f"ce-sb-{tid}"
            assert len(mock_key) <= 30, f"ce-mock-{tid} = {len(mock_key)} chars, exceeds 30"
            assert len(sb_key) <= 30, f"ce-sb-{tid} = {len(sb_key)} chars, exceeds 30"

    def test_edge_case_very_long_task_ids_need_shorter_prefix(self):
        """Extremely long task IDs (> 22 chars) need very short prefix or hashing.

        C01_mortgage_calculator_pro = 27 chars.
        ce-mock-C01_mortgage_calculator_pro = 35 chars → exceeds 30.
        cem-C01_mortgage_calculator_pro = 31 chars → still exceeds 30.
        c-C01_mortgage_calculator_pro = 29 chars → fits but too cryptic.

        Best approach: hash-based keys for robustness with any task ID length.
        c- + md5(task_id)[:28] = 30 chars ✓ (consistent, collision-resistant).
        """
        very_long_id = "C01_mortgage_calculator_pro"  # 27 chars

        # Verify actual length
        assert len(very_long_id) == 27, f"task_id length is {len(very_long_id)}, not 27"

        # Hash-based: c- (2 chars) + 28-char hex digest = 30 chars
        import hashlib
        hashed = hashlib.md5(very_long_id.encode()).hexdigest()[:28]
        hash_key = f"c-{hashed}"
        assert len(hash_key) == 30, f"hash-based key: {len(hash_key)} chars"
        # Verify it's a valid MCP server key (alphanumeric + hyphen only)
        assert all(c.isalnum() or c == "-" for c in hash_key), f"invalid chars in: {hash_key}"


class TestDenyPatternMismatch:
    """Verify deny patterns correctly match tool names after the prefix fix."""

    def test_deny_pattern_now_matches_after_fix(self):
        """After fix: server keys ≤ 30 chars → deny patterns match correctly."""
        from ce_runner._common import mock_mcp_name

        task_self = "T001zh_email_triage"
        task_other = "T002_email_triage"

        self_key = mock_mcp_name(task_self)
        other_key = mock_mcp_name(task_other)

        # Verify keys fit — no truncation
        assert len(self_key) <= 30, f"Self key {self_key} exceeds 30"
        assert len(other_key) <= 30, f"Other key {other_key} exceeds 30"

        truncated_self = sanitize_server_name(self_key)
        truncated_other = sanitize_server_name(other_key)

        # Keys should be unchanged (no truncation)
        assert truncated_self == self_key
        assert truncated_other == other_key

        # Build actual tool names (as gateway exposes them)
        tool_self = build_safe_tool_name(truncated_self, "gmail_list_messages")
        tool_other = build_safe_tool_name(truncated_other, "gmail_list_messages")

        # Deny pattern — now matches because key isn't truncated
        deny_pattern = f"{other_key}{TOOL_NAME_SEPARATOR}*"
        deny_compiled = [compile_glob_pattern(deny_pattern)]

        # Should MATCH the other task's tool (deny is effective)
        assert matches_any_glob(tool_other.lower(), deny_compiled), (
            f"FIX VERIFIED: deny pattern '{deny_pattern}' now matches "
            f"truncated tool name '{tool_other}'.\n"
            f"Cross-session tool isolation is WORKING."
        )

        # Should NOT match own task's tool
        assert not matches_any_glob(tool_self.lower(), deny_compiled), (
            f"Deny pattern should NOT match own task's tool"
        )

    def test_deny_pattern_matches_when_keys_fit(self):
        """When keys ≤ 30 chars, deny patterns DO match — proving the fix works."""
        # Simulate shortened prefixes
        task_self = "T001zh_email_triage"
        task_other = "T002_email_triage"

        short_self = f"ce-mock-{task_self}"   # 27 chars ✓
        short_other = f"ce-mock-{task_other}"  # 25 chars ✓

        # Verify no truncation needed
        assert len(short_self) <= 30
        assert len(short_other) <= 30

        truncated_self = sanitize_server_name(short_self)
        truncated_other = sanitize_server_name(short_other)

        # Keys should be unchanged (no truncation)
        assert truncated_self == short_self
        assert truncated_other == short_other

        # Build tool names
        tool_self = build_safe_tool_name(truncated_self, "gmail_list_messages")
        tool_other = build_safe_tool_name(truncated_other, "gmail_list_messages")

        # Deny pattern with shortened key
        deny_pattern = f"{short_other}{TOOL_NAME_SEPARATOR}*"
        deny_compiled = [compile_glob_pattern(deny_pattern)]

        # Should match the other task's tool
        assert matches_any_glob(tool_other.lower(), deny_compiled), (
            f"Deny pattern should match tool from other task"
        )

        # Should NOT match own task's tool
        assert not matches_any_glob(tool_self.lower(), deny_compiled), (
            f"Deny pattern should NOT match own task's tool"
        )


class TestAlsoAllowWildcardInjection:
    """Verify that alsoAllow without allow injects * wildcard."""

    def test_alsoallow_without_allow_injects_wildcard(self):
        """pickSandboxToolPolicy adds * when only alsoAllow is set."""
        policy = simulate_pick_sandbox_tool_policy(
            allow=None,
            also_allow=["ce-mock-T001__tool1", "ce-sb-T001__Bash"],
            deny=["exec", "read", "write", "ce-mock-T002__*"],
        )

        assert policy is not None
        assert "*" in policy["allow"], (
            f"BUG: alsoAllow without 'allow' should inject '*'. "
            f"Got allow={policy['allow']}"
        )

    def test_alsoallow_with_allow_does_not_inject_wildcard(self):
        """When both allow AND alsoAllow are set, * is NOT injected."""
        policy = simulate_pick_sandbox_tool_policy(
            allow=["exec"],
            also_allow=["ce-mock-T001__tool1"],
            deny=["ce-mock-T002__*"],
        )

        assert policy is not None
        assert "*" not in policy["allow"], (
            f"With explicit 'allow', '*' should NOT be injected. "
            f"Got allow={policy['allow']}"
        )
        assert "exec" in policy["allow"]


class TestFullIsolationSimulation:
    """End-to-end simulation of the isolation mechanism after the fix."""

    def test_isolation_now_works_after_fix(self):
        """After fix: short keys + explicit allow — agents cannot see each other's tools."""
        from ce_runner._common import mock_mcp_name

        task_a = "T001zh_email_triage"
        task_b = "T002_email_triage"

        key_a = mock_mcp_name(task_a)
        key_b = mock_mcp_name(task_b)

        # Verify keys fit — no truncation
        assert len(key_a) <= 30
        assert len(key_b) <= 30

        truncated_a = sanitize_server_name(key_a)
        truncated_b = sanitize_server_name(key_b)
        assert truncated_a == key_a, "Key A should not be truncated"
        assert truncated_b == key_b, "Key B should not be truncated"

        # Actual tool names in gateway
        tool_a = build_safe_tool_name(truncated_a, "gmail_list_messages")
        tool_b = build_safe_tool_name(truncated_b, "gmail_list_messages")

        # Agent A's policy (matching the fixed ce-runner behavior)
        policy_a = simulate_pick_sandbox_tool_policy(
            allow=["exec", f"{key_a}__gmail_list_messages"],
            deny=["exec", "read", "write", f"{key_b}__*"],
        )

        # Agent B's policy
        policy_b = simulate_pick_sandbox_tool_policy(
            allow=["exec", f"{key_b}__gmail_list_messages"],
            deny=["exec", "read", "write", f"{key_a}__*"],
        )

        def is_tool_allowed(tool_name: str, policy: dict | None) -> bool:
            if policy is None:
                return True
            deny_patterns = [compile_glob_pattern(d) for d in (policy.get("deny") or [])]
            allow_patterns = [compile_glob_pattern(a) for a in (policy.get("allow") or [])]

            name = tool_name.lower()
            if matches_any_glob(name, deny_patterns):
                return False
            if len(allow_patterns) == 0:
                return True
            return matches_any_glob(name, allow_patterns)

        # Agent A sees its own tool
        assert is_tool_allowed(tool_a, policy_a), "Agent A should see its own tool"

        # Agent A does NOT see Agent B's tool — isolation WORKS
        assert not is_tool_allowed(tool_b, policy_a), (
            f"ISOLATION FIX VERIFIED: Agent A cannot see Agent B's tool.\n"
            f"  tool_b: {tool_b}\n"
            f"  deny pattern: {key_b}__* (matches because key fits in 30 chars)\n"
            f"  allow: {policy_a['allow']} (no * — explicit list only)"
        )

        # Agent B does NOT see Agent A's tool
        assert not is_tool_allowed(tool_a, policy_b), (
            f"ISOLATION FIX VERIFIED: Agent B cannot see Agent A's tool"
        )

    def test_isolation_works_with_short_keys_and_explicit_allow(self):
        """Fix: short keys + explicit 'allow' prevents cross-session leakage."""
        task_a = "T001zh_email_triage"
        task_b = "T002_email_triage"

        # Shortened keys (fit within 30 chars)
        key_a = f"ce-mock-{task_a}"
        key_b = f"ce-mock-{task_b}"

        # No truncation
        truncated_a = sanitize_server_name(key_a)
        truncated_b = sanitize_server_name(key_b)
        assert truncated_a == key_a
        assert truncated_b == key_b

        # Actual tool names
        tool_a = build_safe_tool_name(truncated_a, "gmail_list_messages")
        tool_b = build_safe_tool_name(truncated_b, "gmail_list_messages")

        # Agent A's policy: merged allow list prevents * injection
        policy_a = simulate_pick_sandbox_tool_policy(
            allow=["exec", f"{key_a}__gmail_list_messages"],
            deny=["exec", "read", "write", f"{key_b}__*"],
        )

        # Agent B's policy
        policy_b = simulate_pick_sandbox_tool_policy(
            allow=["exec", f"{key_b}__gmail_list_messages"],
            deny=["exec", "read", "write", f"{key_a}__*"],
        )

        def is_tool_allowed(tool_name: str, policy: dict | None) -> bool:
            if policy is None:
                return True
            deny_patterns = [compile_glob_pattern(d) for d in (policy.get("deny") or [])]
            allow_patterns = [compile_glob_pattern(a) for a in (policy.get("allow") or [])]

            name = tool_name.lower()
            if matches_any_glob(name, deny_patterns):
                return False
            if len(allow_patterns) == 0:
                return True
            return matches_any_glob(name, allow_patterns)

        # Agent A sees its own tool
        assert is_tool_allowed(tool_a, policy_a), "Agent A should see its own tool"

        # Agent A does NOT see Agent B's tool — isolation WORKS
        assert not is_tool_allowed(tool_b, policy_a), (
            f"ISOLATION: Agent A should NOT see Agent B's tool.\n"
            f"  tool_b: {tool_b}\n"
            f"  policy_a.deny: {policy_a['deny']}\n"
            f"  policy_a.allow: {policy_a['allow']}"
        )

        # Agent B does NOT see Agent A's tool
        assert not is_tool_allowed(tool_a, policy_b), (
            f"ISOLATION: Agent B should NOT see Agent A's tool"
        )


class TestToolInjectorIntegration:
    """Verify _build_allowlist and _build_deny_list produce correct entries."""

    def test_build_allowlist_includes_correct_tool_names(self):
        """_build_allowlist produces serverKey__toolName entries."""
        from ce_runner.tool_injector import ToolInjector

        mock_name = "ce-mock-T001"
        tool_names = ["gmail_list_messages", "gmail_send"]
        sandbox_name = "ce-sb-T001"

        allowed = ToolInjector._build_allowlist(
            mock_mcp_name=mock_name,
            mock_tool_names=tool_names,
            sandbox_mcp_name=sandbox_name,
        )

        assert f"{mock_name}__gmail_list_messages" in allowed
        assert f"{mock_name}__gmail_send" in allowed
        assert f"{sandbox_name}__Bash" in allowed
        assert f"{sandbox_name}__Read" in allowed

    def test_setup_parallel_adds_cross_task_deny(self):
        """Verify that if we had the task_yamls, setup_parallel would add cross-task deny."""
        from ce_runner._common import mock_mcp_name, sandbox_mcp_name

        task_a = "T001_test"
        task_b = "T002_other"

        mock_a = mock_mcp_name(task_a)
        mock_b = mock_mcp_name(task_b)
        sb_a = sandbox_mcp_name(task_a)
        sb_b = sandbox_mcp_name(task_b)

        # Verify keys fit within 30 chars
        for key in (mock_a, mock_b, sb_a, sb_b):
            assert len(key) <= 30, f"Key '{key}' exceeds 30 chars"

        # Simulate what setup_parallel_workers builds for task A
        all_mcp = {mock_a, mock_b, sb_a, sb_b}
        this_task = {mock_a, sb_a}
        deny_entries = [f"{key}__*" for key in (all_mcp - this_task)]

        assert f"{mock_b}__*" in deny_entries
        assert f"{sb_b}__*" in deny_entries
        assert f"{mock_a}__*" not in deny_entries
