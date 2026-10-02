//! Tier A MCP tools — exposes 10 file operations to MCP clients.

use std::sync::Arc;

use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CallToolRequestParam, CallToolResult, ErrorCode, ErrorData, Implementation, ListToolsResult,
    PaginatedRequestParam, ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ServerHandler, tool};

use crate::service::MemoryService;
use crate::tools::{GrepOptions, ListOptions};

#[derive(Clone)]
pub struct MemoryMcpServer {
    svc: Arc<MemoryService>,
}

impl MemoryMcpServer {
    pub fn new(svc: Arc<MemoryService>) -> Self {
        Self { svc }
    }

    /// Single source of truth for the active profile. `tools/list` and
    /// `tools/call` both gate on this; centralising the read makes it
    /// trivial to switch to a runtime-mutable profile later without
    /// touching multiple call sites.
    fn profile(&self) -> crate::config::Profile {
        self.svc.config.memory.profile
    }
}

fn fmt_err<E: std::fmt::Display>(prefix: &str, e: E) -> String {
    format!("{prefix}: {e}")
}

// All Tier A/B/C tool functions return `Result<String, String>`. rmcp's
// `IntoCallToolResult` impl maps `Err(_)` to `CallToolResult::error(...)`
// with `isError: true`, which is what MCP clients need to distinguish a
// real failure from a successful call whose payload happens to start
// with "failed". Returning the bare success string from the previous
// implementation made every error look like a normal text result.
type ToolResult = Result<String, String>;

// ---- Tier A tool implementations ----

