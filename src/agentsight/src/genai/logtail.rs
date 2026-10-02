//! iLogtail 文件导出器
//!
//! 将 GenAI 语义事件以扁平化 JSON 格式写入指定路径，
//! 由 iLogtail 自动采集上传到 SLS，无需配置 AK/SK。
//!
//! 通过环境变量 `SLS_LOGTAIL_FILE` 指定输出路径。
//! 仅当该环境变量设置时才启用。

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use super::encrypt::MessageEncryptor;
use super::exporter::GenAIExporter;
use super::instance_id;
use super::semantic::GenAISemanticEvent;
use crate::interruption::types::InterruptionEvent;
use crate::skill_metrics::extractor::extract_skill_load_counts_from_messages;

/// 环境变量名称
pub const LOGTAIL_ENV_VAR: &str = "SLS_LOGTAIL_FILE";

/// 默认 SLS Logtail 输出路径。
///
/// 当 `SLS_LOGTAIL_FILE` 环境变量未指向 `/var/sysom/` 目录时，
/// AgentSight 使用此路径作为本地默认 SLS 输出文件。
pub const DEFAULT_SLS_LOGTAIL_PATH: &str = "/var/log/anolisa/sls/ops/agentsight.jsonl";

/// 写入默认 SLS 路径（[`DEFAULT_SLS_LOGTAIL_PATH`]）时保留的字段白名单。
///
/// 默认路径面向概览级采集，仅保留模型、Token、Agent、Skill 等关键指标，
/// 删除对话内容、请求参数、时间戳明细等字段以降低数据量。
/// sysom 路径与环境变量路径不受影响，保持完整字段。
///
/// 注意：`__time__`/`__source__`/`__topic__` 等 iLogtail 保留字段也不保留，
/// 仅按本白名单输出。
const DEFAULT_PATH_KEEP_FIELDS: &[&str] = &[
    // LLMCall 记录保留字段
    "gen_ai.operation.name",
    "gen_ai.provider.name",
    "gen_ai.request.model",
    "gen_ai.usage.input_tokens",
    "gen_ai.usage.output_tokens",
    "gen_ai.usage.cache_creation.input_tokens",
    "gen_ai.usage.cache_read.input_tokens",
    "agentsight.agent.name",
    "agentsight.http.domain",
    "agent.skill.name",
    "agent.skill.load_count",
    // 中断事件记录保留字段
    "agentsight.interruption.type",
    "agentsight.interruption.severity",
];

/// 按 [`DEFAULT_PATH_KEEP_FIELDS`] 白名单原地精简记录。
///
/// 仅保留 [`DEFAULT_PATH_KEEP_FIELDS`] 白名单字段，删除其余字段。
/// 仅用于默认 SLS 路径的写入；其他路径应写入完整记录。
fn slim_records_for_default_path(records: &mut [BTreeMap<String, String>]) {
    for m in records.iter_mut() {
        m.retain(|k, _| DEFAULT_PATH_KEEP_FIELDS.contains(&k.as_str()));
    }
}

/// 遥测禁用哨兵文件路径（默认路径写前门控的第二级）。
///
/// 默认路径写入采用两级门控：第一级是目标文件必须存在
/// （`require_path_exists`，标识 iLogtail 已部署）；第二级即本哨兵文件 ——
/// 运维创建此文件即可显式关闭默认路径遥测。sysom / 环境变量路径不受影响。
const TELEMETRY_DISABLED_SENTINEL: &str = "/etc/anolisa/.telemetry_disabled";

/// 检查默认路径遥测是否被哨兵文件禁用。
///
/// 每次调用都执行一次 `stat`（经由 [`std::path::Path::exists`]），不做缓存，
/// 以便运维通过创建 / 删除哨兵文件即时开关默认路径遥测。
fn default_path_telemetry_disabled() -> bool {
    telemetry_disabled_at(std::path::Path::new(TELEMETRY_DISABLED_SENTINEL))
}

/// 判定给定哨兵路径是否存在（存在即视为遥测被禁用）。
///
/// 抽出路径参数以便单元测试用临时文件覆盖存在 / 不存在两种分支，
/// 生产逻辑通过 [`default_path_telemetry_disabled`] 传入固定的
/// [`TELEMETRY_DISABLED_SENTINEL`]。
fn telemetry_disabled_at(sentinel: &std::path::Path) -> bool {
    sentinel.exists()
}

/// 动态 Logtail 路径（由 config watcher 运行时设置）
static DYNAMIC_LOGTAIL_PATH: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// 设置动态 Logtail 输出路径（线程安全）。
///
/// 由 config watcher 在检测到 `runtime.sls_logtail_path` 变更时调用：
/// * 非空字符串 → 设置/更新动态路径，启用 SLS 上传；
/// * 空字符串    → 清空动态路径，已激活的 `LogtailExporter`（`dynamic=true`）
///   下次 `export()` 时检测到 `logtail_path() == None` 直接跳过，实现可逆暂停。
pub fn set_dynamic_logtail_path(path: &str) {
    if let Ok(mut guard) = DYNAMIC_LOGTAIL_PATH.write() {
        if path.is_empty() {
            if guard.is_some() {
                log::info!("Dynamic logtail path cleared (SLS uploads paused)");
            }
            *guard = None;
        } else {
            *guard = Some(path.to_string());
            log::info!("Dynamic logtail path set: {path}");
        }
    }
}

/// 检查 Logtail 导出是否启用（环境变量 SLS_LOGTAIL_FILE 是否设置，或动态路径已配置）
pub fn logtail_enabled() -> bool {
    std::env::var(LOGTAIL_ENV_VAR).is_ok() || {
        DYNAMIC_LOGTAIL_PATH
            .read()
            .map(|g| g.is_some())
            .unwrap_or(false)
    }
}

/// 获取 Logtail 输出路径
///
/// 优先级：环境变量 `SLS_LOGTAIL_FILE` > 动态配置路径
pub fn logtail_path() -> Option<String> {
    // 环境变量优先
    if let Ok(p) = std::env::var(LOGTAIL_ENV_VAR) {
        return Some(p);
    }
    // 回退到动态配置路径
    DYNAMIC_LOGTAIL_PATH.read().ok().and_then(|g| g.clone())
}

/// 返回当前应写入 SLS Logtail 的所有活动路径。
///
/// 规则与 `unified.rs` 中的 exporter 注册逻辑保持一致：
/// - 若 `SLS_LOGTAIL_FILE` 指向 `/var/sysom/` 目录，仅返回该 sysom 路径；
/// - 否则返回 `SLS_LOGTAIL_FILE`（如有）与 [`DEFAULT_SLS_LOGTAIL_PATH`] 的组合。
///
/// 返回的列表已去重，避免同一文件被重复写入。
pub fn active_logtail_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    if let Some(p) = logtail_path() {
        if p.starts_with("/var/sysom/") {
            return vec![std::path::PathBuf::from(p)];
        }
        paths.push(std::path::PathBuf::from(p));
    }
    let default = std::path::PathBuf::from(DEFAULT_SLS_LOGTAIL_PATH);
    if !paths.contains(&default) {
        paths.push(default);
    }
    paths
}

