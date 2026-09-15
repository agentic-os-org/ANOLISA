# Agent Memory

[中文版](README_zh.md)

CMA-style persistent filesystem memory for AI agents, served over MCP. Provides sandboxed file tools, hybrid BM25 + vector search, auto capture/recall, git versioning, and tar.gz snapshots. Agent Memory is a memory component of [ANOLISA](../../README.md). Linux only.

## Features

- **File-form memory** — read/write with filesystem semantics via 37 MCP tools; namespace isolation and path sandboxing (openat2 RESOLVE_BENEATH)
- **Hybrid semantic search** — BM25 keyword + dense vector embeddings with reciprocal rank fusion (RRF); time-decay ranking
- **Auto capture & recall** — observes at conversation end, injects relevant context before the next prompt
- **Memory consolidation** — automatic extraction of atomic facts from session audit logs
- **Versioning & snapshots** — optional git auto-commit + tar.gz snapshots for file-level and mount-level rollback
- **Safety** — prompt-injection detection and secret/PII redaction for injected content
- **Cross-session tasks** — save/resume/close tasks across sessions with full context

## Quick Start

### Install

```bash
# Recommended
anolisa install agent-memory

# Or via RPM (Alinux)
sudo yum install agent-memory
```

### OpenClaw adapter

The bundled plugin (`memory-anolisa`) is deployed by
`/usr/share/anolisa/adapters/agent-memory/openclaw/scripts/install.sh`, which
grants the plugin's declared capabilities by default. Set
`AGENT_MEMORY_ACCEPT_CAPABILITIES=0` to withhold consent — on hosts that gate
consent the install then fails until consent is granted interactively. Set
`AGENT_MEMORY_SAFE_INSTALL=1` to decline the unsafe-install bypass on hosts
that would still receive one. Full reference:
[user guide](../../docs/user-guide/en/token-saving/agent-memory.md).

```bash
bash /usr/share/anolisa/adapters/agent-memory/openclaw/scripts/install.sh
openclaw gateway restart
```

Both optional installer flags are negotiated from `openclaw plugins install
--help`, but the two switches are not symmetric. `--accept-capabilities` is
passed only when the host advertises that exact option — current hosts do, so
`AGENT_MEMORY_ACCEPT_CAPABILITIES` still shapes the argv there: `1` appends
`--accept-capabilities` to `openclaw plugins install <dir> --force`, while `0`
omits it and a host that gates consent then rejects the install.
`--dangerously-force-unsafe-install` is passed only while the host still
advertises the bypass as effective (OpenClaw 2026.6.1 and earlier). Current
hosts list it as a deprecated no-op and never receive it under either setting,
so `AGENT_MEMORY_SAFE_INSTALL` changes nothing there and install-time safety
follows the operator-owned `security.installPolicy`. The install log states
which case applied.

### Integration (MCP client)

Add to your MCP config (Claude Code, Cursor, etc.):

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

### Core Operations

```bash
# Initialize namespace
agent-memory init

# Print resolved config
agent-memory info
```

Once running as MCP server, agents interact via tools:

| Operation | MCP Tool |
|-----------|----------|
| Write memory | `mem_write(path, content)` |
| Read memory | `mem_read(path)` |
| Search | `memory_search(query, mode="hybrid")` |
| Observe | `memory_observe(content, type)` |
| Get context | `memory_get_context(max_tokens)` |
| Snapshot | `mem_snapshot(name)` |

## Architecture

Single-process Tokio async runtime exposing 37 MCP tools over stdio JSON-RPC 2.0:

- **Tier A** (11 tools): file operations — read, write, append, edit, list, grep, diff, mkdir, remove, promote, session_log
- **Tier B** (6 tools): structured retrieval — search, observe, get_context, sessions, timeline, index_refresh
- **Tier C** (7 tools): governance — snapshot, restore, git log/revert, consolidate, compact
- **Sovereignty** (13 tools): about, forget, consent, export/import, tasks, dream

Profile gating (basic/advanced/expert) controls tool visibility per deployment.
The OpenClaw adapter forwards this profile to the child and accepts `basic`/`advanced`
only: `expert` hides the Tier B tools its memory contract is built on, so the plugin
refuses to load with it.

## Requirements

- Linux (x86_64 / aarch64)
- Rust ≥ 1.85 (for source build)
- Node.js ≥ 20 and npm (for source build — bundles the OpenClaw adapter)
- Optional: embedding provider (OpenAI or Ollama) for vector search

## License

Apache License 2.0 — see [LICENSE](LICENSE).