impl MemoryMcpServer {
    #[tool(
        description = "Read a UTF-8 text file from the memory store. Returns the file's full contents."
    )]
    async fn mem_read(&self, #[tool(param)] path: String) -> ToolResult {
        self.svc.read(&path).map_err(|e| fmt_err("read failed", e))
    }

    #[tool(
        description = "Write a UTF-8 text file. Creates parent directories. Set overwrite=true to replace existing."
    )]
    async fn mem_write(
        &self,
        #[tool(param)] path: String,
        #[tool(param)] content: String,
        #[tool(param)] overwrite: Option<bool>,
    ) -> ToolResult {
        self.svc
            .write(&path, &content, overwrite.unwrap_or(false))
            .map(|n| format!("wrote {n} bytes to {path}"))
            .map_err(|e| fmt_err("write failed", e))
    }

    #[tool(description = "Append UTF-8 text to a file (creates if missing).")]
    async fn mem_append(
        &self,
        #[tool(param)] path: String,
        #[tool(param)] content: String,
    ) -> ToolResult {
        self.svc
            .append(&path, &content)
            .map(|n| format!("appended {n} bytes to {path}"))
            .map_err(|e| fmt_err("append failed", e))
    }

    #[tool(
        description = "Replace exactly one occurrence of old_str with new_str in a file. Errors if old_str matches zero or multiple times."
    )]
    async fn mem_edit(
        &self,
        #[tool(param)] path: String,
        #[tool(param)] old_str: String,
        #[tool(param)] new_str: String,
    ) -> ToolResult {
        self.svc
            .edit(&path, &old_str, &new_str)
            .map(|()| format!("edited {path}"))
            .map_err(|e| fmt_err("edit failed", e))
    }

    #[tool(
        description = "List entries under a directory. Empty dir means mount root. recursive=true walks the tree (max depth 16). glob filters by path pattern (e.g. **/*.md)."
    )]
    async fn mem_list(
        &self,
        #[tool(param)] dir: Option<String>,
        #[tool(param)] recursive: Option<bool>,
        #[tool(param)] glob: Option<String>,
    ) -> ToolResult {
        let d = dir.unwrap_or_default();
        let opts = ListOptions {
            recursive: recursive.unwrap_or(false),
            glob,
        };
        let entries = self
            .svc
            .list(&d, opts)
            .map_err(|e| fmt_err("list failed", e))?;
        serde_json::to_string_pretty(&entries).map_err(|e| fmt_err("list serialize failed", e))
    }

    #[tool(
        description = "Search files for a regex pattern. Returns matches as JSON: [{path, line, text}]. Honors r#type as a glob filter and max as result cap."
    )]
    async fn mem_grep(
        &self,
        #[tool(param)] pattern: String,
        #[tool(param)] dir: Option<String>,
        #[tool(param)] r#type: Option<String>,
        #[tool(param)] max: Option<u32>,
        #[tool(param)] case_insensitive: Option<bool>,
    ) -> ToolResult {
        let opts = GrepOptions {
            dir: dir.unwrap_or_default(),
            r#type,
            max: max.map(|m| m as usize),
            case_insensitive: case_insensitive.unwrap_or(false),
        };
        let hits = self
            .svc
            .grep(&pattern, opts)
            .map_err(|e| fmt_err("grep failed", e))?;
        serde_json::to_string_pretty(&hits).map_err(|e| fmt_err("grep serialize failed", e))
    }

    #[tool(description = "Show a unified diff between two text files in the memory store.")]
    async fn mem_diff(
        &self,
        #[tool(param)] path1: String,
        #[tool(param)] path2: String,
    ) -> ToolResult {
        self.svc
            .diff(&path1, &path2)
            .map_err(|e| fmt_err("diff failed", e))
    }

    #[tool(description = "Create a directory (with parents). Idempotent.")]
    async fn mem_mkdir(&self, #[tool(param)] path: String) -> ToolResult {
        self.svc
            .mkdir(&path)
            .map(|()| format!("created {path}"))
            .map_err(|e| fmt_err("mkdir failed", e))
    }

    #[tool(
        description = "Remove a file or directory. recursive=true is required to remove non-empty directories."
    )]
    async fn mem_remove(
        &self,
        #[tool(param)] path: String,
        #[tool(param)] recursive: Option<bool>,
    ) -> ToolResult {
        self.svc
            .remove(&path, recursive.unwrap_or(false))
            .map(|()| format!("removed {path}"))
            .map_err(|e| fmt_err("remove failed", e))
    }

    #[tool(
        description = "Promote a file from the active session's scratch/ to the persistent Memory Store. The destination path is relative to the mount root and must not already exist."
    )]
    async fn mem_promote(
        &self,
        #[tool(param)] session_path: String,
        #[tool(param)] store_path: String,
    ) -> ToolResult {
        crate::tools::promote::promote(&self.svc, &session_path, &store_path)
            .map(|n| format!("promoted {n} bytes: {session_path} -> {store_path}"))
            .map_err(|e| fmt_err("promote failed", e))
    }

    #[tool(
        description = "Read this session's running JSONL tool-call log. Useful for the model to see what it has done in the current session."
    )]
    async fn mem_session_log(&self) -> ToolResult {
        let s = self
            .svc
            .session_log()
            .map_err(|e| fmt_err("session_log failed", e))?;
        Ok(if s.is_empty() {
            "(session log is empty)".to_string()
        } else {
            s
        })
    }

    // ---- Tier B: structured search/write API for weak models or batch use ----

    #[tool(
        description = "Tier B: search the indexed memory store. Default BM25 keyword search. Set mode=vector for semantic (embedding) search, or mode=hybrid for combined ranking. Requires [memory.embedding] config for vector/hybrid. Optional category filters results to a fact category (e.g. 'lesson', 'interest', 'working-context'). Optional agent_scope overrides [memory].agent_scope for this call; accepted values: 'shared' (no filtering), 'isolated:<id>' (only memories tagged with <id>), or 'filter:<id>' (<id>'s own memories plus unscoped ones). When agent_scope is set to isolated/filter, vector/hybrid degrade to scoped BM25 so the isolation boundary is preserved."
    )]
    async fn memory_search(
        &self,
        #[tool(param)] query: String,
        #[tool(param)] top_k: Option<u32>,
        #[tool(param)] mode: Option<String>,
        #[tool(param)] category: Option<String>,
        #[tool(param)] agent_scope: Option<String>,
    ) -> ToolResult {
        // Reject excessively long queries to prevent FTS5 resource exhaustion.
        const MAX_QUERY_LEN: usize = 1024;
        if query.len() > MAX_QUERY_LEN {
            return Err(format!(
                "query too long ({} chars, max {MAX_QUERY_LEN})",
                query.len()
            ));
        }
        let k = top_k.unwrap_or(5) as usize;
        let hits = self
            .svc
            .memory_search(
                &query,
                k,
                mode.as_deref(),
                category.as_deref().filter(|s| !s.is_empty()),
                agent_scope.as_deref().filter(|s| !s.is_empty()),
            )
            .map_err(|e| fmt_err("search failed", e))?;
        serde_json::to_string_pretty(&hits).map_err(|e| fmt_err("search serialize failed", e))
    }

    #[tool(
        description = "Tier B: record an observation with optional type classification. Types: 'user' (preferences/profile), 'feedback' (behavior corrections), 'project' (decisions/status), 'reference' (external resource pointers). Only store non-derivable information — code patterns and file structure can be retrieved in real-time. Returns the relative path."
    )]
    async fn memory_observe(
        &self,
        #[tool(param)] content: String,
        #[tool(param)] hint: Option<String>,
        #[tool(param)] r#type: Option<String>,
    ) -> ToolResult {
        self.svc
            .memory_observe(&content, hint.as_deref(), r#type.as_deref())
            .map(|path| format!("observed at {path}"))
            .map_err(|e| fmt_err("observe failed", e))
    }

    #[tool(
        description = "Tier B: assemble a token-bounded context from the most recently modified memory files. Returns markdown with previews, capped at roughly max_tokens*4 bytes."
    )]
    async fn memory_get_context(&self, #[tool(param)] max_tokens: Option<u32>) -> ToolResult {
        let n = max_tokens.unwrap_or(2048) as usize;
        self.svc
            .memory_get_context(n)
            .map_err(|e| fmt_err("get_context failed", e))
    }

    #[tool(
        description = "List historical session summaries from facts/summary/. Returns session_id, created_at, tool_calls, and description."
    )]
    async fn memory_sessions(&self, #[tool(param)] limit: Option<u32>) -> ToolResult {
        crate::tools::session_history::memory_sessions(&self.svc, limit.unwrap_or(10) as usize)
            .map_err(|e| fmt_err("memory_sessions failed", e))
    }

    #[tool(
        description = "Show tool call timeline for a specific session. Returns timestamped entries with tool, path, status."
    )]
    async fn memory_timeline(
        &self,
        #[tool(param)] session_id: String,
        #[tool(param)] limit: Option<u32>,
    ) -> ToolResult {
        crate::tools::session_history::memory_timeline(
            &self.svc,
            &session_id,
            limit.unwrap_or(50) as usize,
        )
        .map_err(|e| fmt_err("memory_timeline failed", e))
    }
    #[tool(description = "Build a memory store overview with statistics.")]
    async fn memory_summary(&self, #[tool(param)] recent_limit: Option<u32>) -> ToolResult {
        let summary = crate::tools::memory_summary_tool::memory_summary(
            &self.svc,
            recent_limit.unwrap_or(10) as usize,
        )
        .map_err(|e| fmt_err("memory_summary failed", e))?;
        serde_json::to_string_pretty(&summary)
            .map_err(|e| fmt_err("memory_summary serialize failed", e))
    }

    #[tool(description = "Refresh the MEMORY.md index file from all memory entries.")]
    async fn mem_index_refresh(&self) -> ToolResult {
        let n = crate::tools::memory_index::refresh_index(&self.svc)
            .map_err(|e| fmt_err("index_refresh failed", e))?;
        Ok(format!("refreshed MEMORY.md with {n} entries"))
    }

    #[tool(
        description = "Build session-start context from recent summaries and high-confidence memories."
    )]
    async fn memory_session_context(&self, #[tool(param)] limit: Option<u32>) -> ToolResult {
        crate::tools::session_context::memory_session_context(&self.svc, limit.map(|l| l as usize))
            .map_err(|e| fmt_err("session_context failed", e))
    }

    #[tool(description = "Synthesize user profile from memories.")]
    async fn mem_dream(&self) -> ToolResult {
        let profile = crate::tools::user_profile::synthesize_profile(&self.svc)
            .map_err(|e| fmt_err("mem_dream failed", e))?;
        serde_json::to_string_pretty(&profile).map_err(|e| fmt_err("mem_dream serialize failed", e))
    }

    // ---- Tier C: governance (snapshots) ----

    #[tool(
        description = "Create a snapshot of the memory store at this point in time. Returns the snapshot id and metadata. Excludes .anolisa/. Optional `name` is a human label."
    )]
    async fn mem_snapshot(&self, #[tool(param)] name: Option<String>) -> ToolResult {
        let info = self
            .svc
            .mem_snapshot(name.as_deref())
            .map_err(|e| fmt_err("snapshot failed", e))?;
        serde_json::to_string_pretty(&info).map_err(|e| fmt_err("snapshot serialize failed", e))
    }

    #[tool(
        description = "List all snapshots in this namespace, oldest → newest. Returns JSON array of {id, name, created_at, size, backend}."
    )]
    async fn mem_snapshot_list(&self) -> ToolResult {
        let infos = self
            .svc
            .mem_snapshot_list()
            .map_err(|e| fmt_err("snapshot_list failed", e))?;
        serde_json::to_string_pretty(&infos)
            .map_err(|e| fmt_err("snapshot_list serialize failed", e))
    }

    #[tool(
        description = "Restore a snapshot by id. All current files (except .anolisa/) are replaced by the archive contents. Each top-level entry is swapped via a single rename(2): a crash mid-restore leaves the prior state intact with hidden `.<id>.rollback.*` entries under .anolisa/. Concurrent reads of an individual path see either old or new content (or briefly ENOENT during that path's rename window), never a half-written file."
    )]
    async fn mem_snapshot_restore(&self, #[tool(param)] id: String) -> ToolResult {
        self.svc
            .mem_snapshot_restore(&id)
            .map(|()| format!("restored {id}"))
            .map_err(|e| fmt_err("snapshot_restore failed", e))
    }

    // ---- Tier C: governance (git versioning) ----

    #[tool(
        description = "List recent git commits for this memory mount, newest first. Returns JSON [{hash, summary, author, time}]. Optional `path` filters to commits touching that file. Errors when git versioning isn't enabled."
    )]
    async fn mem_log(
        &self,
        #[tool(param)] limit: Option<u32>,
        #[tool(param)] path: Option<String>,
    ) -> ToolResult {
        let n = limit.unwrap_or(20) as usize;
        let entries = self
            .svc
            .mem_log(n, path.as_deref())
            .map_err(|e| fmt_err("mem_log failed", e))?;
        serde_json::to_string_pretty(&entries).map_err(|e| fmt_err("mem_log serialize failed", e))
    }

    #[tool(
        description = "Revert a single file to its content at HEAD, then commit the revert. Useful for undoing the most recent uncommitted edit. Errors when git versioning isn't enabled."
    )]
    async fn mem_revert(&self, #[tool(param)] path: String) -> ToolResult {
        self.svc
            .mem_revert(&path)
            .map(|hash| format!("reverted {path} (commit {hash})"))
            .map_err(|e| fmt_err("mem_revert failed", e))
    }

    // ---- Consolidation (auto + manual trigger) ----

    #[tool(
        description = "Manually trigger memory consolidation: analyse the current session's audit log and extract atomic facts (L1 memories). Auto-consolidation also runs on shutdown. Returns the number of facts extracted."
    )]
    async fn mem_consolidate(&self) -> ToolResult {
        let n = self.svc.consolidate();
        Ok(format!("consolidation complete: {n} facts written"))
    }

    // ---- Index compaction (cold archival) ----

    #[tool(
        description = "Compact the memory index: mark old, never-searched files as cold. Cold files are excluded from normal search but still available via deep search. Returns the number of files compacted."
    )]
    async fn mem_compact(&self) -> ToolResult {
        self.svc
            .compact()
            .map(|n| format!("compacted {n} files to cold storage"))
            .map_err(|e| fmt_err("compact failed", e))
    }

    // ---- Task model (cross-session task persistence) ----

    #[tool(
        description = "Save or update a task for cross-session persistence. Tasks track title, status (in-progress/blocked/done/cancelled), progress (0-100%), next steps, blockers, files modified, and decisions. If 'id' is provided and exists, the task is updated; otherwise a new task is created. Returns the task id."
    )]
    #[allow(clippy::too_many_arguments)]
    async fn memory_task_save(
        &self,
        #[tool(param)] title: String,
        #[tool(param)] status: Option<String>,
        #[tool(param)] progress: Option<u32>,
        #[tool(param)] next_steps: Option<Vec<String>>,
        #[tool(param)] blockers: Option<Vec<String>>,
        #[tool(param)] files_modified: Option<Vec<String>>,
        #[tool(param)] decisions: Option<Vec<String>>,
        #[tool(param)] context: Option<String>,
        #[tool(param)] id: Option<String>,
    ) -> ToolResult {
        crate::tools::memory_task::memory_task_save(
            &self.svc,
            &title,
            status.as_deref(),
            progress.map(|p| p as u8),
            next_steps,
            blockers,
            files_modified,
            decisions,
            context.as_deref(),
            id.as_deref(),
        )
        .map_err(|e| fmt_err("task_save failed", e))
    }

    #[tool(
        description = "Resume a task by id: returns the full task context (title, status, progress, next steps, blockers, files, decisions, context) formatted for continuing work in a new session."
    )]
    async fn memory_task_resume(&self, #[tool(param)] id: String) -> ToolResult {
        crate::tools::memory_task::memory_task_resume(&self.svc, &id)
            .map_err(|e| fmt_err("task_resume failed", e))
    }

    #[tool(
        description = "List tasks. By default shows active tasks (in-progress + blocked). Optional status filter: 'in-progress', 'blocked', 'done', 'cancelled'. Returns JSON array of task summaries."
    )]
    async fn memory_task_list(&self, #[tool(param)] status: Option<String>) -> ToolResult {
        crate::tools::memory_task::memory_task_list(&self.svc, status.as_deref())
            .map_err(|e| fmt_err("task_list failed", e))
    }

    #[tool(
        description = "Close a task (mark as done). Optional reason is appended to the task context. Returns confirmation."
    )]
    async fn memory_task_close(
        &self,
        #[tool(param)] id: String,
        #[tool(param)] reason: Option<String>,
    ) -> ToolResult {
        crate::tools::memory_task::memory_task_close(&self.svc, &id, reason.as_deref())
            .map_err(|e| fmt_err("task_close failed", e))
    }

    #[tool(
        description = "Export the memory store to AMA (Anolisa Memory Archive) JSON. Optional category and source filters."
    )]
    async fn mem_export(
        &self,
        #[tool(param)] category: Option<String>,
        #[tool(param)] source: Option<String>,
    ) -> ToolResult {
        let filter = crate::tools::memory_export::ExportFilter {
            category,
            source,
            include_tasks: true,
        };
        crate::tools::memory_export::memory_export(&self.svc, &filter)
            .map_err(|e| fmt_err("export failed", e))
    }

    #[tool(
        description = "Import memories from AMA JSON. Strategy: 'skip-existing' (default) or 'overwrite'. Set dry_run=true for preview."
    )]
    async fn mem_import(
        &self,
        #[tool(param)] json_data: String,
        #[tool(param)] strategy: Option<String>,
        #[tool(param)] dry_run: Option<bool>,
    ) -> ToolResult {
        let strat = match strategy.as_deref().unwrap_or("skip-existing") {
            "overwrite" => crate::tools::memory_import::ImportStrategy::Overwrite,
            "skip-existing" => crate::tools::memory_import::ImportStrategy::SkipExisting,
            other => {
                return Err(format!("unknown strategy: {other}"));
            }
        };
        crate::tools::memory_import::memory_import(
            &self.svc,
            &json_data,
            strat,
            dry_run.unwrap_or(false),
        )
        .map_err(|e| fmt_err("import failed", e))
    }

    #[tool(
        description = "Search for all memories related to a topic. Returns matching paths and snippets."
    )]
    async fn memory_about(
        &self,
        #[tool(param)] topic: String,
        #[tool(param)] limit: Option<u32>,
    ) -> ToolResult {
        crate::tools::memory_sovereignty::memory_about(
            &self.svc,
            &topic,
            limit.unwrap_or(10) as usize,
        )
        .map_err(|e| fmt_err("memory_about failed", e))
    }

    #[tool(
        description = "Forget memories about a topic. Set confirm=false for preview, confirm=true to delete."
    )]
    async fn memory_forget(
        &self,
        #[tool(param)] topic: String,
        #[tool(param)] confirm: Option<bool>,
    ) -> ToolResult {
        crate::tools::memory_sovereignty::memory_forget(&self.svc, &topic, confirm.unwrap_or(false))
            .map_err(|e| fmt_err("memory_forget failed", e))
    }

    #[tool(
        description = "List all auto-created memories for review. Returns JSON array of entries."
    )]
    async fn memory_auto_created(&self, #[tool(param)] limit: Option<u32>) -> ToolResult {
        crate::tools::memory_sovereignty::memory_auto_created(
            &self.svc,
            limit.unwrap_or(20) as usize,
        )
        .map_err(|e| fmt_err("memory_auto_created failed", e))
    }

    #[tool(
        description = "Manage auto-memory consent. Action: 'query', 'allow', or 'deny'. Scope: 'all', 'consolidation', or 'capture'."
    )]
    async fn memory_consent(
        &self,
        #[tool(param)] action: Option<String>,
        #[tool(param)] scope: Option<String>,
    ) -> ToolResult {
        crate::tools::memory_sovereignty::memory_consent(
            &self.svc,
            action.as_deref(),
            scope.as_deref(),
        )
        .map_err(|e| fmt_err("memory_consent failed", e))
    }
}