/// iLogtail 文件导出器
///
/// 将 GenAI 事件以扁平化 JSON 格式（每行一条记录）写入指定路径，
/// 由 iLogtail 自动采集上传到 SLS。字段命名与 SLS PutLogs 完全一致。
/// 敏感消息字段使用 RSA+AES 混合加密保护。
pub struct LogtailExporter {
    path: PathBuf,
    encryptor: Option<MessageEncryptor>,
    /// 轨迹采集开关（对应 agentsight.json 的 `traceEnabled`）。
    /// 为 `false` 时，LLMCall 上传记录中的
    /// `gen_ai.system_instructions`、`gen_ai.input.messages`、
    /// `gen_ai.output.messages` 等对话内容字段被丢弃；
    /// token 数量、模型、提供商等元数据仍照常上传。
    trace_enabled: bool,
    /// 是否使用动态路径（来自 `runtime.sls_logtail_path` 配置）。
    /// 为 `true` 时每次 `export()` 调用 `logtail_path()` 取最新路径，
    /// 路径为空（被清空）则丢弃本批次，实现可逆的"暂停 / 恢复"语义；
    /// 为 `false` 时（环境变量启动模式）始终写入构造时锁定的 `path`。
    dynamic: bool,
    /// 写入前是否要求目标路径已存在。
    /// 用于默认 SLS 路径：若 `/var/log/anolisa/sls/ops/agentsight.jsonl`
    /// 不存在（iLogtail 未部署或目录未挂载），则跳过写入，避免自动创建文件。
    require_path_exists: bool,
}

impl LogtailExporter {
    /// 创建新的 Logtail 导出器
    ///
    /// 从环境变量 `SLS_LOGTAIL_FILE` 读取路径，自动创建父目录。
    /// 如果环境变量未设置，返回 `None`。
    ///
    /// `encryption_pem`：可选 RSA 公钥 PEM（通常来自 agentsight.json
    /// 的 `encryption.public_key`）。有值且解析成功则启用加密；
    /// 为 None 或解析失败则不加密。
    ///
    /// `trace_enabled`：轨迹采集开关。为 `false` 时不上传对话内容字段，
    /// 但保留 token 数量等元数据。
    pub fn new(encryption_pem: Option<&str>, trace_enabled: bool) -> Option<Self> {
        let path_str = logtail_path()?;
        let path = PathBuf::from(path_str);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let encryptor = encryption_pem.and_then(MessageEncryptor::from_pem);
        if encryptor.is_none() {
            log::info!("Logtail exporter: encryption disabled (no public key configured)");
        }
        if !trace_enabled {
            log::info!(
                "Logtail exporter: traceEnabled=false, conversation content fields (gen_ai.system_instructions, gen_ai.input.messages, gen_ai.output.messages) will NOT be uploaded"
            );
        }
        Some(LogtailExporter {
            path,
            encryptor,
            trace_enabled,
            dynamic: false,
            require_path_exists: false,
        })
    }

    /// 创建默认路径的 Logtail 导出器。
    ///
    /// 固定写入 [`DEFAULT_SLS_LOGTAIL_PATH`]，不受 `SLS_LOGTAIL_FILE`
    /// 环境变量或动态配置影响。用于非 sysom 模式下的本地默认 SLS 输出。
    pub fn new_default(encryption_pem: Option<&str>, trace_enabled: bool) -> Self {
        let path = PathBuf::from(DEFAULT_SLS_LOGTAIL_PATH);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let encryptor = encryption_pem.and_then(MessageEncryptor::from_pem);
        if encryptor.is_none() {
            log::info!(
                "Logtail exporter (default): encryption disabled (no public key configured)"
            );
        }
        if !trace_enabled {
            log::info!(
                "Logtail exporter (default): traceEnabled=false, conversation content fields (gen_ai.system_instructions, gen_ai.input.messages, gen_ai.output.messages) will NOT be uploaded"
            );
        }
        LogtailExporter {
            path,
            encryptor,
            trace_enabled,
            dynamic: false,
            require_path_exists: true,
        }
    }

    /// 从显式路径创建 Logtail 导出器（用于运行时动态激活）
    ///
    /// 与 `new()` 不同，不依赖环境变量，直接使用传入的路径。
    /// 创建的导出器为动态模式，每次 `export()` 都会重新读取最新路径。
    pub fn new_with_path(
        path_str: &str,
        encryption_pem: Option<&str>,
        trace_enabled: bool,
    ) -> Self {
        Self::new_with_path_dynamic(path_str, encryption_pem, trace_enabled, true)
    }

    /// 从显式路径创建固定路径的 Logtail 导出器。
    ///
    /// 与 `new_with_path` 不同，创建的导出器为静态模式，始终写入构造时
    /// 锁定的路径，不受环境变量或动态配置变化影响。
    pub fn new_with_fixed_path(
        path_str: &str,
        encryption_pem: Option<&str>,
        trace_enabled: bool,
    ) -> Self {
        Self::new_with_path_dynamic(path_str, encryption_pem, trace_enabled, false)
    }

    fn new_with_path_dynamic(
        path_str: &str,
        encryption_pem: Option<&str>,
        trace_enabled: bool,
        dynamic: bool,
    ) -> Self {
        let path = PathBuf::from(path_str);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let encryptor = encryption_pem.and_then(MessageEncryptor::from_pem);
        let label = if dynamic { "dynamic" } else { "fixed" };
        if encryptor.is_none() {
            log::info!(
                "Logtail exporter ({label}): encryption disabled (no public key configured)"
            );
        }
        if !trace_enabled {
            log::info!(
                "Logtail exporter ({label}): traceEnabled=false, conversation content fields (gen_ai.system_instructions, gen_ai.input.messages, gen_ai.output.messages) will NOT be uploaded"
            );
        }
        LogtailExporter {
            path,
            encryptor,
            trace_enabled,
            dynamic,
            require_path_exists: false,
        }
    }

