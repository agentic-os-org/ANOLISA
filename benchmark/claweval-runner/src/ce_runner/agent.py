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

"""Agent execution: run openclaw agent via CLI or HTTP API."""

from __future__ import annotations

import base64
import json
import mimetypes
import os
import signal
import subprocess
import threading
import time
from pathlib import Path

import httpx

from ._common import (OPENCLAW_CONFIG, SESSIONS_DIR, _agent_sessions_dir,
                       load_task_yaml, log)


# ── Last-error reporting (per-thread) ────────────────────────────────────────
#
# run_agent / _run_agent_continue / _run_first_turn_via_api keep their
# return-type as a session-file string (or "" on failure) for backwards
# compatibility with existing callers and tests.  When they return "" the
# caller can read ``last_agent_error()`` to learn *why* — e.g. an HTTP 4xx
# from the multimodal endpoint, a CLI non-zero exit, an actual timeout, or
# a missing session file.  The state is thread-local because run_agent is
# invoked concurrently from batch_runner's ThreadPoolExecutor.

_tls = threading.local()


def last_agent_error() -> str:
    """Return the most recent failure reason on this thread (or "")."""
    return getattr(_tls, "reason", "") or ""


def _set_agent_error(reason: str) -> None:
    _tls.reason = reason or ""


def _clear_agent_error() -> None:
    _tls.reason = ""


# ── HTTP API (multimodal) ────────────────────────────────────────────────────

