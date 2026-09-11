# Agent Memory (agent-memory)

[中文版](../../zh/token-saving/agent-memory.md)

agent-memory is ANOLISA's file-form memory MCP server, providing AI agents with a persistent, searchable, sandboxed memory space. Agents read and write memory like a filesystem; the system injects relevant context into subsequent turns via BM25/vector hybrid retrieval and automatic capture/recall, reducing repeated communication and improving task continuity.

- **File-form memory**: read/write memory with filesystem semantics via MCP tools; namespace isolation and path sandboxing.
- **Hybrid semantic search**: BM25 + dense vector + RRF fusion with automatic fallback.
- **Auto capture & recall**: automatically extracts observations at conversation end (deduped) and injects relevant memory when building the next prompt.
- **Safe injection**: prompt-injection detection and escaping for memory content injected into LLM prompts.
- **Versioning & snapshots**: optional auto git commit + tar.gz snapshots for file-level and mount-level rollback.

Use Agent Memory when context must persist across sessions. Tokenless addresses
a different part of the workflow by compressing content that still needs to
enter the current context window.

---

## Requirements

- Linux on x86_64 or aarch64
- An Agent runtime that supports stdio MCP servers

---

## Installation

### Via anolisa CLI (recommended)

```bash
anolisa install agent-memory
```

Produces: `agent-memory` binary, default config, MCP service descriptor, systemd user template, tmpfiles rule, OpenClaw adapter bundle.

### RPM package (AnolisOS / RHEL)

```bash
sudo yum install agent-memory
```

RPM installs to system-level FHS paths:

| Purpose | Path |
|------|------|
| Service binary | `/usr/bin/agent-memory` |
| Default config | `/usr/share/anolisa/agent-memory/default.toml` |
| MCP service descriptor (auto-discovery) | `/usr/share/anolisa/mcp-servers/agent-memory.json` |
| systemd user template | `/usr/lib/systemd/user/anolisa-memory@.service` |
| tmpfiles rule (creates `/run/anolisa/{,sessions}`) | `/usr/lib/tmpfiles.d/anolisa-memory.conf` |
| OpenClaw adapter bundle | `/usr/share/anolisa/adapters/agent-memory/` |
| Docs | `/usr/share/doc/agent-memory/` |

### Source build (developers)

```bash
git clone https://github.com/alibaba/anolisa.git
cd anolisa/src/agent-memory

make build         # cargo build --release --locked
sudo make install  # install to /usr/local
```

Build deps: Rust ≥ 1.85 (edition 2024; CI pins 1.89 to share the monorepo toolchain), cmake (libgit2 vendored), systemd-devel (journald audit fan-out).

### Cross-platform development

Runtime is Linux-only (depends on user_namespace, mount(2), cgroup v2, inotify, journald). On macOS / Windows use the remote flow:

```bash
make remote-build   # push branch and ssh to a Linux host for cargo build
make remote-test    # same + tests + clippy
```

---

## Integration

### Claude Code / Cursor / Continue / any stdio MCP client

Add to your MCP config:

```json
{
  "mcpServers": {
    "agent-memory": {
      "command": "/usr/bin/agent-memory",
      "args": [],
      "env": {
        "USER_ID": "alice",
        "MEMORY_PROFILE": "advanced"
      }
    }
  }
}
```

`/usr/share/anolisa/mcp-servers/agent-memory.json` lists all 37 tool names for auto-discovering clients.

### OpenClaw

The bundled plugin forwards 4 memory-contract tools (`memory_search`, `memory_get`, `memory_observe`, `memory_get_context`) to agent-memory:

```bash
bash /usr/share/anolisa/adapters/agent-memory/openclaw/scripts/install.sh
openclaw gateway restart
```

Or via anolisa adapter management:

```bash
anolisa adapter enable agent-memory openclaw
anolisa adapter status agent-memory
```

**Prerequisite**: `openclaw` CLI on `$PATH`. The script logs clearly and exits 0 if missing — rerun after installing OpenClaw. `yum remove agent-memory` triggers `%preun` to call the uninstall script, leaving no orphaned config.

**Tool-name hand-off**: OpenClaw's bundled `memory-core` plugin owns the `memory_get` and `memory_search` tool names, and OpenClaw's plugin tool registry is first-wins. While `memory-core` stays loaded it keeps those names, so this plugin's own same-named tools are dropped and `memory_get` answers from `memory-core` instead of from agent-memory.