    /// 返回导出文件路径
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// 将扁平化记录批量写入文件（append 模式）
    ///
    /// `dynamic=true` 时每次重新调用 `logtail_path()` 取最新路径；
    /// 若动态路径已被 `set_dynamic_logtail_path("")` 清空（暂停语义），
    /// 直接丢弃本批次，不报错。
    fn write_batch(&self, events: &[GenAISemanticEvent]) {
        let target_path: PathBuf = if self.dynamic {
            match logtail_path() {
                Some(p) if !p.is_empty() => {
                    let p = PathBuf::from(p);
                    if let Some(parent) = p.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    p
                }
                _ => return, // 动态路径已清空 → 暂停状态，丢弃本批次
            }
        } else {
            self.path.clone()
        };

        // 默认路径写前门控（总开关，优先短路）：每次写前 stat 遥测禁用哨兵文件，
        // 存在则视为运维已关闭默认路径遥测，直接跳过，不再检查目标文件。
        // sysom / 环境变量路径不受此门控影响。
        if target_path == std::path::Path::new(DEFAULT_SLS_LOGTAIL_PATH)
            && default_path_telemetry_disabled()
        {
            return;
        }

        // 部署检查：目标文件不存在（iLogtail 未部署）则跳过，不自动创建。
        // 首次跳过打一条 warn，避免"启用了却无数据"时无迹可查（后续静默）。
        if self.require_path_exists && !target_path.exists() {
            static WARN_ONCE: std::sync::Once = std::sync::Once::new();
            WARN_ONCE.call_once(|| {
                log::warn!(
                    "SLS default-path export skipped: target file {target_path:?} does not exist (iLogtail not deployed?); create it to enable uploads"
                );
            });
            return;
        }

        let mut records =
            events_to_flat_records(events, self.encryptor.as_ref(), self.trace_enabled);
        if records.is_empty() {
            return;
        }
        // 默认 SLS 路径仅保留概览级字段；sysom / 环境变量路径写入完整记录。
        if target_path == std::path::Path::new(DEFAULT_SLS_LOGTAIL_PATH) {
            slim_records_for_default_path(&mut records);
        }

        let file = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target_path)
        {
            Ok(f) => f,
            Err(e) => {
                log::warn!("Failed to open logtail file {target_path:?}: {e}");
                return;
            }
        };

        let mut writer = BufWriter::new(file);
        for record in &records {
            match serde_json::to_string(record) {
                Ok(json_line) => {
                    if let Err(e) = writeln!(writer, "{json_line}") {
                        log::warn!("Failed to write logtail record: {e}");
                        return;
                    }
                }
                Err(e) => {
                    log::warn!("Failed to serialize logtail record: {e}");
                }
            }
        }

        if let Err(e) = writer.flush() {
            log::warn!("Failed to flush logtail file: {e}");
        }
    }
}

impl GenAIExporter for LogtailExporter {
    fn name(&self) -> &str {
        // Distinguish dynamic exporters so the runtime can replace an existing
        // dynamic exporter instead of accumulating duplicates when the SLS path
        // is deactivated and re-activated repeatedly.
        if self.dynamic {
            "logtail-file-dynamic"
        } else {
            "logtail-file"
        }
    }

    fn export(&self, events: &[GenAISemanticEvent]) {
        self.write_batch(events);
    }
}