def _run_first_turn_via_api(text: str,
                             image_attachments: list,
                             text_attachments: list,
                             timeout: int, agent_id: str | None,
                             task_dir: str) -> str:
    """Run first agent turn via HTTP API for multimodal tasks with image attachments.

    Uses POST /v1/chat/completions (OpenAI-compatible) to send images as
    data URIs.  After the call, finds the new session entry in sessions.json
    by comparing ``updatedAt`` timestamps — the deterministic way to locate
    the session file created by this request, even when multiple trials
    share the same agent directory.

    *text_attachments* are inlined as additional ``text`` content blocks
    after the prompt text so mixed image/text payloads survive a single
    multimodal request without crashing the model on non-image data.

    *timeout* caps the total wall-clock time (seconds) for all HTTP attempts
    combined, honouring the user-supplied ``--timeout`` CLI flag.  Per-read
    inactivity is still governed by ``CE_RUNNER_HTTP_INACTIVITY_S``.

    Returns the session file path or empty string on failure.
    """
    with open(OPENCLAW_CONFIG) as f:
        oc_config = json.load(f)

    gateway_port = oc_config["gateway"]["port"]
    auth_token = oc_config["gateway"]["auth"]["token"]

    # Build multimodal content array
    content: list[dict] = [{"type": "text", "text": text}]
    for attachment in image_attachments:
        img_path = Path(attachment)
        if not img_path.is_absolute():
            img_path = (Path(task_dir) / attachment).resolve()
        if not img_path.exists():
            log(f"  [WARNING] attachment not found, skipping: {img_path}")
            continue
        mime_type = mimetypes.guess_type(str(img_path))[0]
        if not mime_type or not mime_type.startswith("image/"):
            log(f"  [WARNING] unrecognised image MIME, skipping: {img_path}")
            continue
        try:
            data = img_path.read_bytes()
        except OSError as exc:
            log(f"  [WARNING] failed to read image attachment {img_path}: {exc}")
            continue
        b64 = base64.b64encode(data).decode()
        content.append({
            "type": "image_url",
            "image_url": {"url": f"data:{mime_type};base64,{b64}"},
        })

    for attachment in text_attachments:
        txt_path = Path(attachment)
        if not txt_path.is_absolute():
            txt_path = (Path(task_dir) / attachment).resolve()
        if not txt_path.exists():
            log(f"  [WARNING] attachment not found, skipping: {txt_path}")
            continue
        try:
            body = txt_path.read_text(encoding="utf-8", errors="replace")
        except OSError as exc:
            log(f"  [WARNING] failed to read text attachment {txt_path}: {exc}")
            continue
        content.append({
            "type": "text",
            "text": f"\n\n--- attachment: {txt_path.name} ---\n{body}",
        })

    model = f"openclaw/{agent_id}" if agent_id else "openclaw/main"
    sessions_dir = _agent_sessions_dir(agent_id) if agent_id else SESSIONS_DIR
    # openclaw normalises agent IDs to lowercase in sessions.json keys
    agent_prefix = (agent_id or "main").lower()
    key_prefix = f"agent:{agent_prefix}:openai:"

    # Snapshot the highest updatedAt for *openai* entries of this agent
    max_ts_before = _max_updated_at_for_prefix(sessions_dir, key_prefix)

    body = {
        "model": model,
        "messages": [{"role": "user", "content": content}],
        "stream": True,
    }

    inactivity_s = float(os.environ.get("CE_RUNNER_HTTP_INACTIVITY_S", "120"))
    max_retries = int(os.environ.get("CE_RUNNER_HTTP_MAX_RETRIES", "5"))
    payload_bytes = len(json.dumps(body))
    n_images = len(image_attachments)
    n_text = len(text_attachments)
    attachment_sizes: list[int] = []
    for att in list(image_attachments) + list(text_attachments):
        ap = Path(att) if Path(att).is_absolute() else (Path(task_dir) / att).resolve()
        try:
            attachment_sizes.append(ap.stat().st_size)
        except OSError:
            attachment_sizes.append(-1)
    endpoint = f"127.0.0.1:{gateway_port}/v1/chat/completions"
    diag_tpl = (
        f"class={{cls}} wall={{wall:.1f}}s attempt={{attempt}} read_s={{read_s}} "
        f"endpoint={endpoint} model={model} "
        f"payload_bytes={payload_bytes} n_images={n_images} n_text={n_text} "
        f"attachment_sizes={attachment_sizes}"
    )
    _clear_agent_error()

    _RETRYABLE = (httpx.ReadTimeout, httpx.ConnectTimeout, httpx.WriteTimeout)

    last_exc: BaseException | None = None
    t0 = time.monotonic()
    deadline = t0 + timeout
    for attempt in range(1, max_retries + 2):  # max_retries+1 total attempts
        if attempt > 1:
            backoff = 2 ** (attempt - 1)
            if time.monotonic() + backoff > deadline:
                log(f"  [WARNING] Wall-clock timeout ({timeout}s) reached, "
                    f"aborting before attempt {attempt}")
                break
            log(f"  [RETRY] attempt {attempt}/{max_retries + 1} after {backoff}s backoff")
            time.sleep(backoff)
            max_ts_before = _max_updated_at_for_prefix(sessions_dir, key_prefix)

        if time.monotonic() > deadline:
            log(f"  [WARNING] Wall-clock timeout ({timeout}s) exceeded")
            break

        # Read timeout: fixed for the first 3 attempts (quick probe of
        # transient stalls), then doubles from attempt 4 onward to accommodate
        # genuinely slow upstream paths.  With inactivity_s=120:
        #   attempts 1-3: 120s, attempt 4: 240s, 5: 480s, 6: 960s
        read_s = inactivity_s * max(1, 2 ** (attempt - 3))
        http_timeout = httpx.Timeout(
            connect=30.0,
            read=read_s,
            write=120.0,
            pool=5.0,
        )
        t_attempt = time.monotonic()
        try:
            with httpx.Client(timeout=http_timeout) as client:
                with client.stream(
                    "POST",
                    f"http://127.0.0.1:{gateway_port}/v1/chat/completions",
                    json=body,
                    headers={
                        "Authorization": f"Bearer {auth_token}",
                        "Content-Type": "application/json",
                    },
                ) as resp:
                    resp.raise_for_status()
                    resp.read()
            last_exc = None
            break
        except httpx.HTTPStatusError as exc:
            wall = time.monotonic() - t_attempt
            body_text = ""
            try:
                body_text = (exc.response.text or "")[:200]
            except Exception:
                pass
            ctx = diag_tpl.format(cls=type(exc).__name__, wall=wall,
                                  attempt=attempt, read_s=read_s)
            reason = f"http_status_{exc.response.status_code}: {body_text} {ctx}"
            log(f"  [ERROR] HTTP API /v1/chat/completions failed: {reason}")
            _set_agent_error(reason)
            return ""
        except _RETRYABLE as exc:
            wall = time.monotonic() - t_attempt
            ctx = diag_tpl.format(cls=type(exc).__name__, wall=wall,
                                  attempt=attempt, read_s=read_s)
            log(f"  [WARNING] HTTP stall ({type(exc).__name__}) attempt "
                f"{attempt}/{max_retries + 1} wall={wall:.0f}s")
            last_exc = exc
            continue
        except httpx.RequestError as exc:
            wall = time.monotonic() - t_attempt
            ctx = diag_tpl.format(cls=type(exc).__name__, wall=wall, attempt=attempt)
            reason = f"http_error: {exc!r} {ctx}"
            log(f"  [ERROR] HTTP API /v1/chat/completions failed: {reason}")
            _set_agent_error(reason)
            return ""

    if last_exc is not None:
        wall = time.monotonic() - t0
        ctx = diag_tpl.format(cls=type(last_exc).__name__, wall=wall,
                              attempt=attempt, read_s=read_s)
        reason = f"http_timeout_after_{wall:.0f}s {ctx}"
        log(f"  [ERROR] HTTP API exhausted {max_retries + 1} attempts, "
            f"last error: {type(last_exc).__name__} {ctx}")
        _set_agent_error(reason)
        return ""

    # Deadline exceeded without a successful response
    if time.monotonic() > deadline:
        wall = time.monotonic() - t0
        reason = f"http_wall_clock_timeout_{wall:.0f}s (limit={timeout}s)"
        _set_agent_error(reason)
        return ""

    # Find the new session entry via sessions.json timestamps
    session_file = _find_new_session_by_timestamp(sessions_dir, key_prefix,
                                                  max_ts_before)
    if session_file:
        return session_file

    # Fallback: most recently modified .jsonl session file (exclude trajectory)
    sdir = Path(sessions_dir)
    sessions = _session_jsonl_files(sdir)
    if sessions:
        return str(max(sessions, key=lambda p: p.stat().st_mtime))
    _set_agent_error("session_file_missing")
    return ""


