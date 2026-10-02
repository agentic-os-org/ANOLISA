"""Contract tests for security observability skill distribution."""

import json
from pathlib import Path

_AGENT_SEC_CORE_DIR = Path(__file__).resolve().parents[3]
_AGENTS_PATH = _AGENT_SEC_CORE_DIR / "AGENTS.md"
_MANIFEST_PATH = _AGENT_SEC_CORE_DIR / "cosh-extension" / "cosh-extension.json"
_COMPONENT_MANIFEST_PATH = _AGENT_SEC_CORE_DIR / ".anolisa" / "component.toml"
_SKILL_PATH = _AGENT_SEC_CORE_DIR / "skills" / "security-observability" / "SKILL.md"


def _parse_frontmatter(content: str) -> dict[str, str]:
    assert content.startswith("---\n")
    frontmatter = content.split("---", 2)[1]
    values: dict[str, str] = {}
    for line in frontmatter.splitlines():
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        values[key.strip()] = value.strip()
    return values


def test_cosh_extension_does_not_bundle_security_observability_skill() -> None:
    manifest = json.loads(_MANIFEST_PATH.read_text(encoding="utf-8"))

    assert "skills" not in manifest


def test_component_manifest_installs_security_observability_with_skills_bundle() -> (
    None
):
    content = _COMPONENT_MANIFEST_PATH.read_text(encoding="utf-8")

    assert 'source = "share/anolisa/skills/"' in content
    assert 'target = "{datadir}/skills/"' in content
    assert _SKILL_PATH.is_file()


def test_security_observability_skill_has_valid_frontmatter() -> None:
    content = _SKILL_PATH.read_text(encoding="utf-8")
    frontmatter = _parse_frontmatter(content)

    assert frontmatter["name"] == "security-observability"
    assert "agent-sec-cli" in frontmatter["description"]
    assert "安全事件" in frontmatter["description"]
    assert "会话" in frontmatter["description"]


def test_security_observability_skill_documents_cli_and_output_contracts() -> None:
    content = _SKILL_PATH.read_text(encoding="utf-8")

    assert "agent-sec-cli events" in content
    assert "agent-sec-cli observability report" in content
    assert "observability report --last --format json" in content
    assert "observability report --session-id '<session_id>' --format json" in content
    assert "`--last` 查询最近记录的会话" in content
    assert "`--session-id '<session_id>'` 查询指定会话" in content
    assert "--last-hours" in content
    assert "--since" in content
    assert "--until" in content
    assert "category`、`event_type`、`trace_id`" in content
    assert "--limit '<matching_count_or_safe_page_size>'" in content
    assert "--count" in content

    event_fields = {
        "event_id",
        "event_type",
        "category",
        "result",
        "timestamp",
        "trace_id",
        "pid",
        "uid",
        "session_id",
        "run_id",
        "call_id",
        "tool_call_id",
        "details",
    }
    report_fields = {
        "first_seen",
        "last_seen",
        "duration_seconds",
        "turn_count",
        "llm_calls",
        "request_bytes",
        "response_bytes",
        "tool_breakdown",
        "security_verdicts",
        "security_hint",
    }

    for field in event_fields | report_fields:
        assert f"`{field}`" in content or f'"{field}"' in content

    assert "backend-specific" in content or "后端专属" in content
    assert "succeeded/failed" in content or "succeeded` / `failed" in content
    assert "pass` / `warn` / `deny" in content


def test_security_observability_skill_includes_few_shot_queries() -> None:
    content = _SKILL_PATH.read_text(encoding="utf-8")

    assert "## Few-shot 场景" in content
    assert "帮我查询最近一个小时出现的安全事件" in content
    assert "events --last-hours 1 --output json" in content
    assert "帮我查询本次会话出现的安全事件" in content
    assert "events --session-id '<current_session_id>' --output json" in content
    assert "帮我复盘本次会话的安全情况" in content
    assert "帮我复盘最近一次 Agent 会话的安全情况" not in content


def test_security_observability_skill_documents_cosh_ng_session_id_lookup() -> None:
    """The cosh-ng-specific session lookup must stay explicit and fenced off.

    ``runtime_context`` only exists in cosh-ng, and ``COSH_SESSION_ID`` is a
    different identity namespace there, so both facts have to survive edits.
    """
    content = _SKILL_PATH.read_text(encoding="utf-8")

    assert "## 获取当前 session_id" in content
    assert "### cosh-ng 特别用法：`runtime_context` 工具" in content
    assert "`provider_session_id`" in content
    assert "events --session-id '<provider_session_id>' --output json" in content
    assert "它应当是 UUID" in content
    assert "必须继续按 `^[0-9a-fA-F]" in content
    assert (
        "observability report --session-id '<provider_session_id>' --format json"
        in content
    )
    # The skill must scope the tool to cosh-ng rather than presenting it as generic.
    assert "仅适用于 **cosh-ng** Agent" in content
    # Anti-pattern: the shell/terminal identity is not the agent session id.
    assert "不要用环境变量 `$COSH_SESSION_ID` 代替" in content
    assert "shell_session_id" in content
    # runtime_context carries no run_id, so run scoping still needs another source.
    assert "不返回 `run_id`" in content


def test_security_observability_skill_scopes_session_queries_to_cosh_ng() -> None:
    """Only cosh-ng can resolve its own session id.

    Every other runtime must fall back to a time range or ``--last`` and state
    the real query scope, instead of stalling on an id it cannot obtain.
    """
    content = _SKILL_PATH.read_text(encoding="utf-8")

    assert "### 其他 Agent 运行时：不使用 `--session-id`" in content
    assert "只有 cosh-ng 能让 Agent 取得自己的 `session_id`" in content
    assert "默认改用**时间范围查询**" in content
    assert "**必须在报告中说明实际查询范围**" in content
    # The old "ask the user for the id" fallback must not come back.
    assert "不要因为拿不到 `session_id` 而停下来反复询问用户" in content


def test_security_observability_skill_allows_safe_correlation_ids() -> None:
    content = _SKILL_PATH.read_text(encoding="utf-8")
    agents = _AGENTS_PATH.read_text(encoding="utf-8")

    assert "不一定是 UUID" in content
    assert "session-001" in content
    assert "thread_xxx" in content
    assert "长度不超过 256 字符" in content
    assert "^[A-Za-z0-9][A-Za-z0-9._:@+=,/-]{0,255}$" in content
    assert "cosh-ng `runtime_context.provider_session_id`" in agents
    assert "which is expected to be a UUID" in agents


def test_security_observability_parameters_are_covered_by_agents_contract() -> None:
    content = _AGENTS_PATH.read_text(encoding="utf-8")

    assert "skills/security-observability/SKILL.md" in content
    assert "self-contained" in content
    assert "implicit contract with the CLI help text" in content
    assert "Do not replace it with instructions" in content
    assert "--help" in content
    assert "contract tests" in content
    assert "explicit `--limit`" in content
    assert "counts between 101 and 200" in content
    assert "bounded safe-character full-match" in content
