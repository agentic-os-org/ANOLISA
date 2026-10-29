# 更新日志

[English](CHANGELOG.md)

本项目的所有重要变更都会记录在此文件中。

本文档格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
项目遵循[语义化版本](https://semver.org/lang/zh-CN/spec/v2.0.0.html)。

## [0.2.12] - 2026-10-28

### 修复

- **agent-memory**：更新至 v0.2.12，通过 `memory_capture` 在每次 OpenClaw 自动观察时检查持久化的采集同意设置，包括其他客户端所做的修改；被拒绝或失败的采集仍可重试，成功采集会出现在 `memory_auto_created` 中。请同步升级服务端与适配器；旧服务端会拒绝新的 Hook 调用，不会回退到手动观察。手动写入仍可使用，已有观察记录不会重新标注（#8865）
- **agent-memory**：立即将新观察记录归属到 `MCP_CLIENT_NAME`，使观察它的 Agent 能在 isolated 范围召回，其他 Agent 的 filter 范围检索会排除这些记录。重启或运行 `mem_index_refresh` 不会补填已有未标注索引行的归属（#7776）
- **agent-memory**：在发布 `mem_edit` 和 `mem_write` 覆盖写入前准备完整替换文件，准备阶段失败时保留原内容。替换要求目录可读写且位于同一文件系统、Linux procfs 可用，并有容纳新内容的空间；自定义属主及主机安全标签策略需要验证，其他硬链接保留旧内容。若错误提示替换已发布但同步失败，新内容已可见，重试前应先读取确认（#8982）
- **agent-memory**：`mem_revert` 只恢复指定文件，保留 Git HEAD、暂存内容和其他文件的待提交修改，响应现在标明来源提交。若旧内容仍在暂存区，手动提交前应显式暂存恢复后的文件（#9082）
- **agent-memory**：在发布快照恢复结果前验证 gzip 校验和及完整性，损坏或截断的归档会报错，同时保留当前记忆文件（#7631）
- **agent-memory**：在启动索引前开始监听记忆变更，即使时间戳未变也会刷新大小发生变化的文件，并在旧文件变为目录时保留新索引的子文件，使这些启动场景下的关键词和向量结果保持最新。时间戳和大小均未变化的重写仍不在扫描指纹的检测保证范围内（#8880、#9127、#9089）
- **agent-memory**：将带点文件名以及 `AND`、`NOT`、`OR` 作为普通关键词检索文本，Agent 可召回 `config.toml` 等词语而不触发检索语法错误；空格分隔的词语保留先匹配全部词语、再回退到部分匹配的行为（#8012）
- **agent-memory**：遇到无法读取的检索索引行时报告错误并提供恢复指引，不再返回看似成功的空结果或部分结果，使关键词、向量和混合检索失败可见，同时避免从失败检索生成无意义的兴趣或经验（#8854）
- **agent-memory**：缩短 OpenClaw 召回查询时保留完整 Unicode 字符，避免拆断 emoji 或生成损坏的查询前缀（#4730）
- **agent-memory**：将会话日志尾部读取限制在 1 MiB 并保留 UTF-8 边界，超大的近期条目不再导致日志读取失败或错误显示为空；超大条目仍可能仅返回部分尾部内容（#7896）
- **agent-memory**：将导入 AMA frontmatter 键、值和列表项中的 ASCII 控制字符规范为空格，防止注入元数据行或提前结束 frontmatter；导入时有意不保留这些字符（#4626）
- **agent-memory**：在 AMA 导出再导入后保留 `memory_summary` 的概念和文件排名，并将单行列表项中的逗号、引号和反斜杠保留在原条目内，无需重写已有导入文件即可获得准确汇总（#8862）
- **agent-memory**：缩短过长标题或描述时保留完整的 `MEMORY.md` 链接目标，使 Agent 能沿长路径读取记忆，同时保留索引总大小限制（#3621）
- **agent-memory**：`mem_diff` 返回带有一对文件头的有效 unified patch，并正确转义文件名中的控制字符，使调用方能够解析和应用结果（#3575）
- **agent-memory**：仅在有效的文件开头 frontmatter 中写入取代标记，并在保留新事实的同时记录索引更新失败，避免修改普通正文，也便于诊断反复发生的整合冲突（#3517、#3532）
- **agent-memory**：整合时保留无法表示的 `N days ago` 和 `N 天前` 原文，防止日期溢出失败或误替换为当天日期（#9144）
- **agent-memory**：OpenClaw 安装和卸载脚本遵守 `ANOLISA_DRY_RUN=1`，预览适用操作而不执行安装、删除文件或重写配置；安装仍可能执行只读的 CLI 帮助探测（#7755）
- **agent-memory**：在随包发布的 MCP 目录中列出全部 38 个运行时工具，使依赖描述文件的客户端可以发现此前遗漏的会话、汇总、上下文、画像和索引刷新工具（#9148）

## [0.2.11] - 2026-10-25

### 修复

- **agent-memory**：更新至 v0.2.11，在关键词检索的结果数量限制之前按事实分类过滤，即使其他分类排名更高，也能找到所需记忆；同时覆盖短中文查询和回退到关键词检索的路径，分类名称保持大小写敏感。配置了 embedding provider 的向量/混合检索仍在截断结果后过滤分类（#8721）
- **agent-memory**：在向量排序和结果数量限制之前按配置排除冷记忆，并过滤已被替代的记忆，避免被排除的记忆挤占向量和混合检索中的有效结果（#3805）
- **agent-memory**：内容修改后使旧向量失效，并在文件超过索引大小限制或不再是 UTF-8 文本时移除旧搜索条目，避免关键词和向量检索返回过时内容（#3803、#3820）
- **agent-memory**：按字面路径仅移除指定路径及其下级索引，保留名称含类似通配符字符或大小写不同的无关记忆（#3801）
- **agent-memory**：目录属性更新后跳过未变化文件的重复索引和 embedding 调用，减少 embedding 用量，同时保留缺失向量的补全能力，以及在同一批文件监听事件中删除后恢复的文件（#8477）
- **agent-memory**：让并发的自动增量整理串行执行，并在异常展开后释放执行标记，避免重复事实且允许后续自动整理继续运行；在应用数量预算前对保留相同工具调用的情景记忆去重，同时保留独立任务和错误恢复情景（#8303、#8305）
- **agent-memory**：生成兴趣和用户画像时跳过脱敏后的搜索长度标记，避免新增 `len=N` 兴趣，同时保留旧日志中记录的真实查询；历史上已生成的无效事实仍会保留，可通过 `memory_forget` 删除（#7928）
- **agent-memory**：正确读取结束分隔线后没有换行的 frontmatter，导出/导入可保留 ID 和分类，摘要及用户画像也不会再将元数据当作正文（#8687）
- **agent-memory**：OpenClaw 接收跨管道分块的响应时保留中文、重音字符和 emoji，并在初始化握手失败后先释放 MCP 子进程再重试，避免工具结果乱码和遗留进程（#8454、#8543）
- **agent-memory**：在服务启动前拒绝无效的时间衰减配置，对非有限数值的环境变量覆盖发出警告并忽略，避免破坏新旧记忆的排序。升级前请将文件配置修正为有限的 `time_decay_lambda >= 0` 和 `[0, 1]` 范围内的 `time_decay_alpha`；无效的文件配置现在会阻止启动（#8701、#8699）
- **agent-memory**：用户或组映射未完成时，重试仍将命名空间初始化失败视为不可恢复错误，避免服务在隔离设置不完整的状态下启动（#8439）

## [0.2.10] - 2026-10-15

### 新增

- **agent-memory**：更新至 v0.2.10，提供包含记忆服务和 OpenClaw 适配器的 Linux x86_64 与 ARM64 原始发布包，用户可直接安装经过校验的归档，无需从源码构建（#7648、#7649）

### 修复

- **agent-memory**：按宿主提供的 Tool Search 或直接工具调用方式引导 Agent，使延迟加载的记忆工具能够被发现并以有效参数调用（#7645）
- **agent-memory**：等待 MCP 子进程退出，并在 OpenClaw 插件实例销毁时清理进程，避免关闭和重新加载时遗留记忆进程（#7482、#7661）

## [0.2.9] - 2026-10-11

### 修复

- **agent-memory**: 更新至 v0.2.9，定位 OpenClaw adapter 的二进制时要求路径指向可执行文件，用户将目录配置为 `binaryPath` 时会在启动阶段收到配置错误；即使 `PATH` 较早的条目中存在名为 `agent-memory` 的目录，也能继续找到后续有效的可执行文件或指向可执行文件的符号链接（#4855）

## [0.2.8] - 2026-09-20

### 新增

- **agent-memory**：配置的会话目录不可用时，回退到当前用户独享的运行时目录或临时目录，非特权安装无需创建 root 所有的目录即可使用会话日志和 `mem_promote`；回退路径会检查所有者、权限及符号链接重定向（#3266）

### 变更

- **agent-memory**：**不兼容变更：** OpenClaw 工具更名为 `anolisa_memory_search` 和 `anolisa_memory_get`，Agent 可以同时访问 ANOLISA 记忆和 OpenClaw 自带记忆工具，避免名称冲突；升级时需更新提示词及工具 allowlist、重启 gateway 并开启新会话。内部 MCP 名称和已存储记忆保持不变（#3347）
- **agent-memory**：在 OpenClaw 的 `coding` profile 中声明全部四个插件工具，Agent 在该 profile 下无需禁用 `memory-core` 即可访问插件工具；自定义策略仍需显式添加工具条目，中间版本安装器留下的禁用标记需按[用户指南](../../docs/user-guide/zh/agent-memory/index.md)手动恢复（#3347）

### 修复

- **agent-memory**：在 OpenClaw 插件 schema 中接纳 `sessionId` 和 `sessionDir`，并在插件启动时按服务端规则检查标识符的 128 字节 UTF-8 上限，用户可在子进程重启后复用会话临时文件，并在无效标识符改变命名空间之前看到配置错误（#3213）
- **agent-memory**：让更强的 BM25 匹配排在前面，并统一关键词分数越高越相关的约定，搜索、混合检索和自动召回会优先返回最相关的匹配记忆（#3296）
- **agent-memory**：拒绝 OpenClaw 插件不支持的 `expert` profile，提示改用 `basic` 或 `advanced`，并在验证重载配置之前停止旧客户端，用户可获得明确的配置错误，避免旧记忆进程继续持有存储锁（#3238）
- **agent-memory**：补齐 `make dist` 源码归档所需文件，源码包和 RPM 构建可获得编译及安装所需的示例目标、配置和适配器资源（#3231）

## [0.2.7] - 2026-09-09

### 修复

- **agent-memory**：根据宿主帮助信息选择 OpenClaw 安装参数并默认授予插件声明的能力，用户可在要求授权的宿主上安装，也可通过 `AGENT_MEMORY_ACCEPT_CAPABILITIES=0` 拒绝授权；不再传递已弃用且无效的 unsafe-install 参数，并区分授权拒绝与安装目录不可写的问题（#3149）
- **agent-memory**：将 corpus 命中标记为 `agent-memory` 并提供适用于 `memory_get corpus=all` 的读取路径，Agent 可读取与 OpenClaw 工作区记忆路径重叠的存储记忆，并在分段读取时获得准确的 `fromLine` / `lineCount`（#3177）
- **agent-memory**：在查找二进制前校验插件配置，即使尚未安装 `agent-memory`，用户也能看到无效 `userId` / `sessionId` 的具体错误（#3155）
- **agent-memory**：声明 RPM 组件标识，ANOLISA 可将已安装的软件包识别为 `agent-memory` 组件（#2560）
- **agent-memory**：在统一构建流程中声明 Node.js >= 20 和 npm 为源码构建依赖，用户模式自动准备构建 OpenClaw 适配器所需的工具，系统模式则在构建前提示缺失工具（#3187）

## [0.2.6] - 2026-07-30

### 修复

- **agent-memory**：更新到 v0.2.6，在短 token 的 LIKE 严格匹配无结果时改用 OR 匹配，并优先保留匹配更多关键词的结果，Agent 可从包含停用词的 prompt 中召回相关记忆而不再得到空结果（#2040）

## [0.2.5] - 2026-07-27

### 修复

- **agent-memory**：更新到 v0.2.5，从较长的英文和 CJK prompt 中生成聚焦的召回查询并合并结果，Agent 可从冗长 prompt 中召回相关记忆且不会静默遗漏主题（#1574）

## 0.2.4

- fix(memory)：修复 observe 后自动召回返回空结果的问题——`memory_observe` 后同步重建索引，使 `before_prompt_build` hook 能找到新内容（#1520）
- fix(memory)：`install.sh` 为 hook 设置 `allowConversationAccess`（#1521）

## 0.2.3

- fix(memory)：在 trigger 匹配和 hash 计算前，将 OpenClaw content block 数组 `[{type:"text", text:"..."}]` 规范化为字符串，避免自动捕获因内容被转换成 `"[object Object]"` 而失效
- fix(memory)：增加 BM25 OR fallback——当隐式 AND FTS5 查询返回 0 行且存在多个 token 时，使用 `'\"token1\" OR \"token2\" OR ...'` 重试，使部分匹配也能返回，而非静默失败
- fix(memory)：将 `format!("{:.120}", query)` 替换为 `format!("bm25:len={}", query.len())`，清理 `audit_log`，避免用户查询内容泄漏到日志路径

## 0.2.2

- 修复 `memory_observe` hint 清理，使 YAML 转义的 hint 能通过不解析 YAML escape 的手写 frontmatter reader 往返读取：用 `sanitize_hint()` 替换 `yaml_escape_hint()`，仅将换行符和 ASCII 控制字符替换为空格；增加 8 个单元测试和一个真实 parser 往返测试，覆盖含反斜杠的 Windows 路径
- 为 `MemoryConfig` 增加 `max_hint_bytes`（默认 512）和 `MEMORY_MAX_HINT_BYTES` 环境变量覆盖；将 `&MemoryConfig` 贯穿 `memory_observe`、`MemoryService` facade 和 MCP server
- 修复 `make install INSTALL_PROFILE=user PREFIX=$HOME/.local` 在 `install-adapter-resources` 阶段因 Permission denied 失败的问题：遵循 `INSTALL_PROFILE` 并从 `$(PREFIX)` 推导 `DATADIR`/`SHARE_DIR`，使所有可写路径均服从 profile（system 模式不变）；与 tokenless/ws-ckpt 安装约定保持一致
- 增加 `safe_fs` 安全边界单元测试（path escape、symlink traversal、sandbox root violation），并修复 `cargo fmt --all --check` 暴露的格式和 import order 问题

## 0.2.1

- 修复配置 embedding provider 时 vector/hybrid search panic 和索引为空的问题：index worker 在没有 tokio Handle 的 std::thread 上运行，导致无法生成 embedding，而 `memory_search mode=vector|hybrid` 从 worker thread 调用 Handle::block_on；现在 spawn 时捕获 runtime handle 并传入 worker，search path 使用 block_in_place
- 修复 `memory_get_context` 将 `.git` 内部文件（如 `.git/logs/HEAD`）泄漏到 Agent context 的问题：通过 `safe_fs` 中共享的 `is_under_git` predicate 扩展 reserved-path filter，覆盖 `.git/`
- 修复 `full_scan`（启动和 inotify overflow 恢复）只构建 BM25 index 而不生成 dense embedding，导致既有文件在修改前对 vector search 不可见的问题；新增 `paths_without_vec` 查询和 backfill pass，并将 embedding 逻辑集中到与 flush 共用的 `embed_sync` helper
- 修复 `memory_search` 对短 CJK query term（少于 3 个字符，如“花名”/“小云”）返回 0 条结果的问题：trigram tokenizer 不会为少于 3 个字符的 term 生成 token，因此此类查询改用 `body LIKE '%term%'` substring scan，同时保留 recall、agent-scope filtering 以及 cold/superseded exclusion
- 根据第一次真实 response 确定 embedding dimensions，不再硬编码 1536（DashScope text-embedding-v3 为 1024）：维度存储在以估算值初始化的 AtomicUsize 中，并在首次 embed 时覆盖
- 通过 `.anolisa/component.toml` 增加 anolisa-cli adapter contract，使 CLI adapter manager 能通过 `[[adapters]]` TOML schema 发现 OpenClaw plugin bundle

## 0.2.0

- 增加 prompt injection 安全模块（`looksLikePromptInjection` + `escapeMemoryForPrompt`），Rust core 和 TS adapter 保持一致
- 为安全模块增加 secret detection 和 PII redaction
- 增加 auto-recall `before_prompt_build` hook，每轮注入相关 memory
- 增加带 trigger filtering、SHA256 dedup 和 injection rejection 的 auto-capture `agent_end` hook
- 通过可插拔 `EmbeddingProvider` 增加 dense-vector semantic search（OpenAI `/v1/embeddings`、Ollama `/api/embed`）
- 增加 `files_vec` table（schema v2），与 FTS5 BM25 一起存储 per-file dense embedding
- 增加通过 reciprocal rank fusion（RRF，k=60）融合 BM25 和 vector score 的 hybrid search
- 为 `memory_search` 增加 `mode` 参数（bm25/vector/hybrid），并支持 graceful fallback 到 BM25
- 通过 `[memory].agent_scope`（shared/isolated/filter，schema v5）增加 per-agent memory isolation
- 增加带 `consent.toml` preference 的 memory sovereignty tool（memory_about/forget/auto_created/consent）
- 为 `memory_observe` 增加 4 类封闭 memory classification（user/feedback/project/reference）
- 增加 `mem_export` 和 `mem_import`，用于跨 Agent memory migration（AMA archive format）
- 增加用于 memory overview 和 source tracking 的 `memory_summary` tool
- 增加 `memory_session_context` tool
- 增加 `memory_sessions` 和 `memory_timeline` session history query tool
- 增加 `MEMORY.md` index file 和 `mem_index_refresh` tool
- 增加 user profile synthesis（Dreaming V3 `mem_dream`）
- 增加 memory consolidation：shutdown 时从 session audit log 自动提取 L1 atomic fact
- 从连贯的 tool-call chain 中增加 episodic memory extraction
- 增加 cross-session task persistence 和 incremental consolidation
- 增加 consolidation quality filter（mutual exclusion、non-derivable、date normalization）
- 为 BM25/vector/hybrid score 增加 time-decay ranking（exp(-λ×age_days)）
- 使用 `mem_compact` tool 对长期从未访问的文件进行 cold archival
- 写入新 fact 前通过 BM25 similarity 增加 conflict detection
- 增加 category subdirectory（`facts/<category>/`）以及 `memory_search` category filter
- 增加 token tracking（`AuditEntry` 中的 `tokens` 字段）
- 增加手动触发 consolidation 的 `mem_consolidate` tool
- 为 `memory_search corpus=all` 增加 corpus supplement registration
- 增加 `EmbeddingConfig`（None/OpenAI/Ollama），支持 TOML parsing 和 env override
- 为 `memory_search` signature 增加可选的 `mode` 和 `category` 参数
- 将 `memory_search` query 限制为 1024 个字符，防止 FTS5 resource exhaustion
- 将 embedding error response body 截断到 200 个字符，防止 API key 泄漏
- 在 `ConsolidatedFact` 中区分 CJK 与 ASCII token estimation
- 在 mutex 下持有 `FactWriter` JSONL file handle，防止 line interleaving
- 通过 canonicalize + starts_with traversal guard，从 db path 推导 `BM25Store` mount root
- 根据 entry timestamp 而非 chain length 计算 Episode duration
- 将 `session_id` 传播到提取的 episodic fact
- 从 `consolidate()` 返回 fact count，供 `mem_consolidate` reporting 使用
- 修复 search response 中的 `effectiveMode`，使其反映实际使用的 mode
- 修复 embedding API empty-response handling，返回维度正确的 zero vector

## 0.1.0

- 引入面向 AI Agent 的 filesystem memory MCP server（仅 Linux），通过 stdio JSON-RPC 2.0 提供三个 tier 的 21 个 tool（Tier A file op、Tier B BM25 search、Tier C governance）
- 在 `~/.anolisa/memory/<ns>/` 下增加 per-namespace mount，并支持可选的 user namespace + private tmpfs isolation（auto/userland/userns strategy）
- 对每次 Tier A file open 使用 `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` 强制执行 path sandbox
- 增加带 transactional upsert、schema migration、trigram CJK tokenizer 和 inotify-driven debounced flush 的 SQLite FTS5 BM25 background index
- 增加可选的 git versioning，auto-commit 在 per-handle mutex 下串行执行
- 增加 tar.gz snapshot，使用严格的 id whitelist、restore 时的 atomic rename swap，以及 `.anolisa/trash/` 下的 rollback entry
- 增加可选的 cgroup v2 `memory.max` self-limit，在 tokio runtime 启动前应用
- 增加 JSONL audit log（`O_NOFOLLOW | O_CLOEXEC`、`Mutex<File>`），并支持可选的 systemd-journald fan-out
- 在 `tools/list` 和 `tools/call` 强制执行 profile gating（basic/advanced/expert），config struct 使用 `deny_unknown_fields`
- 在 `/run/anolisa/sessions/<sid>/` 下增加 per-session scratch 和 log（0700），并附带 tmpfiles.d snippet
- 增加经过加固的 systemd user template `anolisa-memory@.service`（`ProtectKernelTunables/Modules/Logs`、`SystemCallFilter`、`MemoryDenyWriteExecute`、`RestrictNamespaces`、`RestrictAddressFamilies=AF_UNIX`）
- 增加使用 offline vendor tarball 和单个 statically-linked binary（bundled SQLite + vendored libgit2）的 RPM packaging
- 增加 OpenClaw plugin `memory-anolisa`，支持 install/detect/uninstall lifecycle，并提供 4 个通过 stdio child 路由到 MCP server 的 memory contract tool
- 增加从 `Cargo.toml` 到 manifest/package/openclaw/mcp JSON 和 bundle 的 single-source version sync
- 增加 `mcp-harness` example，以及覆盖 12 个 integration suite 的 140 个 automated test