def _max_updated_at_for_prefix(sessions_dir: str, key_prefix: str) -> int:
    """Return the maximum ``updatedAt`` across entries whose key matches *key_prefix*."""
    index_path = Path(sessions_dir) / "sessions.json"
    try:
        with open(index_path) as f:
            index = json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return 0

    max_ts = 0
    for key, entry in index.items():
        if key.startswith(key_prefix):
            ts = entry.get("updatedAt", 0)
            if ts and ts > max_ts:
                max_ts = ts
    return max_ts


def _find_new_session_by_timestamp(sessions_dir: str, key_prefix: str,
                                   after_ts: int) -> str:
    """Find a session file via sessions.json by looking for new entries
    whose ``updatedAt`` is strictly greater than *after_ts*.

    Retries briefly because sessions.json may not be flushed to disk
    immediately after the API response is sent.
    """
    index_path = Path(sessions_dir) / "sessions.json"

    for _ in range(10):
        try:
            with open(index_path) as f:
                index = json.load(f)
        except (FileNotFoundError, json.JSONDecodeError):
            time.sleep(0.1)
            continue

        best_ts = 0
        best_sid = ""
        for key, entry in index.items():
            if not key.startswith(key_prefix):
                continue
            ts = entry.get("updatedAt", 0)
            if ts and ts > after_ts and ts > best_ts:
                best_ts = ts
                best_sid = entry.get("sessionId", "")

        if best_sid:
            sf = Path(sessions_dir) / f"{best_sid}.jsonl"
            if sf.exists():
                return str(sf)

        time.sleep(0.1)

    return ""