/// 将 GenAI 语义事件转换为扁平化 key-value 记录
///
/// 返回 `Vec<BTreeMap<String, String>>`，每个 BTreeMap 代表一条日志记录。
/// 字段命名遵循 OTel GenAI 标准和 AgentSight 扩展规范。
/// 包含 iLogtail 保留字段：`__time__`、`__source__`、`__topic__`。
///
/// 此函数被 Logtail 文件导出器使用，由 iLogtail 采集后上传到 SLS。
/// 敏感消息字段（system_instructions/input.messages/output.messages）使用混合加密保护。
///
/// `trace_enabled=false` 时跳过 LLMCall 中的对话内容字段
/// (`gen_ai.system_instructions`、`gen_ai.input.messages`、
/// `gen_ai.output.messages`)，token 数量等元数据仍上传。
pub fn events_to_flat_records(
    events: &[GenAISemanticEvent],
    encryptor: Option<&MessageEncryptor>,
    trace_enabled: bool,
) -> Vec<BTreeMap<String, String>> {
    let hostname = instance_id::get_instance_id();
    let uid = instance_id::get_owner_account_id();
    let mut records = Vec::with_capacity(events.len());

    for event in events {
        let mut m = BTreeMap::new();
        let timestamp = chrono::Utc::now().timestamp();

        // iLogtail 保留字段
        m.insert("__time__".to_string(), timestamp.to_string());
        m.insert("__source__".to_string(), hostname.to_string());
        m.insert("__topic__".to_string(), "agentsight".to_string());

        // 每条日志都写入 instance
        m.insert("instance".to_string(), hostname.to_string());

        // 写入 uid (owner-account-id)
        if !uid.is_empty() {
            m.insert("uid".to_string(), uid.to_string());
        }

        match event {
            GenAISemanticEvent::LLMCall(call) => {
                // ── OTel GenAI Required ──
                m.insert("gen_ai.provider.name".to_string(), call.provider.clone());
                m.insert("gen_ai.request.model".to_string(), call.model.clone());
                m.insert(
                    "gen_ai.operation.name".to_string(),
                    call.metadata
                        .get("operation_name")
                        .cloned()
                        .unwrap_or_else(|| "chat".to_string()),
                );

                // ── OTel GenAI Conditionally Required ──
                if let Some(ref error) = call.error {
                    m.insert("error.type".to_string(), error.clone());
                }
                if let Some(port) = call.metadata.get("server.port") {
                    m.insert("server.port".to_string(), port.clone());
                }

                // ── OTel GenAI Recommended ──
                if let Some(rid) = call.metadata.get("response_id") {
                    m.insert("gen_ai.response.id".to_string(), rid.clone());
                } else {
                    m.insert("gen_ai.response.id".to_string(), call.call_id.clone());
                }
                m.insert("gen_ai.response.model".to_string(), call.model.clone());
                if let Some(reason) = call
                    .response
                    .messages
                    .first()
                    .and_then(|msg| msg.finish_reason.as_ref())
                {
                    m.insert(
                        "gen_ai.response.finish_reasons".to_string(),
                        format!("[\"{reason}\"]"),
                    );
                }
                if let Some(temp) = call.request.temperature {
                    m.insert("gen_ai.request.temperature".to_string(), temp.to_string());
                }
                if let Some(max) = call.request.max_tokens {
                    m.insert("gen_ai.request.max_tokens".to_string(), max.to_string());
                }
                if let Some(fp) = call.request.frequency_penalty {
                    m.insert(
                        "gen_ai.request.frequency_penalty".to_string(),
                        fp.to_string(),
                    );
                }
                if let Some(pp) = call.request.presence_penalty {
                    m.insert(
                        "gen_ai.request.presence_penalty".to_string(),
                        pp.to_string(),
                    );
                }
                if let Some(tp) = call.request.top_p {
                    m.insert("gen_ai.request.top_p".to_string(), tp.to_string());
                }
                if let Some(tk) = call.request.top_k {
                    m.insert("gen_ai.request.top_k".to_string(), tk.to_string());
                }
                if let Some(seed) = call.request.seed {
                    m.insert("gen_ai.request.seed".to_string(), seed.to_string());
                }
                if let Some(ref usage) = call.token_usage {
                    m.insert(
                        "gen_ai.usage.input_tokens".to_string(),
                        usage.input_tokens.to_string(),
                    );
                    m.insert(
                        "gen_ai.usage.output_tokens".to_string(),
                        usage.output_tokens.to_string(),
                    );
                    if let Some(cache_create) = usage.cache_creation_input_tokens {
                        m.insert(
                            "gen_ai.usage.cache_creation.input_tokens".to_string(),
                            cache_create.to_string(),
                        );
                    }
                    if let Some(cache_read) = usage.cache_read_input_tokens {
                        m.insert(
                            "gen_ai.usage.cache_read.input_tokens".to_string(),
                            cache_read.to_string(),
                        );
                    }
                }
                if let Some(addr) = call.metadata.get("server.address") {
                    m.insert("server.address".to_string(), addr.clone());
                }
                m.insert("gen_ai.output.type".to_string(), "text".to_string());

                // ── gen_ai.system_instructions (system role messages) ──
                // 受 trace_enabled 控制：system prompt 通常包含产品业务逻辑、
                // 工具说明等敏感配置，traceEnabled=false 时同样不上传。
                if trace_enabled {
                    let system_msgs: Vec<&super::semantic::InputMessage> = call
                        .request
                        .messages
                        .iter()
                        .filter(|msg| msg.role == "system")
                        .collect();
                    if !system_msgs.is_empty() {
                        if let Ok(json) = serde_json::to_string(&system_msgs) {
                            m.insert(
                                "gen_ai.system_instructions".to_string(),
                                MessageEncryptor::maybe_encrypt(encryptor, &json),
                            );
                        }
                    }
                }

                // ── gen_ai.input.messages (增量：只取最新一轮) ──
                // 仅在 trace_enabled=true 时上传对话内容。轨迹开关关闭时
                // 仅保留 token 数量等元数据，不上传用户输入。
                // 从后往前找最后一条 user message，取它及之后的所有非 system 消息
                if trace_enabled {
                    let latest_msgs =
                        super::semantic::latest_round_input_messages(&call.request.messages);
                    if !latest_msgs.is_empty() {
                        if let Ok(json) = serde_json::to_string(&latest_msgs) {
                            m.insert(
                                "gen_ai.input.messages".to_string(),
                                MessageEncryptor::maybe_encrypt(encryptor, &json),
                            );
                        }
                    }
                }

                // ── gen_ai.output.messages (parts-based with finish_reason) ──
                // 同样受 trace_enabled 控制，不上传模型响应内容。
                if trace_enabled && !call.response.messages.is_empty() {
                    if let Ok(json) = serde_json::to_string(&call.response.messages) {
                        m.insert(
                            "gen_ai.output.messages".to_string(),
                            MessageEncryptor::maybe_encrypt(encryptor, &json),
                        );
                    }
                }

                // ── 加密标记字段 ──
                if encryptor.is_some() {
                    m.insert("agentsight.encrypted".to_string(), "true".to_string());
                }

                // ── AgentSight extensions ──
                m.insert("agentsight.pid".to_string(), call.pid.to_string());
                m.insert(
                    "agentsight.process_name".to_string(),
                    call.process_name.clone(),
                );
                if let Some(ref name) = call.agent_name {
                    m.insert("agentsight.agent.name".to_string(), name.clone());
                }
                m.insert(
                    "agentsight.duration_ns".to_string(),
                    call.duration_ns.to_string(),
                );
                m.insert(
                    "agentsight.start_timestamp_ns".to_string(),
                    call.start_timestamp_ns.to_string(),
                );
                m.insert(
                    "agentsight.end_timestamp_ns".to_string(),
                    call.end_timestamp_ns.to_string(),
                );
                if let Some(method) = call.metadata.get("method") {
                    m.insert("agentsight.http.method".to_string(), method.clone());
                }
                if let Some(path) = call.metadata.get("path") {
                    m.insert("agentsight.http.path".to_string(), path.clone());
                }
                if let Some(domain) = call.metadata.get("http.domain") {
                    m.insert("agentsight.http.domain".to_string(), domain.clone());
                }
                if let Some(status) = call.metadata.get("status_code") {
                    m.insert("agentsight.http.status_code".to_string(), status.clone());
                }
                if call.request.stream
                    || call
                        .metadata
                        .get("is_sse")
                        .map(|v| v == "true")
                        .unwrap_or(false)
                {
                    m.insert("agentsight.stream".to_string(), "true".to_string());
                    if let Some(cnt) = call.metadata.get("sse_event_count") {
                        m.insert("agentsight.sse_event_count".to_string(), cnt.clone());
                    }
                }
                if let Some(rid) = call.metadata.get("response_id") {
                    m.insert("trace_id".to_string(), rid.clone());
                } else {
                    m.insert("trace_id".to_string(), call.call_id.clone());
                }
                if let Some(cid) = call.metadata.get("conversation_id") {
                    m.insert("gen_ai.conversation.id".to_string(), cid.clone());
                }
                if let Some(sid) = call.metadata.get("session_id") {
                    m.insert("gen_ai.session.id".to_string(), sid.clone());
                }

                // ── AgentSkill extensions: skill names and counts as JSON arrays ──
                // Default to null when no skills are detected; write arrays when present.
                let skill_counts = extract_skill_load_counts_from_messages(&call.response.messages);
                if skill_counts.is_empty() {
                    m.insert("agent.skill.name".to_string(), "null".to_string());
                    m.insert("agent.skill.load_count".to_string(), "null".to_string());
                } else {
                    let mut skill_pairs: Vec<(String, u64)> = skill_counts.into_iter().collect();
                    skill_pairs.sort_by(|a, b| a.0.cmp(&b.0));
                    let names: Vec<String> = skill_pairs.iter().map(|(k, _)| k.clone()).collect();
                    let counts: Vec<u64> = skill_pairs.iter().map(|(_, v)| *v).collect();
                    m.insert(
                        "agent.skill.name".to_string(),
                        serde_json::to_string(&names).unwrap_or_default(),
                    );
                    m.insert(
                        "agent.skill.load_count".to_string(),
                        serde_json::to_string(&counts).unwrap_or_default(),
                    );
                }
            }
            GenAISemanticEvent::ToolUse(tool) => {
                m.insert("gen_ai.operation.name".to_string(), "tool_use".to_string());
                m.insert("gen_ai.tool.name".to_string(), tool.tool_name.clone());
                if let Some(ref parent_id) = tool.parent_llm_call_id {
                    m.insert("gen_ai.response.id".to_string(), parent_id.clone());
                }
                m.insert(
                    "agentsight.tool.success".to_string(),
                    tool.success.to_string(),
                );
                m.insert("agentsight.pid".to_string(), tool.pid.to_string());
                if let Some(ref dur) = tool.duration_ns {
                    m.insert("agentsight.duration_ns".to_string(), dur.to_string());
                }
                if let Some(ref error) = tool.error {
                    m.insert("error.type".to_string(), error.clone());
                }
            }
            GenAISemanticEvent::AgentInteraction(interaction) => {
                m.insert(
                    "gen_ai.operation.name".to_string(),
                    "agent_interaction".to_string(),
                );
                m.insert(
                    "agentsight.agent.name".to_string(),
                    interaction.agent_name.clone(),
                );
                m.insert(
                    "agentsight.agent.interaction_type".to_string(),
                    interaction.interaction_type.clone(),
                );
                m.insert("agentsight.pid".to_string(), interaction.pid.to_string());
            }
            GenAISemanticEvent::StreamChunk(chunk) => {
                m.insert(
                    "gen_ai.operation.name".to_string(),
                    "stream_chunk".to_string(),
                );
                m.insert("agentsight.stream.id".to_string(), chunk.stream_id.clone());
                m.insert(
                    "agentsight.stream.chunk_index".to_string(),
                    chunk.chunk_index.to_string(),
                );
                m.insert("agentsight.pid".to_string(), chunk.pid.to_string());
            }
        }

        records.push(m);
    }

    records
}