Only the `anolisa adapter` entry point performs the hand-off:

- `anolisa adapter enable agent-memory openclaw` disables `memory-core` and records the transition in the adapter receipt, driven by the `[[adapters.openclaw.displaces]]` declaration in the component contract; `anolisa adapter disable agent-memory` restores it from that receipt. `adapter enable --dry-run` lists the hand-off as a planned action, and `adapter disable --dry-run` lists the restore.
- `anolisa adapter status agent-memory` reports degraded when `memory-core` is enabled again afterwards — by an operator command or a framework update — because the collision is back even though this plugin itself still registers and loads. It also reports **unknown** rather than healthy for a displacement it has recorded: `plugins disable` only writes config, and ANOLISA has no channel to the running gateway, so it can confirm the hand-off was *recorded* but not that the gateway has *applied* it. Whether you need to do anything further depends on OpenClaw's plugin reload mode: a mode that hot-reloads `plugins.entries.*` applies the change by itself, and one that does not needs `openclaw gateway restart`. Note that restarting does **not** turn this `unknown` into `healthy`, because ANOLISA still cannot observe the gateway afterwards. Read `unknown` here as "not observable", not as a fault. To confirm the hand-off yourself, call `memory_get` and check which plugin answers — a real tool call goes through the running gateway. Do **not** use `openclaw plugins list` or `plugins inspect` for this: both read the persisted registry and config, so before a restart they show `memory-core` as disabled while the old gateway may still be serving its tools. That reads as confirmation and is not.

The `install.sh` entry point above does **not** touch `memory-core`. After installing that way, run `openclaw plugins disable memory-core` yourself (and `openclaw plugins enable memory-core` to undo it), or use `anolisa adapter enable agent-memory openclaw` instead.

The adapter path never re-enables a `memory-core` you had disabled yourself before enabling, and never takes the memory slot back from a choice you made afterwards: if `plugins.slots.memory` points at another plugin by removal time, or you closed the slot explicitly with `plugins.slots.memory = "none"`, `memory-core` is left disabled and the reason is reported. If your OpenClaw is configured not to hot-reload plugin config, run `openclaw gateway restart` after enabling or disabling; a hot-reloading host applies both by itself.

**What disabling `memory-core` costs**: the hand-off disables the whole bundled plugin, not just the two colliding tool names. Everything OpenClaw's `memory-core` provides beyond them is unavailable for as long as agent-memory is enabled — its own `memory_*` tools, the `openclaw memory` command surface, and the background dreaming / consolidation lifecycle that plugin runs. This is not an edge case: it happens on every `adapter enable`.

agent-memory covers the retrieval path in the meantime (see *MCP tool set* and *Auto consolidation* below), but it reads and writes `~/.anolisa/memory`, not `memory-core`'s own store — so anything `memory-core` had already accumulated stays untouched and out of reach until the plugin is re-enabled, and the two stores are not merged for you.

`memory-core` comes back when `anolisa adapter disable agent-memory` restores it, subject to the guard above: if `plugins.slots.memory` has since been given to another backend or closed explicitly, it stays disabled and you re-enable it yourself with `openclaw plugins enable memory-core`. As above, a host that does not hot-reload plugin config needs `openclaw gateway restart` for either change.

Running `anolisa adapter enable agent-memory openclaw` or the agent-memory OpenClaw `install.sh` accepts the plugin's declared capabilities. Both entry points pass `--accept-capabilities` only when `plugins install --help` advertises that exact option, so older hosts keep working. Set `AGENT_MEMORY_ACCEPT_CAPABILITIES=0` when running `install.sh` to withhold consent — gating hosts then reject the install until you grant it yourself, e.g. via an interactive `openclaw plugins install`.

Install-time environment variables (runtime `MEMORY_*` variables are listed separately under Environment variables):

| Variable | Default | Effect |
|---|---|---|
| `AGENT_MEMORY_ACCEPT_CAPABILITIES` | accept | `1`/`true`/`yes`/`on` grant consent when the host advertises the flag; `0`/`false`/`no`/`off` withhold it, so gating hosts reject the install; any other value aborts with an error (exit code 2) before install |
| `AGENT_MEMORY_SAFE_INSTALL` | unset | `1` declines `--dangerously-force-unsafe-install` on hosts that would still receive it; hosts advertising it as a deprecated no-op omit it either way |
| `OPENCLAW_BIN` | `openclaw` | openclaw CLI binary to invoke |
| `OPENCLAW_STATE_DIR` | `~/.openclaw` | state directory passed to every openclaw CLI invocation |
| `OPENCLAW_HOME` | `~/.openclaw` | default for `OPENCLAW_STATE_DIR` only; never passed to the CLI (unset for every call) |