# ── CLI agent ────────────────────────────────────────────────────────────────

def _read_err_tail(err_log: str, n: int = 200) -> str:
    """Best-effort tail of *err_log* (last *n* chars), single line, stripped.

    Used to embed CLI failure context into the structured error reason.
    """
    try:
        with open(err_log, "rb") as f:
            try:
                f.seek(-n, os.SEEK_END)
            except OSError:
                f.seek(0)
            data = f.read()
    except OSError:
        return ""
    text = data.decode("utf-8", errors="replace").strip()
    # Collapse newlines so reason fits on one log line.
    return " ".join(text.split())[-n:]


def _terminate_process_group(proc: subprocess.Popen) -> None:
    """Best-effort SIGKILL of the CLI process group to reap orphan grandchildren.

    The CLI is launched with ``start_new_session=True`` so it becomes a process
    group leader; killing the whole group ensures any background workers it
    spawned do not survive as orphans.  ProcessLookupError/OSError are ignored
    because the group may already be gone.
    """
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
    except (ProcessLookupError, OSError):
        pass


def run_agent(session_id: str, task_yaml: str, timeout: int,
              agent_id: str | None = None) -> str:
    """Run openclaw agent and return session file path (or empty on failure).

    When the task prompt includes attachments (images), the first turn is sent
    via the HTTP /v1/chat/completions API so that images are correctly
    recognised by the model.  Text-only tasks use the CLI ``--message`` path.

    When *agent_id* is provided, the agent command uses ``--agent <id>`` so
    the gateway routes the request to the correct isolated agent (with its
    own MCP tool allowlist).
    """
    task = load_task_yaml(task_yaml)
    prompt = task.get("prompt", {})
    text = prompt if isinstance(prompt, str) else prompt.get("text", "")
    attachments = prompt.get("attachments", []) if isinstance(prompt, dict) else []

    task_dir = str(Path(task_yaml).parent)
    image_attachments: list = []
    text_attachments: list = []
    for attachment in attachments:
        att_path = Path(attachment)
        if not att_path.is_absolute():
            att_path = (Path(task_dir) / attachment).resolve()
        if not att_path.exists():
            log(f"  [WARNING] attachment not found, skipping: {att_path}")
            continue
        mime_type, _ = mimetypes.guess_type(str(att_path))
        if (mime_type or "").startswith("image/"):
            image_attachments.append(attachment)
        else:
            text_attachments.append(attachment)

    _clear_agent_error()

    if image_attachments:
        # Mixed or image-only: still go via multimodal HTTP API.
        return _run_first_turn_via_api(text, image_attachments,
                                       text_attachments, timeout, agent_id,
                                       task_dir)

    # ── Text-only path (CLI behaviour, with inlined text attachments) ──────
    message = text
    for attachment in text_attachments:
        att_path = Path(attachment)
        if not att_path.is_absolute():
            att_path = (Path(task_dir) / attachment).resolve()
        try:
            body = att_path.read_text(encoding="utf-8", errors="replace")
        except OSError as exc:
            log(f"  [WARNING] failed to read text attachment {att_path}: {exc}")
            continue
        message = (
            f"{message}\n\n--- attachment: {att_path.name} ---\n{body}"
        )
    result_json = f"/tmp/openclaw_agent_result_{session_id}.json"
    err_log = f"/tmp/openclaw_agent_{session_id}.err"

    # Process timeout = agent timeout + 60s grace for gateway response + cleanup
    proc_timeout = timeout + 60

    cmd = [
        "openclaw", "agent",
        "--session-id", session_id,
        "--message", message,
        "--thinking", "high",
        "--timeout", str(timeout),
        "--json",
    ]
    if agent_id:
        cmd.extend(["--agent", agent_id])

    timed_out = False
    with open(result_json, "w") as out, open(err_log, "w") as err:
        proc = subprocess.Popen(cmd, stdout=out, stderr=err,
                                start_new_session=True)
        try:
            proc.communicate(timeout=proc_timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            log(f"  [WARNING] Agent process timed out after {proc_timeout}s")
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
            except (ProcessLookupError, OSError):
                pass
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                _terminate_process_group(proc)
                proc.wait()
        finally:
            # Reap any background grandchildren the CLI may have left behind.
            _terminate_process_group(proc)

    rc = proc.returncode
    if timed_out:
        _set_agent_error(f"cli_timeout_after_{proc_timeout}s")
    elif rc is not None and rc != 0:
        _set_agent_error(f"cli_exit_{rc}: {_read_err_tail(err_log)}")

    # Extract actual session ID and find the session file
    actual_session_id = _extract_session_id_from_result(result_json)
    session_file = _find_session_file(actual_session_id or session_id,
                                      agent_id=agent_id)
    if not session_file and not last_agent_error():
        _set_agent_error("session_file_missing")
    return session_file


def _extract_session_id_from_result(result_json: str) -> str:
    """Extract actual session ID from openclaw agent result JSON."""
    try:
        with open(result_json) as f:
            data = json.load(f)
        return (
            data.get("result", {})
                .get("meta", {})
                .get("agentMeta", {})
                .get("sessionId", "")
        )
    except Exception:
        return ""


def _session_jsonl_files(sdir: Path) -> list[Path]:
    """Return session .jsonl files, excluding .trajectory.jsonl snapshots."""
    if not sdir.is_dir():
        return []
    return [p for p in sdir.glob("*.jsonl")
            if not p.name.endswith(".trajectory.jsonl")]


def _find_session_file(session_id: str, agent_id: str | None = None) -> str:
    """Find session file by ID, or fall back to most recently modified."""
    sessions_dir = _agent_sessions_dir(agent_id) if agent_id else SESSIONS_DIR
    if session_id:
        session_file = Path(sessions_dir) / f"{session_id}.jsonl"
        if session_file.exists():
            return str(session_file)
    sessions = _session_jsonl_files(Path(sessions_dir))
    if sessions:
        return str(max(sessions, key=lambda p: p.stat().st_mtime))
    return ""


def _get_last_assistant_has_tool_calls(session_file: str) -> bool:
    """Check if the last assistant message in a session has tool calls.

    Returns True if the last assistant message contains toolCall blocks,
    meaning the agent is still working. Returns False if it ended with
    only text (agent finished its turn).
    """
    last_assistant_has_tools = False
    try:
        with open(session_file) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if event.get("type") != "message":
                    continue
                msg = event.get("message", {})
                if msg.get("role") != "assistant":
                    continue
                content = msg.get("content", [])
                if isinstance(content, list):
                    has_tools = any(
                        c.get("type") == "toolCall" for c in content
                    )
                    last_assistant_has_tools = has_tools
    except Exception:
        pass
    return last_assistant_has_tools


def _build_conversation_for_user_agent(session_file: str) -> list:
    """Build a simplified conversation history from session file for UserAgent.

    Returns a list of dicts with 'role' and 'text' keys, representing the
    conversation from the user's perspective (suitable for UserAgent).
    """
    messages = []
    try:
        with open(session_file) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if event.get("type") != "message":
                    continue
                msg = event.get("message", {})
                role = msg.get("role", "")
                if role not in ("user", "assistant"):
                    continue
                content = msg.get("content", [])
                if isinstance(content, str):
                    text = content
                elif isinstance(content, list):
                    texts = [
                        c.get("text", "") for c in content
                        if c.get("type") == "text"
                    ]
                    text = "\n".join(t for t in texts if t)
                else:
                    text = ""
                if text:
                    messages.append({"role": role, "text": text})
    except Exception:
        pass
    return messages


def _call_user_agent_llm(ua_config: dict, persona: str,
                          conversation: list) -> str | None:
    """Call the UserAgent LLM to generate a simulated user response.

    Uses the same prompt format as claw-eval's UserAgent class.
    Returns the response text, or None if user is satisfied ([DONE]).
    """
    import random

    system_prompt = f"""你是一个模拟用户。你的任务是根据以下人设与AI助手进行对话。

## 你的人设
{persona}

## 规则
1. 始终保持人设角色，用自然口语回复，不要暴露你是AI
2. 根据助手的提问如实回答（基于你的人设信息）
3. 如果助手问了你人设中没有的信息，说"不太清楚具体数字"或类似自然回复
4. 如果助手已经给出了完整的计算结果和建议，且你没有更多问题，输出 [DONE]
5. 如果你对回答满意或助手已充分回答了你的问题，输出 [DONE]
6. 回复要简短自然，像真实用户一样（1-3句话）
"""

    # Format transcript
    lines = []
    for msg in conversation:
        if msg["role"] == "user":
            text = msg["text"]
            if text.startswith("[user_agent]"):
                text = text[len("[user_agent]"):].strip()
            lines.append(f"[用户]: {text}")
        elif msg["role"] == "assistant":
            lines.append(f"[助手]: {msg['text']}")
    transcript = "\n".join(lines)

    user_msg = (
        f"以下是到目前为止的对话：\n\n{transcript}\n\n"
        "请根据你的人设回复助手的最新消息。如果你满意了就输出 [DONE]。"
    )

    try:
        from openai import OpenAI
    except ImportError:
        log("[WARNING] openai package not available for UserAgent")
        return None

    client = OpenAI(
        api_key=ua_config["api_key"],
        base_url=ua_config["base_url"],
    )

    max_retries = 10
    for attempt in range(max_retries):
        try:
            resp = client.chat.completions.create(
                model=ua_config["model_id"],
                messages=[
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": user_msg},
                ],
                temperature=0.7,
                max_tokens=65536,
            )
            text = (resp.choices[0].message.content or "").strip()
            if "[DONE]" in text:
                return None
            if text:
                return text
            return None
        except Exception as exc:
            delay = min(2 ** (attempt + 1), 16) + random.uniform(0, 1)
            log(f"  [user-agent-retry] {type(exc).__name__}, "
                f"attempt {attempt + 1}/{max_retries}, waiting {delay:.1f}s ...")
            time.sleep(delay)

    return None


def _run_agent_continue(session_id: str, message: str, timeout: int,
                        agent_id: str | None = None) -> str:
    """Continue an existing openclaw agent session with a new message.

    Returns session file path or empty string on failure.
    """
    result_json = f"/tmp/openclaw_agent_result_{session_id}_cont.json"
    err_log = f"/tmp/openclaw_agent_{session_id}_cont.err"

    proc_timeout = timeout + 60

    cmd = [
        "openclaw", "agent",
        "--session-id", session_id,
        "--message", message,
        "--thinking", "high",
        "--timeout", str(timeout),
        "--json",
    ]
    if agent_id:
        cmd.extend(["--agent", agent_id])

    _clear_agent_error()
    timed_out = False
    with open(result_json, "w") as out, open(err_log, "w") as err:
        proc = subprocess.Popen(cmd, stdout=out, stderr=err,
                                start_new_session=True)
        try:
            proc.communicate(timeout=proc_timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            log(f"  [WARNING] Agent continue process timed out after {proc_timeout}s")
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
            except (ProcessLookupError, OSError):
                pass
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                _terminate_process_group(proc)
                proc.wait()
        finally:
            # Reap any background grandchildren the CLI may have left behind.
            _terminate_process_group(proc)

    rc = proc.returncode
    if timed_out:
        _set_agent_error(f"cli_timeout_after_{proc_timeout}s")
    elif rc is not None and rc != 0:
        _set_agent_error(f"cli_exit_{rc}: {_read_err_tail(err_log)}")

    actual_id = _extract_session_id_from_result(result_json)
    session_file = _find_session_file(actual_id or session_id,
                                      agent_id=agent_id)
    if not session_file and not last_agent_error():
        _set_agent_error("session_file_missing")
    return session_file


def _tokenize(text: str) -> set[str]:
    """Split *text* into a token set for similarity comparison.

    English words are split on whitespace.  CJK runs are decomposed into
    character bigrams (mirroring the agentsight loop-detector strategy) so
    that overlap reflects phrase-level similarity rather than the near-100%
    character-set overlap that raw ``set(text)`` produces for Chinese.
    """
    tokens: set[str] = set()
    for word in text.split():
        cjk_chars = [ch for ch in word if '\u4e00' <= ch <= '\u9fff'
                     or '\u3400' <= ch <= '\u4dbf' or '\uf900' <= ch <= '\ufaff']
        if len(cjk_chars) >= 2:
            for i in range(len(cjk_chars) - 1):
                tokens.add(cjk_chars[i] + cjk_chars[i + 1])
        elif len(cjk_chars) == 1:
            tokens.add(cjk_chars[0])
        else:
            tokens.add(word.lower())
    return tokens


def _is_repetitive(reply: str, history: list[str], threshold: float = 0.7) -> bool:
    """Check if reply is too similar to any previous UA reply (token overlap)."""
    reply_tokens = _tokenize(reply)
    for prev in history:
        prev_tokens = _tokenize(prev)
        if not reply_tokens or not prev_tokens:
            continue
        overlap = len(reply_tokens & prev_tokens) / max(len(reply_tokens), len(prev_tokens))
        if overlap >= threshold:
            return True
    return False


def run_agent_with_user_agent(session_id: str, task_yaml: str, timeout: int,
                               ua_config: dict,
                               agent_id: str | None = None) -> str:
    """Run openclaw agent with multi-round UserAgent dialogue support.

    For C tasks (user_agent enabled), implements:
    1. Initial agent run with task prompt
    2. If agent finishes without tool calls, invoke UserAgent LLM
    3. If UserAgent responds, continue agent with UserAgent's reply
    4. Repeat until UserAgent says [DONE] or max_rounds reached

    Returns session file path.
    """
    task = load_task_yaml(task_yaml)
    ua_task_cfg = task.get("user_agent", {})
    persona = ua_task_cfg.get("persona", "")
    max_rounds = ua_task_cfg.get("max_rounds", 8)

    # Step 1: Initial run
    log(f"  [user-agent] Starting multi-round dialogue (max_rounds={max_rounds})")
    session_file = run_agent(session_id, task_yaml, timeout, agent_id=agent_id)
    if not session_file:
        return ""

    # Multi-round loop
    ua_replies: list[str] = []
    repetition_count = 0

    for round_num in range(1, max_rounds + 1):
        # Check if agent's last message has tool calls (still working)
        if _get_last_assistant_has_tool_calls(session_file):
            log(f"  [user-agent] Agent still has tool calls, unexpected end")
            break

        # Build conversation and call UserAgent
        conversation = _build_conversation_for_user_agent(session_file)
        log(f"  [user-agent] Round {round_num}/{max_rounds}: calling UserAgent LLM...")

        ua_reply = _call_user_agent_llm(ua_config, persona, conversation)

        if ua_reply is None:
            log(f"  [user-agent] User satisfied ([DONE]) at round {round_num}")
            break

        if _is_repetitive(ua_reply, ua_replies):
            repetition_count += 1
            if repetition_count >= 2:
                log(f"  [user-agent] Repetitive replies detected ({repetition_count}x), "
                    f"stopping at round {round_num}")
                break
        else:
            repetition_count = 0

        ua_replies.append(ua_reply)
        log(f"  [user-agent] Round {round_num}: {ua_reply[:100]}...")

        # Continue agent session with UserAgent's reply
        ua_message = f"[user_agent]\n{ua_reply}"
        session_file = _run_agent_continue(session_id, ua_message, timeout,
                                           agent_id=agent_id)
        if not session_file:
            log(f"  [user-agent] Failed to continue session at round {round_num}")
            break

    return session_file