/// 将中断事件转换为扁平化 key-value 记录
///
/// 通过 `gen_ai.operation.name = "interruption"` 与 LLMCall 记录区分。
/// 复用通用字段（instance/uid/pid/agent.name/session.id/conversation.id/trace_id/
/// start_timestamp_ns），并增加 `agentsight.interruption.*` 专属字段，便于在 SLS
/// 中以同一索引同时查询会话与中断事件。
pub fn interruption_events_to_flat_records(
    events: &[InterruptionEvent],
) -> Vec<BTreeMap<String, String>> {
    let hostname = instance_id::get_instance_id();
    let uid = instance_id::get_owner_account_id();
    let mut records = Vec::with_capacity(events.len());

    for event in events {
        let mut m = BTreeMap::new();
        let timestamp = chrono::Utc::now().timestamp();

        // iLogtail 保留字段
        m.insert("__time__".to_string(), timestamp.to_string());
        m.insert("__source__".to_string(), hostname.to_string());
        m.insert("__topic__".to_string(), "agentsight".to_string());

        m.insert("instance".to_string(), hostname.to_string());
        if !uid.is_empty() {
            m.insert("uid".to_string(), uid.to_string());
        }

        // 区分字段：与 LLMCall/ToolUse 等记录区分
        m.insert(
            "gen_ai.operation.name".to_string(),
            "interruption".to_string(),
        );

        // ── 复用 LLMCall 记录的关联字段 ──
        if let Some(pid) = event.pid {
            m.insert("agentsight.pid".to_string(), pid.to_string());
        }
        if let Some(ref name) = event.agent_name {
            m.insert("agentsight.agent.name".to_string(), name.clone());
        }
        if let Some(ref sid) = event.session_id {
            m.insert("gen_ai.session.id".to_string(), sid.clone());
        }
        if let Some(ref cid) = event.conversation_id {
            m.insert("gen_ai.conversation.id".to_string(), cid.clone());
        }
        if let Some(ref tid) = event.trace_id {
            m.insert("trace_id".to_string(), tid.clone());
        }
        m.insert(
            "agentsight.start_timestamp_ns".to_string(),
            event.occurred_at_ns.to_string(),
        );

        // ── 中断事件专属字段 ──
        m.insert(
            "agentsight.interruption.id".to_string(),
            event.interruption_id.clone(),
        );
        m.insert(
            "agentsight.interruption.type".to_string(),
            event.interruption_type.as_str().to_string(),
        );
        m.insert(
            "agentsight.interruption.severity".to_string(),
            event.severity.as_str().to_string(),
        );
        m.insert(
            "agentsight.interruption.resolved".to_string(),
            event.resolved.to_string(),
        );
        if let Some(ref detail) = event.detail {
            m.insert("agentsight.interruption.detail".to_string(), detail.clone());
        }
        if let Some(ref cid) = event.call_id {
            m.insert("agentsight.interruption.call_id".to_string(), cid.clone());
        }

        records.push(m);
    }

    records
}

/// 将中断事件批量导出到 iLogtail 文件（追加写入）
///
/// 写入路径与 `LogtailExporter::write_batch` 保持一致：
/// - 若 `SLS_LOGTAIL_FILE` 指向 `/var/sysom/` 目录，仅写入该 sysom 路径；
/// - 否则写入 `SLS_LOGTAIL_FILE`（如有）与 [`DEFAULT_SLS_LOGTAIL_PATH`] 的组合。
///
/// 对于默认 SLS 路径，若目标文件不存在则跳过写入，不自动创建目录或文件；
/// 环境变量 / sysom 路径保持原有 create-if-missing 行为。
/// 由 iLogtail 统一采集到 SLS。
pub fn export_interruption_events(events: &[InterruptionEvent]) {
    if events.is_empty() {
        return;
    }
    let paths = active_logtail_paths();
    if paths.is_empty() {
        return;
    }

    let records = interruption_events_to_flat_records(events);
    if records.is_empty() {
        return;
    }

    // 默认路径仅保留概览级字段（interruption.type / severity）；
    // sysom / 环境变量路径写入完整记录。
    let mut slim_records = records.clone();
    slim_records_for_default_path(&mut slim_records);

    let default_path = std::path::PathBuf::from(DEFAULT_SLS_LOGTAIL_PATH);
    for path in &paths {
        // 默认路径写前门控（总开关，优先短路）：哨兵文件存在则跳过默认路径写入。
        if path == &default_path && default_path_telemetry_disabled() {
            continue;
        }
        // Skip the default path when it does not exist, matching the behavior
        // of the default LogtailExporter for LLMCall records.
        if path == &default_path && !path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let records_to_write = if path == &default_path {
            &slim_records
        } else {
            &records
        };
        write_interruption_records_to_path(path, records_to_write);
    }
}