The standalone `install.sh` negotiates the unsafe-install bypass the same way: it passes `--dangerously-force-unsafe-install` only while the installer advertises that option as effective. OpenClaw 2026.6.1 and earlier run an install-time safety scan that blocks `child_process` plugins non-interactively — this plugin spawns the agent-memory MCP server over stdio — so those hosts receive the bypass. OpenClaw 2026.6.5 and later dropped install-time dangerous-code blocking and list the option as a deprecated no-op, so they never receive it; there install-time safety is decided by the operator-owned `security.installPolicy`, which no script flag can override. `AGENT_MEMORY_SAFE_INSTALL=1` declines the bypass on hosts where it still has effect and changes nothing on current ones — the install log states which case applied. If the `plugins install --help` probe itself fails, the host cannot be classified: the script keeps the bypass so pre-2026.6.2 hosts still install, logs a WARNING, and `AGENT_MEMORY_SAFE_INSTALL=1` declines it there too.

When an install fails, the script reports only what it can verify. An unwritable `${OPENCLAW_STATE_DIR}/extensions` is named as the filesystem-permission failure that on its own would break the install — fix that directory, and do not touch the policy for it. Otherwise the `openclaw` output above the script's note is the evidence, and `security.installPolicy` appears only as a conditional to confirm there, never as an asserted cause: a host that advertises the bypass as a deprecated no-op says nothing about why an install failed.

Plugin contract ↔ agent-memory MCP tool mapping:

| OpenClaw contract | agent-memory MCP tool |
|---|---|
| `memory_search` | `memory_search` (BM25 default; `mode=vector\|hybrid` with embedding) |
| `memory_get` | `mem_read` |
| `memory_observe` | `memory_observe` |
| `memory_get_context` | `memory_get_context` |

Plugin config (via OpenClaw UI or `openclaw.json` `plugins.entries["memory-anolisa"].config`):

| Key | Default | Purpose |
|---|---|---|
| `binaryPath` | auto-discovery: `$PATH` → `/usr/bin/agent-memory` → `/usr/local/bin/agent-memory` → `~/.local/bin/agent-memory` | absolute binary path |
| `userId` | env `USER_ID` → OS `uid` → env `$USER` | namespace `user_id`; same validation as Rust side |
| `profile` | `advanced` | profile gate, passed as `MEMORY_PROFILE` env |
| `maxReadBytes` | `1048576` (1 MiB) | `mem_read` cap, passed as `MEMORY_MAX_READ_BYTES` |
| `maxWriteBytes` | `16777216` (16 MiB) | `mem_write` cap, passed as `MEMORY_MAX_WRITE_BYTES` |
| `sessionId` | env `MEMORY_SESSION_ID` → new `ses_<random>` | namespace session; must be fixed |
| `sessionDir` | env `MEMORY_SESSION_DIR` → `/run/anolisa/sessions` | session scratch + log root |

The plugin passes a minimal env allowlist to the subprocess (`PATH`, `HOME`, `USER`, `USER_ID`, `LANG`/`LC_ALL`/`LC_CTYPE`, `TZ`, `TMPDIR`, `XDG_RUNTIME_DIR`, and all `MEMORY_`/`RUST_`-prefixed vars); other env does not leak. `USER_ID` matches exactly — `USER_IDX` is not allowed.

---

## MCP tool set (37 tools)

All tools are invoked via MCP `tools/call` with JSON object arguments. Errors return `CallToolResult { isError: true }` so clients can distinguish business errors from "successful but content contains 'failed'". Profile is enforced at both `tools/list` and `tools/call`.

### Tier A — file operations (11)