rmcp::tool_box!(MemoryMcpServer {
    mem_read,
    mem_write,
    mem_append,
    mem_edit,
    mem_list,
    mem_grep,
    mem_diff,
    mem_mkdir,
    mem_remove,
    mem_promote,
    mem_session_log,
    memory_search,
    memory_observe,
    memory_get_context,
    memory_sessions,
    memory_timeline,
    memory_summary,
    memory_session_context,
    mem_dream,
    mem_index_refresh,
    mem_snapshot,
    mem_snapshot_list,
    mem_snapshot_restore,
    mem_log,
    mem_revert,
    mem_consolidate,
    mem_compact,
    memory_task_save,
    memory_task_resume,
    memory_task_list,
    memory_task_close,
    mem_export,
    mem_import,
    memory_about,
    memory_forget,
    memory_auto_created,
    memory_consent,
} memory_tool_box);

impl ServerHandler for MemoryMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: Default::default(),
            capabilities: ServerCapabilities {
                tools: Some(Default::default()),
                ..Default::default()
            },
            server_info: Implementation {
                name: "agent-memory".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            instructions: Some(
                "Persistent file-based memory mounted under \
                 ~/.anolisa/memory/<ns>/. Use mem_read/write/edit/append/list/grep/diff/mkdir/\
                 remove to organize your notes freely; the .anolisa/ subdirectory is reserved \
                 for OS metadata and not writable."
                    .into(),
            ),
        }
    }

    fn list_tools(
        &self,
        _request: PaginatedRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, rmcp::Error>> + Send + '_ {
        let profile = self.profile();
        let all = memory_tool_box().list();
        let filtered: Vec<_> = all
            .into_iter()
            .filter(|t| profile.tool_visible(t.name.as_ref()))
            .collect();
        std::future::ready(Ok(ListToolsResult {
            next_cursor: None,
            tools: filtered,
        }))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<CallToolResult, rmcp::Error>> + Send + '_ {
        // Profile gating is enforced at the call site, not just at list:
        // an `expert`-profile client that hard-codes `memory_search` (or
        // crafts the call manually) must be refused, otherwise the
        // tools/list filter is just a UX hint and the contract that
        // "expert hides Tier B" is not actually a boundary.
        let profile = self.profile();
        let tool_name: String = request.name.as_ref().to_string();
        let visible = profile.tool_visible(&tool_name);
        let tcc = ToolCallContext::new(self, request, context);
        async move {
            if !visible {
                return Err(ErrorData::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    format!("tool '{tool_name}' is not exposed under the {profile:?} profile"),
                    None,
                ));
            }
            memory_tool_box().call(tcc).await
        }
    }
}