fn write_interruption_records_to_path(
    path: &std::path::Path,
    records: &[BTreeMap<String, String>],
) {
    let file = match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("Failed to open logtail file {path:?} for interruption export: {e}");
            return;
        }
    };

    let mut writer = BufWriter::new(file);
    for record in records {
        match serde_json::to_string(record) {
            Ok(json_line) => {
                if let Err(e) = writeln!(writer, "{json_line}") {
                    log::warn!("Failed to write interruption logtail record: {e}");
                    return;
                }
            }
            Err(e) => {
                log::warn!("Failed to serialize interruption logtail record: {e}");
            }
        }
    }

    if let Err(e) = writer.flush() {
        log::warn!("Failed to flush logtail file (interruption): {e}");
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::genai::semantic::{
        InputMessage, LLMCall, LLMRequest, LLMResponse, MessagePart, OutputMessage, TokenUsage,
    };
    use std::collections::HashMap;

    /// 构造一个包含 user/assistant 对话与 token usage 的 LLMCall。
    fn make_full_llm_call() -> LLMCall {
        let request = LLMRequest {
            messages: vec![
                InputMessage {
                    role: "system".to_string(),
                    parts: vec![MessagePart::Text {
                        content: "you are helpful".to_string(),
                    }],
                    name: None,
                },
                InputMessage {
                    role: "user".to_string(),
                    parts: vec![MessagePart::Text {
                        content: "hello secret".to_string(),
                    }],
                    name: None,
                },
            ],
            temperature: Some(0.7),
            max_tokens: Some(1024),
            frequency_penalty: None,
            presence_penalty: None,
            top_p: None,
            top_k: None,
            seed: None,
            stop_sequences: None,
            stream: false,
            tools: None,
            raw_body: None,
        };
        let mut call = LLMCall::new(
            "call-trace-test".to_string(),
            1_000,
            "openai".to_string(),
            "gpt-4".to_string(),
            request,
            42,
            "test-proc".to_string(),
        );
        call.set_response(
            LLMResponse {
                messages: vec![OutputMessage {
                    role: "assistant".to_string(),
                    parts: vec![MessagePart::Text {
                        content: "sensitive reply".to_string(),
                    }],
                    name: None,
                    finish_reason: Some("stop".to_string()),
                }],
                streamed: false,
                raw_body: None,
            },
            5_000,
        );
        call.set_token_usage(TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        call.metadata = HashMap::new();
        call
    }

    #[test]
    fn test_trace_enabled_true_includes_messages() {
        // 默认轨迹开启：system_instructions、input.messages、output.messages 均上传
        let event = GenAISemanticEvent::LLMCall(make_full_llm_call());
        let records = events_to_flat_records(&[event], None, true);
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert!(
            r.contains_key("gen_ai.system_instructions"),
            "system_instructions should be uploaded when traceEnabled=true"
        );
        assert!(
            r.contains_key("gen_ai.input.messages"),
            "input.messages should be uploaded when traceEnabled=true"
        );
        assert!(
            r.contains_key("gen_ai.output.messages"),
            "output.messages should be uploaded when traceEnabled=true"
        );
        // token 数量元数据也应存在
        assert_eq!(
            r.get("gen_ai.usage.input_tokens").map(String::as_str),
            Some("100")
        );
        assert_eq!(
            r.get("gen_ai.usage.output_tokens").map(String::as_str),
            Some("50")
        );
    }

    #[test]
    fn test_trace_enabled_false_drops_messages_keeps_token_metadata() {
        // 轨迹关闭：system_instructions、input.messages、output.messages 均不上传，
        // token 数量等元数据仍保留
        let event = GenAISemanticEvent::LLMCall(make_full_llm_call());
        let records = events_to_flat_records(&[event], None, false);
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert!(
            !r.contains_key("gen_ai.system_instructions"),
            "system_instructions must NOT be uploaded when traceEnabled=false"
        );
        assert!(
            !r.contains_key("gen_ai.input.messages"),
            "input.messages must NOT be uploaded when traceEnabled=false"
        );
        assert!(
            !r.contains_key("gen_ai.output.messages"),
            "output.messages must NOT be uploaded when traceEnabled=false"
        );

        // token 消耗与模型元数据仍需上传
        assert_eq!(
            r.get("gen_ai.usage.input_tokens").map(String::as_str),
            Some("100")
        );
        assert_eq!(
            r.get("gen_ai.usage.output_tokens").map(String::as_str),
            Some("50")
        );
        assert_eq!(
            r.get("gen_ai.provider.name").map(String::as_str),
            Some("openai")
        );
        assert_eq!(
            r.get("gen_ai.request.model").map(String::as_str),
            Some("gpt-4")
        );
        assert_eq!(r.get("agentsight.pid").map(String::as_str), Some("42"));
        assert_eq!(
            r.get("agentsight.duration_ns").map(String::as_str),
            Some("4000")
        );
        // 不允许任何对话内容字段泄漏：包括 .messages 结尾的字段以及 system_instructions
        for key in r.keys() {
            assert!(
                !key.ends_with(".messages") && key != "gen_ai.system_instructions",
                "unexpected conversation-content field leaked when traceEnabled=false: {key}",
            );
        }
    }

    #[test]
    fn test_trace_enabled_false_does_not_affect_non_llmcall_events() {
        // 轨迹关闭对 ToolUse / AgentInteraction / StreamChunk 本身不增加过滤逻辑
        // （这些事件本来就不包含 input/output messages）
        use crate::genai::semantic::ToolUse;
        let tool = ToolUse {
            tool_use_id: "tu-1".to_string(),
            timestamp_ns: 0,
            tool_name: "shell".to_string(),
            arguments: serde_json::Value::Null,
            result: None,
            duration_ns: Some(1000),
            success: true,
            error: None,
            parent_llm_call_id: Some("parent-1".to_string()),
            pid: 7,
        };
        let event = GenAISemanticEvent::ToolUse(tool);
        let records = events_to_flat_records(&[event], None, false);
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(
            r.get("gen_ai.operation.name").map(String::as_str),
            Some("tool_use")
        );
        assert_eq!(r.get("gen_ai.tool.name").map(String::as_str), Some("shell"));
        assert_eq!(r.get("agentsight.pid").map(String::as_str), Some("7"));
    }

    // Serialize tests that mutate the process-wide SLS_LOGTAIL_FILE env var
    // or the global dynamic logtail path.
    //
    // Background tests also acquire this lock because `logtail_path()` checks
    // the environment variable before the dynamic path, so any concurrent
    // env-var mutation would otherwise make their path assertions flaky.
    pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn reset_logtail_state() {
        // SAFETY: tests acquire ENV_LOCK before mutating this variable,
        // so no other test thread can observe a race.
        unsafe { std::env::remove_var(LOGTAIL_ENV_VAR) };
        set_dynamic_logtail_path("");
    }

    #[test]
    fn test_active_logtail_paths_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        reset_logtail_state();
        let paths = active_logtail_paths();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from(DEFAULT_SLS_LOGTAIL_PATH));
    }

    #[test]
    fn test_active_logtail_paths_env_combined_with_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        reset_logtail_state();
        let tmp = std::env::temp_dir().join(format!("agentsight_logtail_{}", std::process::id()));
        let env_path = tmp.join("env.jsonl");
        // SAFETY: tests acquire ENV_LOCK before mutating this variable.
        unsafe { std::env::set_var(LOGTAIL_ENV_VAR, env_path.to_str().unwrap()) };

        let paths = active_logtail_paths();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&PathBuf::from(DEFAULT_SLS_LOGTAIL_PATH)));
        assert!(paths.contains(&env_path));

        std::fs::remove_dir_all(&tmp).ok();
        reset_logtail_state();
    }

    #[test]
    fn test_active_logtail_paths_env_equals_default_deduped() {
        let _guard = ENV_LOCK.lock().unwrap();
        reset_logtail_state();
        // SAFETY: tests acquire ENV_LOCK before mutating this variable.
        unsafe { std::env::set_var(LOGTAIL_ENV_VAR, DEFAULT_SLS_LOGTAIL_PATH) };

        let paths = active_logtail_paths();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from(DEFAULT_SLS_LOGTAIL_PATH));
        reset_logtail_state();
    }

    #[test]
    fn test_active_logtail_paths_sysom_only() {
        let _guard = ENV_LOCK.lock().unwrap();
        reset_logtail_state();
        let sysom_path = "/var/sysom/ilog/agentsight/agentsight.jsonl";
        // SAFETY: tests acquire ENV_LOCK before mutating this variable.
        unsafe { std::env::set_var(LOGTAIL_ENV_VAR, sysom_path) };

        let paths = active_logtail_paths();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from(sysom_path));
        reset_logtail_state();
    }

    #[test]
    fn test_logtail_exporter_new_default_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        reset_logtail_state();
        let exporter = LogtailExporter::new_default(None, false);
        assert_eq!(exporter.path(), PathBuf::from(DEFAULT_SLS_LOGTAIL_PATH));
        reset_logtail_state();
    }

    #[test]
    fn test_default_exporter_skips_when_path_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        reset_logtail_state();

        // Use a path under a temp directory that does not exist.
        let tmp =
            std::env::temp_dir().join(format!("agentsight_default_missing_{}", std::process::id()));
        let exporter = LogtailExporter {
            path: tmp.join("agentsight.jsonl"),
            encryptor: None,
            trace_enabled: true,
            dynamic: false,
            require_path_exists: true,
        };

        let event = GenAISemanticEvent::LLMCall(make_full_llm_call());
        exporter.export(&[event]);

        assert!(!tmp.exists());
        reset_logtail_state();
    }

    #[test]
    fn test_default_exporter_writes_when_path_exists() {
        let _guard = ENV_LOCK.lock().unwrap();
        reset_logtail_state();

        let tmp =
            std::env::temp_dir().join(format!("agentsight_default_exists_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("agentsight.jsonl");
        std::fs::write(&path, "").unwrap();

        let exporter = LogtailExporter {
            path: path.clone(),
            encryptor: None,
            trace_enabled: true,
            dynamic: false,
            require_path_exists: true,
        };

        let event = GenAISemanticEvent::LLMCall(make_full_llm_call());
        exporter.export(&[event]);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.is_empty());
        assert!(content.contains("\"gen_ai.operation.name\""));

        std::fs::remove_dir_all(&tmp).ok();
        reset_logtail_state();
    }

    #[test]
    fn test_logtail_exporter_new_with_fixed_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        reset_logtail_state();
        let tmp = std::env::temp_dir().join(format!("agentsight_fixed_{}", std::process::id()));
        let path = tmp.join("fixed.jsonl");
        let exporter = LogtailExporter::new_with_fixed_path(path.to_str().unwrap(), None, false);
        assert_eq!(exporter.path(), path);
        std::fs::remove_dir_all(&tmp).ok();
        reset_logtail_state();
    }

    #[test]
    fn test_skill_fields_in_llm_call_records() {
        let mut call = make_full_llm_call();
        call.response.messages.push(OutputMessage {
            role: "assistant".to_string(),
            parts: vec![
                MessagePart::ToolCall {
                    id: Some("tc-1".to_string()),
                    name: "Skill".to_string(),
                    arguments: Some(serde_json::json!({"skill": "pdf"})),
                },
                MessagePart::ToolCall {
                    id: Some("tc-2".to_string()),
                    name: "read_file".to_string(),
                    arguments: Some(serde_json::json!({"file_path": "/skills/read/SKILL.md"})),
                },
            ],
            name: None,
            finish_reason: None,
        });

        let event = GenAISemanticEvent::LLMCall(call);
        let records = events_to_flat_records(&[event], None, false);
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(
            r.get("agent.skill.name").map(String::as_str),
            Some("[\"pdf\",\"read\"]")
        );
        assert_eq!(
            r.get("agent.skill.load_count").map(String::as_str),
            Some("[1,1]")
        );
    }

    #[test]
    fn test_skill_fields_default_null_when_no_skills() {
        // When no skill tool calls are present, fields should default to null.
        let event = GenAISemanticEvent::LLMCall(make_full_llm_call());
        let records = events_to_flat_records(&[event], None, false);
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.get("agent.skill.name").map(String::as_str), Some("null"));
        assert_eq!(
            r.get("agent.skill.load_count").map(String::as_str),
            Some("null")
        );
    }

    #[test]
    fn test_export_interruption_events_writes_records() {
        let _guard = ENV_LOCK.lock().unwrap();
        reset_logtail_state();
        let tmp =
            std::env::temp_dir().join(format!("agentsight_interruption_{}", std::process::id()));
        let path = tmp.join("interruption.jsonl");
        // SAFETY: tests acquire ENV_LOCK before mutating this variable.
        unsafe { std::env::set_var(LOGTAIL_ENV_VAR, path.to_str().unwrap()) };

        let event = InterruptionEvent::new(
            crate::interruption::InterruptionType::AgentCrash,
            Some("session-1".to_string()),
            None,
            Some("conv-1".to_string()),
            None,
            Some(42),
            Some("test-agent".to_string()),
            1_000_000,
            Some(serde_json::json!({"pid": 42})),
        );
        export_interruption_events(&[event]);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.is_empty());
        assert!(content.contains("agent_crash"));
        assert!(content.contains("session-1"));

        std::fs::remove_dir_all(&tmp).ok();
        reset_logtail_state();
    }

    #[test]
    fn test_export_interruption_events_env_path_creates_if_missing() {
        // Env-variable paths keep the original create-if-missing behavior.
        let _guard = ENV_LOCK.lock().unwrap();
        reset_logtail_state();
        let tmp = std::env::temp_dir().join(format!(
            "agentsight_interruption_env_{}",
            std::process::id()
        ));
        let path = tmp.join("interruption.jsonl");
        // SAFETY: tests acquire ENV_LOCK before mutating this variable.
        unsafe { std::env::set_var(LOGTAIL_ENV_VAR, path.to_str().unwrap()) };

        let event = InterruptionEvent::new(
            crate::interruption::InterruptionType::AgentCrash,
            Some("session-1".to_string()),
            None,
            Some("conv-1".to_string()),
            None,
            Some(42),
            Some("test-agent".to_string()),
            1_000_000,
            Some(serde_json::json!({"pid": 42})),
        );
        export_interruption_events(&[event]);

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("agent_crash"));

        std::fs::remove_dir_all(&tmp).ok();
        reset_logtail_state();
    }

    #[test]
    fn test_export_interruption_events_skips_missing_default_path() {
        // When no env var is set, active_logtail_paths() returns the default path.
        // The default path should not be auto-created if it does not exist.
        let _guard = ENV_LOCK.lock().unwrap();
        reset_logtail_state();

        let default = std::path::PathBuf::from(DEFAULT_SLS_LOGTAIL_PATH);
        // Clean up any leftover file/directory from prior runs so the assertion is valid.
        if default.exists() {
            std::fs::remove_file(&default).ok();
            if let Some(parent) = default.parent() {
                std::fs::remove_dir(parent).ok();
            }
        }
        assert!(!default.exists());

        let event = InterruptionEvent::new(
            crate::interruption::InterruptionType::AgentCrash,
            Some("session-1".to_string()),
            None,
            Some("conv-1".to_string()),
            None,
            Some(42),
            Some("test-agent".to_string()),
            1_000_000,
            Some(serde_json::json!({"pid": 42})),
        );
        export_interruption_events(&[event]);

        assert!(!default.exists());
        reset_logtail_state();
    }

    #[test]
    fn test_slim_records_keeps_only_whitelisted_llm_fields() {
        // 完整 LLMCall 记录经精简后，仅保留白名单业务字段与 iLogtail 保留字段。
        let event = GenAISemanticEvent::LLMCall(make_full_llm_call());
        let mut records = events_to_flat_records(&[event], None, true);
        assert_eq!(records.len(), 1);

        // 精简前应包含将被删除的字段
        assert!(records[0].contains_key("gen_ai.input.messages"));
        assert!(records[0].contains_key("gen_ai.request.temperature"));
        assert!(records[0].contains_key("agentsight.pid"));

        slim_records_for_default_path(&mut records);
        let r = &records[0];

        // 保留：白名单业务字段
        assert_eq!(
            r.get("gen_ai.provider.name").map(String::as_str),
            Some("openai")
        );
        assert_eq!(
            r.get("gen_ai.request.model").map(String::as_str),
            Some("gpt-4")
        );
        assert_eq!(
            r.get("gen_ai.usage.input_tokens").map(String::as_str),
            Some("100")
        );
        assert_eq!(
            r.get("gen_ai.usage.output_tokens").map(String::as_str),
            Some("50")
        );
        assert!(r.contains_key("gen_ai.operation.name"));
        assert!(r.contains_key("agent.skill.name"));
        assert!(r.contains_key("agent.skill.load_count"));

        // 删除：iLogtail 系统字段
        assert!(!r.contains_key("__time__"));
        assert!(!r.contains_key("__source__"));
        assert!(!r.contains_key("__topic__"));

        // 删除：对话内容、请求参数、时间戳明细、instance/uid 等
        assert!(!r.contains_key("gen_ai.input.messages"));
        assert!(!r.contains_key("gen_ai.output.messages"));
        assert!(!r.contains_key("gen_ai.system_instructions"));
        assert!(!r.contains_key("gen_ai.request.temperature"));
        assert!(!r.contains_key("agentsight.pid"));
        assert!(!r.contains_key("agentsight.duration_ns"));
        assert!(!r.contains_key("gen_ai.response.id"));
        assert!(!r.contains_key("instance"));
    }

    #[test]
    fn test_slim_records_keeps_only_whitelisted_interruption_fields() {
        // 中断记录经精简后，仅保留 interruption.type / severity。
        let event = InterruptionEvent::new(
            crate::interruption::InterruptionType::AgentCrash,
            Some("session-1".to_string()),
            None,
            Some("conv-1".to_string()),
            None,
            Some(42),
            Some("test-agent".to_string()),
            1_000_000,
            Some(serde_json::json!({"pid": 42})),
        );
        let mut records = interruption_events_to_flat_records(&[event]);
        assert_eq!(records.len(), 1);

        slim_records_for_default_path(&mut records);
        let r = &records[0];

        // 保留：白名单中断字段
        assert!(r.contains_key("agentsight.interruption.type"));
        assert!(r.contains_key("agentsight.interruption.severity"));

        // 删除：系统字段与会话关联、明细字段
        assert!(!r.contains_key("__time__"));
        assert!(!r.contains_key("__source__"));
        assert!(!r.contains_key("__topic__"));
        assert!(!r.contains_key("gen_ai.session.id"));
        assert!(!r.contains_key("gen_ai.conversation.id"));
        assert!(!r.contains_key("agentsight.interruption.id"));
        assert!(!r.contains_key("agentsight.interruption.detail"));
        assert!(!r.contains_key("agentsight.pid"));
    }

    #[test]
    fn test_telemetry_sentinel_absent_means_enabled() {
        // 正常环境下哨兵文件不存在，默认路径遥测不应被禁用。
        // 测试不创建 /etc 下文件，避免污染系统与依赖 root 权限；
        // 仅在哨兵文件确实不存在时断言，保证 CI 稳定。
        if !std::path::Path::new(TELEMETRY_DISABLED_SENTINEL).exists() {
            assert!(!default_path_telemetry_disabled());
        }
    }

    #[test]
    fn test_telemetry_disabled_at_reflects_sentinel_presence() {
        // 用临时文件覆盖门控核心判断的存在 / 不存在两条分支，
        // 不依赖硬编码的 /etc 哨兵路径，也无需 root。
        let tmp =
            std::env::temp_dir().join(format!("agentsight_telemetry_gate_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let sentinel = tmp.join(".telemetry_disabled");

        // 不存在 → 未禁用
        assert!(!sentinel.exists());
        assert!(!telemetry_disabled_at(&sentinel));

        // 创建后存在 → 禁用
        std::fs::write(&sentinel, "").unwrap();
        assert!(telemetry_disabled_at(&sentinel));

        // 删除后恢复 → 再次未禁用（验证实时 stat、不缓存）
        std::fs::remove_file(&sentinel).unwrap();
        assert!(!telemetry_disabled_at(&sentinel));

        std::fs::remove_dir_all(&tmp).ok();
    }
}