| Tool | Required | Optional | Returns |
|------|------|------|------|
| `mem_read` | `path` | — | UTF-8 file content |
| `mem_write` | `path`, `content` | `overwrite` | `wrote N bytes to <path>` |
| `mem_append` | `path`, `content` | — | `appended N bytes to <path>` |
| `mem_edit` | `path`, `old_str`, `new_str` | — | `edited <path>` (`old_str` must match exactly once) |
| `mem_list` | — | `dir`, `recursive`, `glob` | `{name, type, size, mtime}` array |
| `mem_grep` | `pattern` | `dir`, `type`, `max`, `case_insensitive` | `{path, line, text}` array |
| `mem_diff` | `path1`, `path2` | — | unified diff |
| `mem_mkdir` | `path` | — | `created <path>` |
| `mem_remove` | `path` | `recursive` | `removed <path>` |
| `mem_promote` | `session_path`, `store_path` | — | atomically move session scratch file into the persistent store |
| `mem_session_log` | — | — | current session JSONL |

### Tier B — structured retrieval (6)

| Tool | Required | Optional | Returns |
|------|------|------|------|
| `memory_search` | `query` | `top_k` (default 5), `mode` (bm25/vector/hybrid), `category` | `{path, score, snippet, suspicious}` array |
| `memory_observe` | `content` | `hint`, `type` | `observed at notes/observed/<ulid>.md` |
| `memory_get_context` | — | `max_tokens` (default 2048) | markdown preview of recently modified files; each entry has `suspicious` |
| `memory_sessions` | — | `limit` (default 10) | historical session list |
| `memory_timeline` | `session_id` | `limit` (default 50) | tool-call timeline for a specific session |
| `mem_index_refresh` | — | — | force-rebuild the FTS5 index |

### Tier C — governance & versioning (7)

| Tool | Required | Optional | Returns |
|------|------|------|------|
| `mem_snapshot` | — | `name` | `{id, name, created_at, size, backend}` |
| `mem_snapshot_list` | — | — | array sorted by `created_at` |
| `mem_snapshot_restore` | `id` | — | `restored <id>` |
| `mem_log` | — | `limit` (default 20), `path` | `{hash, summary, author, time}` array (requires git) |
| `mem_revert` | `path` | — | `reverted <path> (commit <hash>)` (requires git) |
| `mem_consolidate` | — | — | `consolidation complete: N facts written` |
| `mem_compact` | — | — | `compacted N files to cold storage` |

### Sovereignty & import/export (13)

| Tool | Required | Optional | Returns / notes |
|------|------|------|------|
| `memory_about` | `topic` | `limit` (default 10) | matching memory paths and snippets for a topic |
| `memory_auto_created` | — | `limit` (default 20) | JSON array of auto-extracted facts |
| `memory_consent` | — | `action` (query/allow/deny), `scope` (all/consolidation/capture) | grant/revoke memory operations |
| `memory_forget` | `topic` | `confirm` (default `false`=preview, `true`=delete) | delete memory entries about a topic |
| `mem_export` | — | `category`, `source` | export the store as an AMA JSON string (does not write a file) |
| `mem_import` | `json_data` | `strategy` (skip-existing/overwrite, default skip-existing), `dry_run` (default false) | import memory from an AMA JSON string |
| `memory_task_save` | `title` | `status`, `progress`, `next_steps`, `blockers`, `files_modified`, `decisions`, `context`, `id` | save/update a task; returns the task id (pass `id` to update an existing task) |
| `memory_task_list` | — | `status` (in-progress/blocked/done/cancelled) | JSON array of task summaries |
| `memory_task_resume` | `id` | — | resume task context (formatted for continuing in a new session) |
| `memory_task_close` | `id` | `reason` | close a task (mark done) |
| `memory_summary` | — | `recent_limit` (default 10) | memory store statistics overview JSON |
| `memory_session_context` | — | `limit` | session-start context injection |
| `mem_dream` | — | — | user profile synthesis JSON |

### Error code semantics

| MCP code | Meaning |
|------------|------|
| `-32601` METHOD_NOT_FOUND | tool hidden by current profile |
| `-32602` INVALID_PARAMS | missing or wrong-type param |
| `-32603` INTERNAL_ERROR | server fault |
| `isError: true` | tool ran but returned a business error (path missing, sandbox rejection, size limit, etc.) |

---

## Core features

### File-form memory

Agents organize memory by path, matching the human filesystem model:

```
notes/day1.md
decisions/2026-05/db-pick.md
context/project-overview.md
```

Namespace layout:

```
~/.anolisa/memory/user-<uid>/        # mount root
├── README.md                        # auto-generated overview
├── notes/                           # free-form notes
├── decisions/                       # user-defined subdirs
└── .anolisa/                        # OS-managed, not writable by agents
    ├── manifest.toml                # namespace metadata
    ├── audit.log                    # JSONL tool-call audit
    ├── index.db                     # FTS5 SQLite
    ├── snapshots/                   # tar.gz archives + sidecar
    ├── trash/                       # entries retained on restore
    └── git/                         # bare git mirror (when git enabled)
```

Session dir (tmpfs, 0700):

```
/run/anolisa/sessions/<sid>/
├── meta.toml
├── log.jsonl
└── scratch/                         # session-only drafts; promoted via mem_promote
```

### Sandbox protection

Every file open is anchored at the mount root via kernel `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`:

- Rejects `..` traversal
- Rejects symlinks (including mid-call replacement; recursive deletes use `fdopendir` + `fstatat(AT_SYMLINK_NOFOLLOW)` + `unlinkat` so swaps can't race)
- Rejects access to metadata dirs (`.anolisa`, `.git`, `.gitignore` via `TargetIsReserved`)
- `mem_snapshot_restore` filters tar entry types — rejects `Symlink`/`Hardlink`/`Device`/`Fifo`
- Oversized payloads rejected per `max_*_bytes`

**Mount strategies**:

| Strategy | When | Behavior |
|------|------|------|
| `userland` (default) | any environment | mount is just a directory; sandbox enforced by `openat2` |
| `userns` | Linux ≥ 4.6 with unprivileged user namespace | `unshare` into a new user+mount namespace, mount a private tmpfs, then bind-mount the backing dir; host-side processes can't see `/mnt/memory/<ns>/` |
| `auto` | runtime probe | try `userns`; fall back to `userland` on any error |

### Version control

Optional auto git commit (libgit2 vendored):

```bash
MEMORY_GIT_ENABLED=true MEMORY_GIT_AUTO_COMMIT=true agent-memory
```

With git on, `mem_log` exposes change history and `mem_revert` gives the agent a real "undo" button. `mem_snapshot*` provides mount-wide tar.gz point-in-time backups independent of git.

### Full-text search

SQLite FTS5 BM25 index, sub-millisecond queries. A background tokio task watches the mount via `inotify`; events are debounced 200 ms and applied in a single transaction. Tokenizer is `trigram` (substring matching for ≥3-char terms). The trigram tokenizer emits one token per 3-character window, so a query term shorter than 3 characters (common for CJK words like "花名" / "小云") produces no tokens and would silently match nothing; `memory_search` detects this case and falls back to a `body LIKE '%term%'` substring scan so short CJK queries still recall. `IN_Q_OVERFLOW` triggers a full rescan — events are never silently dropped.

### Hybrid vector search

BM25 + dense vector hybrid retrieval, fused via RRF (Reciprocal Rank Fusion, k=60). Vectors come from a pluggable Embedding Provider:

| Provider | Configuration | Notes |
|----------|---------|------|
| OpenAI | `MEMORY_EMBEDDING_BACKEND=openai` + `OPENAI_API_KEY` | calls OpenAI Embeddings API |
| Ollama | `MEMORY_EMBEDDING_BACKEND=ollama` + `OLLAMA_BASE_URL` | local Ollama instance |

`memory_search` supports `mode`: `bm25` (default) / `vector` (cosine similarity) / `hybrid` (RRF fusion). Without embedding config, `vector`/`hybrid` auto-degrade to BM25 — no error.

### Auto consolidation

On shutdown, automatically extracts atomic facts from the session audit log (`mem_consolidate`) using 6 heuristic rules (zero LLM calls) — identifies high-frequency paths, search patterns, etc., and persists them as structured memory. Also manually triggerable via the `mem_consolidate` tool. Includes episodic memory extraction and conflict detection (BM25 threshold).

### Audit & observability

Every successful tool call appends a JSONL line to `<mount>/.anolisa/audit.log`; with sessions enabled, also to `/run/anolisa/sessions/<sid>/log.jsonl`. `audit.journald=true` fans out to systemd-journald with structured fields (`MESSAGE_ID`, `AGENT_MEMORY_TOOL`, etc.) for `journalctl --user-unit=anolisa-memory@<user>` filtering.

---

## Configuration

### Config file

Default location: `~/.anolisa/memory.toml`. The file is optional; Agent Memory
uses built-in defaults when it is absent. All structs enable
`serde(deny_unknown_fields)` — typos hard-fail at load. Minimal config:

```toml
[global]
user_id = "alice"

[memory]
profile = "advanced"           # basic | advanced | expert
max_read_bytes = 1048576       # 1 MiB
max_write_bytes = 16777216     # 16 MiB
max_append_bytes = 4194304     # 4 MiB

[memory.paths]
base_dir = "~/.anolisa/memory"

[memory.session]
base_dir = "/run/anolisa/sessions"
end_action = "discard"         # discard | keep

[memory.mount]
strategy = "auto"              # auto | userland | userns

[memory.index]
enabled = true
time_decay_lambda = 0.01
time_decay_alpha = 0.3
cold_after_days = 30
exclude_cold_on_search = true

[memory.audit]
journald = false

[memory.cgroup]
enabled = false
memory_max = "512M"

[memory.git]
enabled = false
auto_commit = true

[memory.consolidation]
enabled = true
max_facts = 20
min_tool_calls = 3
episodic_enabled = true
min_episode_steps = 3
max_episodes_per_session = 10
conflict_detection = true
conflict_bm25_threshold = -2.0
```

### Environment variables

Every config key has a matching `MEMORY_*` env var. Priority: **env > config.toml > default**.

| Variable | Description | Default |
|----------|------|------|
| `USER_ID` | user identity (validated; invalid values warn-and-ignore) | — |
| `MEMORY_PROFILE` | profile (basic/advanced/expert) | advanced |
| `MEMORY_BASE_DIR` | memory store root | `~/.anolisa/memory` |
| `MEMORY_SESSION_DIR` | session root | `/run/anolisa/sessions` |
| `MEMORY_SESSION_ID` | fixed session id (required for `mem_promote`) | new `ses_<random>` |
| `MEMORY_SESSION_END` | session end action (discard/keep) | discard |
| `MEMORY_MOUNT_STRATEGY` | mount strategy (auto/userland/userns) | auto |
| `MEMORY_MAX_READ_BYTES` | per-read cap | 1 MiB |
| `MEMORY_MAX_WRITE_BYTES` | per-write cap | 16 MiB |
| `MEMORY_MAX_APPEND_BYTES` | per-append cap | 4 MiB |
| `MEMORY_INDEX_ENABLED` | enable FTS5 index | true |
| `MEMORY_INDEX_TIME_DECAY_LAMBDA` | time decay (≥0) | 0.01 |
| `MEMORY_INDEX_TIME_DECAY_ALPHA` | time weight ratio (0–1) | 0.3 |
| `MEMORY_INDEX_COLD_AFTER_DAYS` | cold archive days | 30 |
| `MEMORY_INDEX_EXCLUDE_COLD` | exclude cold from search | true |
| `MEMORY_AUDIT_JOURNALD` | fan out to journald | false |
| `MEMORY_CGROUP_ENABLED` | enable cgroup limits | false |
| `MEMORY_CGROUP_MEMORY_MAX` | cgroup memory cap | 512M |
| `MEMORY_GIT_ENABLED` | enable git versioning | false |
| `MEMORY_GIT_AUTO_COMMIT` | auto commit | true |
| `MEMORY_EMBEDDING_BACKEND` | embedding backend (none/openai/ollama) | none |
| `MEMORY_OPENAI_API_KEY` | OpenAI API key (falls back to `OPENAI_API_KEY`) | — |
| `MEMORY_OPENAI_MODEL` | OpenAI embedding model | text-embedding-3-small |
| `MEMORY_OLLAMA_MODEL` | Ollama embedding model | nomic-embed-text |
| `MEMORY_OLLAMA_BASE_URL` | Ollama base URL | http://localhost:11434 |
| `MEMORY_CONSOLIDATION_ENABLED` | enable auto consolidation | true |
| `MEMORY_CONSOLIDATION_MAX_FACTS` | max facts per run | 20 |
| `MEMORY_CONSOLIDATION_MIN_CALLS` | min tool-call threshold | 3 |
| `MEMORY_EPISODIC_ENABLED` | episodic extraction | true |
| `MEMORY_MIN_EPISODE_STEPS` | min episode steps | 3 |
| `MEMORY_MAX_EPISODES` | max episodes per session | 10 |
| `MEMORY_CONFLICT_DETECTION` | conflict detection | true |
| `MEMORY_CONFLICT_THRESHOLD` | BM25 conflict threshold | -2.0 |

Data storage: `~/.anolisa/memory/<namespace>/`.

### Profiles

Profiles are UX hints, not security boundaries, but enforced at both `tools/list` and `tools/call`:

- **basic** — all 37 tools shown; weaker models can use the Tier B structured API.
- **advanced** (default) — all 37 tools shown; stronger models should prefer Tier A file ops.
- **expert** — hides Tier B (`memory_search`, `memory_observe`, `memory_get_context`, `mem_consolidate`, `memory_forget`, `memory_consent`); `tools/call` returns `METHOD_NOT_FOUND`. For proficient models that only need Tier A and Tier C.

### Embedding config

```toml
[memory.embedding]
backend = "openai"                # or "ollama"
api_key = ""                      # empty: auto-read OPENAI_API_KEY
model = "text-embedding-3-small"
# Ollama: backend = "ollama", model = "nomic-embed-text", base_url = "http://localhost:11434"
```

---

## Use cases

- Cross-session persistence of notes and decisions (Claude Code, Cursor, Continue, custom rmcp clients).
- Multi-agent systems where Agent A writes and Agent B reads shared notes.
- Operation audit and state recovery (`mem_log`, JSONL audit, journald, `mem_revert`, `mem_snapshot_restore`).
- Multi-turn "draft first, persist when decided" pattern (`mem_promote` atomically moves files from session scratch into the persistent store).

---

## SDK / client integration

### Python (official `mcp` SDK)

```python
import asyncio
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

async def main():
    server = StdioServerParameters(
        command="/usr/bin/agent-memory", args=[],
        env={"USER_ID": "alice"},
    )
    async with stdio_client(server) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            tools = await session.list_tools()
            print([t.name for t in tools.tools])
            result = await session.call_tool(
                "mem_write",
                {"path": "notes/from-python.md", "content": "hello"},
            )
            assert not result.isError

asyncio.run(main())
```

### TypeScript (`@modelcontextprotocol/sdk`)

```typescript
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const transport = new StdioClientTransport({
  command: "/usr/bin/agent-memory", args: [],
  env: { USER_ID: "alice" },
});
const client = new Client({ name: "my-app", version: "1.0.0" }, {});
await client.connect(transport);
const result = await client.callTool({
  name: "mem_grep",
  arguments: { pattern: "TODO", recursive: true, max: 50 },
});
```

### Rust (`rmcp`)

```rust
use rmcp::transport::child_process::ChildProcessTransport;
use rmcp::ServiceExt;

let transport = ChildProcessTransport::new(
    tokio::process::Command::new("/usr/bin/agent-memory"),
).await?;
let client = ().serve(transport).await?;
let tools = client.list_tools(Default::default()).await?;
```

### Promote workflow (multi-turn)

1. Set `MEMORY_SESSION_ID=<sid>` and `MEMORY_SESSION_DIR=/run/anolisa/sessions` for each agent run.
2. Agent writes drafts to `/run/anolisa/sessions/<sid>/scratch/`.
3. When a draft is worth keeping, the agent calls `mem_promote` to atomically move it into the persistent store.

---

## Testing & verification

### Automated tests

```bash
cd src/agent-memory
cargo fmt --check
cargo clippy -- -D warnings
cargo test                              # full suite
cargo test --test e2e_agent_test        # tool E2E
cargo test --test mcp_integration_test  # protocol layer
cargo test --test linux_userns_test -- --ignored  # needs unprivileged userns
make smoke                              # one-shot end-to-end smoke
```

CI runs `fmt --check` + `clippy -D warnings` + `cargo test` on Rust 1.89.

### Interactive `mcp-harness`

```bash
cargo run --example mcp-harness -- /tmp/mem-test
```

| Command | Description |
|------|------|
| `list` | list visible tools |
| `call <tool> <json_args>` | invoke a tool |
| `help` | help |
| `quit` | quit |

Scenarios: `--scenario full` / `git --git` / `promote` / `--verbose` (prints JSON-RPC).

### Raw JSON-RPC (protocol-level debugging)

```bash
mkdir -p /tmp/mem-test/__sessions__
MEMORY_BASE_DIR=/tmp/mem-test \
MEMORY_SESSION_DIR=/tmp/mem-test/__sessions__ \
MEMORY_MOUNT_STRATEGY=userland \
USER_ID=tester \
agent-memory
```

Handshake + tool call:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"manual","version":"1.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mem_write","arguments":{"path":"test.md","content":"hello"}}}
```

### Sandbox escape verification

```json
{"name":"mem_read","arguments":{"path":"../../etc/passwd"}}
```
→ `isError: true`, message `path outside mount root`.

```json
{"name":"mem_write","arguments":{"path":".anolisa/audit.log","content":"x"}}
```
→ `isError: true`, message `target is reserved`.

---

## Troubleshooting

### Diagnostic tools

```bash
# Component-level diagnosis (follow the reported fix plan manually)
anolisa doctor agent-memory

# Adapter status
anolisa adapter status agent-memory

# Debug startup
RUST_LOG=agent_memory=debug agent-memory
```

### Common issues

| Symptom | Likely cause | Fix |
|------|----------|------|
| startup `unshare(NEWUSER\|NEWNS): EPERM` | unprivileged user namespace disabled | `sysctl kernel.unprivileged_userns_clone=1`, or `MEMORY_MOUNT_STRATEGY=userland` |
| `tmpfs /mnt: EBUSY` | `/mnt` occupied in new namespace | restart the process |
| macOS / Windows `cargo build` fails on `libsystemd`/`nix` | non-Linux host | `make remote-build` / `remote-test` |
| `tools/call memory_search` returns `METHOD_NOT_FOUND` | `MEMORY_PROFILE=expert` hides Tier B | switch to `advanced`, or use Tier A directly |
| config typos silently ignored | — | now hard-fail; check startup stderr |
| `mem_log` returns `[]` despite writes | git versioning not enabled | `MEMORY_GIT_ENABLED=true MEMORY_GIT_AUTO_COMMIT=true` |
| search misses just-written content | inside the 200 ms debounce window | retry, or use `mem_grep` (regex on the filesystem, no index) |
| `mem_promote` reports `session not found` | `MEMORY_SESSION_ID`/`MEMORY_SESSION_DIR` unset or scratch missing | see Promote workflow |
| OpenClaw plugin not loaded | `openclaw` CLI not on PATH | rerun `install.sh` after installing OpenClaw |
| install.sh reports `Plugin "memory-anolisa" requires capability consent` | OpenClaw >= 2026.8.1 consent gate; installer-options probe failed, `AGENT_MEMORY_ACCEPT_CAPABILITIES=0` is set, or script predates the fix | check install output for the probe WARNING or opt-out refusal line; update agent-memory, unset the opt-out, or run `openclaw plugins install <plugin-dir> --force --accept-capabilities` manually. A withheld install rejected by the gate exits with code 3; if OpenClaw rewords the rejection message, the script falls back to exit 1 with the opt-out note |
| install.sh reports the install target is not writable | `${OPENCLAW_STATE_DIR}/extensions` (or its nearest existing parent) is not writable by the user running the script, so OpenClaw's `mkdir extensions/memory-anolisa` fails with `EACCES` | fix that directory's ownership/permissions — or point `OPENCLAW_STATE_DIR` at a writable state directory — and re-run. This is a filesystem failure, not a policy refusal: do not relax `security.installPolicy` for it |
| install.sh fails on a host that lists `--dangerously-force-unsafe-install` as a deprecated no-op | OpenClaw 2026.6.5+ runs no install-time scan, so the script sent no bypass and cannot shape install-time safety there; the cause is in the `openclaw` output | read the CLI output above the script's note. Only if it names `security.installPolicy` is that operator-owned policy what to relax — re-running the script or setting `AGENT_MEMORY_SAFE_INSTALL` cannot override it |
| install.sh reports the safety scan blocked the plugin | OpenClaw 2026.6.1 or earlier scans plugin sources at install time and flags the plugin's `child_process.spawn` MCP transport | unset `AGENT_MEMORY_SAFE_INSTALL` so the script passes the bypass it declined, or upgrade OpenClaw |
| system state out of sync after manual dnf | — | `sudo anolisa --install-mode system repair agent-memory`; use system-scoped `forget` / `adopt` only when intentionally rebuilding the record for a present RPM |

For deeper investigation: start with `RUST_LOG=agent_memory=debug` and inspect both stderr and `<mount>/.anolisa/audit.log`.

---

**License**: Apache-2.0
**Version**: 0.2.1
**Document version**: 2.0 (aligned with ANOLISA-design user-guide structure)
