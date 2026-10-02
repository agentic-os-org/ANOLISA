//! Session / Conversation ID resolver.
//!
//! 将 `session_id` 的 fallback 与 `conversation_id` 的计算改造为：
//!
//! ```text
//! session_id      = SHA256("session"      + 该 session  内最早 response_id)[..32]
//! conversation_id = SHA256("conversation" + 该 conv     内最早 response_id)[..32]
//! ```
//!
//! 同一 session / conversation 的"最早 response_id"通过两个 LRU 缓存锚定：
//! - `session_first_resp`：以 `(agent_name, pid, 首条 user message)` 的 SHA256 作为 key
//! - `conv_first_resp`   ：以 `(agent_name, pid, 最后一条 user message)` 的 SHA256 作为 key
//!
//! 在 key 中加入 `agent_name + pid` 维度是为了避免同机不同 agent（或
//! 同 agent 不同进程）在用户输入相同时撞到同一 LRU bucket 从而获得
//! 相同的 session_id / conversation_id。
//!
//! 加域前缀（"session" / "conversation"）实现域分离，确保两类 ID 不会
//! 在 `first_response_id` 相同的边角情况下发生哈希碰撞。
//!
//! 当传入的 `response_id` 为空、或 user message 文本为空时直接返回 `None`，
//! 由调用方决定是否回写元数据；后续 `complete_pending` 阶段会带着真正的
//! response_id 再次调用本模块完成回填。
//!
//! ## 滑动窗口 TTL：兜底的时间锚点
//!
//! 除了 `finish_conversation` 这种由调用方显式驱动的"轮结束"信号外，
//! `conv_first_resp` 的每条锚定记录还带有 `anchored_at` 时间戳，构成一个
//! **滑动窗口 TTL**：
//! - 命中即续期：只要这个 key 还在被同一轮的后续调用（如工具调用循环）
//!   持续访问，`anchored_at` 就会不断刷新，真正还在进行中的轮次不会
//!   被误判为过期；
//! - 静默超过 [`conversation_max_age`] 后视为过期：一旦这个 key 长时间
//!   （默认 30 分钟）没有任何调用碰过，就说明上一轮早已结束，下一次
//!   出现相同文本会重新锚定一个全新的 `response_id`。
//!
//! 这作为 `finish_conversation` 的兜底防线：即便某个 agent 集成尚未覆盖
//! 可识别的终止态 `finish_reason`，或中断检测路径本身遗漏了某个场景，
//! 超龄的锚点最终也会自然失效，不会无限期地把不相关的新轮次并入旧轮。
//! `session_first_resp` 不设 TTL —— session_id 代表整个 agent 进程的完整
//! 会话，语义上允许跨越很长时间，不应因为时间久了就被拆分。

use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lru::LruCache;
use sha2::{Digest, Sha256};

/// Maximum number of (session_key | conv_key) → first_response_id entries
/// kept in memory. Sized to align with `ResponseSessionMapper`.
const MAX_ENTRIES: usize = 10_000;

/// 域前缀：避免 session_id 与 conversation_id 在 first_response_id 相同时碰撞。
const SESSION_DOMAIN: &str = "session";
const CONVERSATION_DOMAIN: &str = "conversation";

/// 覆盖 conversation 锚点 TTL（秒）的环境变量名。
const CONVERSATION_MAX_AGE_ENV: &str = "AGENTSIGHT_CONVERSATION_MAX_AGE_SECS";

/// conversation 锚点的默认 TTL：30 分钟。
///
/// 一轮对话即便包含多次工具调用循环，正常情况下也会在几分钟到十几分钟内
/// 完成；30 分钟是相对宽松的上限，既能容纳偶尔较慢的长任务轮次，又能在
/// `finish_reason`/中断检测都没有触发驱逐时，为固定模板文本（如系统
/// recap nudge）或用户重发的相同 prompt 兜底止损，避免锚点无限期存活。
fn conversation_max_age() -> Duration {
    std::env::var(CONVERSATION_MAX_AGE_ENV)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(30 * 60))
}

/// LRU 中缓存的一条锚定记录：首次见到该 key 时的 `response_id`，
/// 以及最近一次被访问（创建或命中续期）的时间点。
struct Anchor {
    response_id: String,
    anchored_at: Instant,
}

impl Anchor {
    fn new(response_id: &str, now: Instant) -> Self {
        Anchor {
            response_id: response_id.to_string(),
            anchored_at: now,
        }
    }
}

/// `IdResolver` 负责把"内容 key + 当前 response_id"折算为稳定的
/// session_id / conversation_id。
///
/// 内部使用 `Mutex<LruCache<...>>`，所以可以以 `&self` 形式安全地在
/// `GenAIBuilder` 多线程上下文里调用。
pub struct IdResolver {
    /// session_key (SHA256 of first user message) → 锚定记录。不设 TTL。
    session_first_resp: Mutex<LruCache<String, Anchor>>,
    /// conversation_key (SHA256 of last user message) → 锚定记录。
    /// 受滑动窗口 TTL 约束，见模块文档。
    conv_first_resp: Mutex<LruCache<String, Anchor>>,
}

impl Default for IdResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl IdResolver {
    /// 构造一个空的 resolver，两份 LRU 容量均为 [`MAX_ENTRIES`]。
    pub fn new() -> Self {
        let cap = NonZeroUsize::new(MAX_ENTRIES).expect("MAX_ENTRIES must be non-zero");
        IdResolver {
            session_first_resp: Mutex::new(LruCache::new(cap)),
            conv_first_resp: Mutex::new(LruCache::new(cap)),
        }
    }

    /// 计算 session_id。
    ///
    /// - `agent_name`：该调用所属的 agent 名称（OpenClaw / Cosh / Hermes / ...）。
    ///   与 `pid` 一起加入 LRU key，避免同机不同 agent / 不同进程在 user query
    ///   相同时撞库。传空串也合法，会作为 key 的一部分参与哈希。
    /// - `pid`：产生本次调用的进程 ID。重启后 PID 变化会自然产生新
    ///   session_id，这与"一个 agent 进程一个会话"的语义一致。
    /// - `first_user_text`：当前请求中"第一条 user message"的原始文本。
    ///   空字符串视为无法定位 session_key，返回 `None`。
    /// - `response_id`：当前 LLM 调用的 response_id；空字符串返回 `None`。
    ///
    /// 多次调用同一 `(agent_name, pid, first_user_text)` 时锚定第一次写入的
    /// `response_id`，后续 `response_id` 即使变化，也会得到与首次相同的
    /// session_id。
    pub fn resolve_session_id(
        &self,
        agent_name: &str,
        pid: i32,
        first_user_text: &str,
        response_id: &str,
    ) -> Option<String> {
        Self::resolve(
            &self.session_first_resp,
            SESSION_DOMAIN,
            agent_name,
            pid,
            first_user_text,
            response_id,
            None, // session_id 不设 TTL，见模块文档
            0,    // session key 不受同 prompt 重复问题影响
        )
    }

    /// 计算 conversation_id。
    ///
    /// - `agent_name` / `pid`：同 `resolve_session_id`，用于隔离同机不同 agent /
    ///   不同进程。
    /// - `last_user_text`：当前请求中"最后一条 user message"的原始文本，
    ///   作为 conversation_key 的素材。空字符串返回 `None`。
    /// - `response_id`：当前调用的 response_id；空字符串返回 `None`。
    /// - `user_message_count`：请求中"真正的用户消息"条数，用于区分同进程
    ///   内相同文本的不同轮次对话。
    ///
    /// 锚定记录受滑动窗口 TTL（[`conversation_max_age`]）约束：命中即续期，
    /// 静默超龄则重新锚定，作为 `finish_conversation` 的兜底时间防线。
    pub fn resolve_conversation_id(
        &self,
        agent_name: &str,
        pid: i32,
        last_user_text: &str,
        response_id: &str,
        user_message_count: usize,
    ) -> Option<String> {
        Self::resolve(
            &self.conv_first_resp,
            CONVERSATION_DOMAIN,
            agent_name,
            pid,
            last_user_text,
            response_id,
            Some(conversation_max_age()),
            user_message_count,
        )
    }

    /// 通用实现：定位/记录"首个 response_id"，再用域前缀 + first_response_id
    /// 计算最终 ID（取 SHA256 前 32 位 hex）。
    ///
    /// `max_age`：为 `Some(ttl)` 时启用滑动窗口 TTL——若现有锚点距上次
    /// 访问已超过 `ttl`，视为过期并用本次 `response_id` 重新锚定；未过期
    /// 则命中并刷新 `anchored_at` 续期。为 `None` 时不过期，行为与原来一致。
    fn resolve(
        cache: &Mutex<LruCache<String, Anchor>>,
        domain: &str,
        agent_name: &str,
        pid: i32,
        text: &str,
        response_id: &str,
        max_age: Option<Duration>,
        user_message_count: usize,
    ) -> Option<String> {
        if text.is_empty() || response_id.is_empty() {
            return None;
        }

        let key = compose_key(agent_name, pid, text, user_message_count);
        let now = Instant::now();
        let anchored_response_id = {
            let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
            let expired = match (max_age, guard.peek(&key)) {
                (Some(ttl), Some(anchor)) => now.duration_since(anchor.anchored_at) > ttl,
                _ => false,
            };
            if !expired {
                if let Some(existing) = guard.get_mut(&key) {
                    // 命中且未过期：继续复用旧 response_id，刷新访问时间实现
                    // 滑动窗口续期，避免真正进行中的轮次因长时间运行而过期。
                    existing.anchored_at = now;
                    return Some(domain_hash(domain, &existing.response_id));
                }
            }
            // 未命中或已过期：把当前 response_id 写入作为新锚点。
            guard.put(key, Anchor::new(response_id, now));
            response_id.to_string()
        };

        Some(domain_hash(domain, &anchored_response_id))
    }

    /// 只读查询 session_id：查 LRU 中是否已锁定 `(agent, pid, first_user_text)` 的
    /// first_response_id，命中则返回与正常路径完全一致的 session_id，未命中
    /// 返回 `None`（不写入、不修改 LRU 顺序）。
    ///
    /// 主要用于 crash-drain 路径：进程崩溃、响应永远不会到达时，如果
    /// 同 PID 之前已正常完成过 LLM 调用（LRU 已 anchor），则复用同一个
    /// session_id，避免同一会话被崩溃拆分。
    pub fn peek_session_id(
        &self,
        agent_name: &str,
        pid: i32,
        first_user_text: &str,
    ) -> Option<String> {
        Self::peek(
            &self.session_first_resp,
            SESSION_DOMAIN,
            agent_name,
            pid,
            first_user_text,
            None, // session_id 不设 TTL
            0,    // session key 不受同 prompt 重复问题影响
        )
    }

    /// 只读查询 conversation_id，用途与 `peek_session_id` 同。同样受滑动窗口 TTL
    /// 约束：若锚点已超龄，视为未命中返回 `None`（与 `resolve_conversation_id`
    /// 的过期判断保持一致，但本身不写入/不刷新访问时间）。
    pub fn peek_conversation_id(
        &self,
        agent_name: &str,
        pid: i32,
        last_user_text: &str,
        user_message_count: usize,
    ) -> Option<String> {
        Self::peek(
            &self.conv_first_resp,
            CONVERSATION_DOMAIN,
            agent_name,
            pid,
            last_user_text,
            Some(conversation_max_age()),
            user_message_count,
        )
    }

    /// 标记一轮对话（turn）已经结束，驱逐 `(agent_name, pid, last_user_text)` 对应的
    /// conversation anchor，使下一次出现相同 `last_user_text` 时重新锁定一个全新的
    /// `first_response_id`，从而得到不同的 conversation_id。
    ///
    /// # 背景
    ///
    /// `resolve_conversation_id` 按设计需要让*同一轮*对话内的多次 LLM 调用（例如带
    /// 工具调用的多步 agent loop）共享同一个 conversation_id，因此对同一个
    /// `(agent_name, pid, last_user_text)` key 永久锚定“第一次见到”的 response_id。
    ///
    /// 但当同一段文本（例如固定模板的系统 nudge，如“The user stepped away and is
    /// coming back...”）被复用于*不同*的真实对话轮次时，这个永久锚定会导致数十分钟
    /// 甚至更久之后的全新对话轮次仍然复用第一次的锚点，产生相同的 conversation_id，
    /// 继而让下游（如按 conversation_id 分组计数 step 的日志处理管道）误判为同一轮
    /// 对话仍在继续。
    ///
    /// 调用方应在检测到本轮对话的终止信号（例如 `finish_reason` 不是
    /// `tool_calls`/`tool_use` 等表示流程继续的取值）后调用本方法，显式结束该 key
    /// 的锚定，使后续相同文本重新开启一轮对话。
    pub fn finish_conversation(
        &self,
        agent_name: &str,
        pid: i32,
        last_user_text: &str,
        user_message_count: usize,
    ) {
        if last_user_text.is_empty() {
            return;
        }
        let key = compose_key(agent_name, pid, last_user_text, user_message_count);
        let mut guard = self
            .conv_first_resp
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.pop(&key);
    }

    /// `peek_*` 的通用实现：仅查询 LRU，不写入、不提升顺序。
    ///
    /// `max_age`、过期判断逻辑与 `resolve` 一致，但命中时不会刷新
    /// `anchored_at`（保持本函数"只读不写"的语义）。
    fn peek(
        cache: &Mutex<LruCache<String, Anchor>>,
        domain: &str,
        agent_name: &str,
        pid: i32,
        text: &str,
        max_age: Option<Duration>,
        user_message_count: usize,
    ) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        let key = compose_key(agent_name, pid, text, user_message_count);
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        // `LruCache::peek` 不会提升条目顺序，适合 crash 路径“只读不写”。
        let anchor = guard.peek(&key)?;
        if let Some(ttl) = max_age {
            if Instant::now().duration_since(anchor.anchored_at) > ttl {
                return None;
            }
        }
        Some(domain_hash(domain, &anchor.response_id))
    }
}

/// crash-drain 路径专用 fallback ID。
///
/// 仅在 PID 第一个请求就崩、`peek_*` 未命中 LRU 时使用。调用方需传入：
/// - `domain`: “session” 或 “conversation”（函数内部会加 “crash-” 前缀做域隔离）
/// - `agent_name`/`pid`: 与正常路径 LRU key 维度一致，隔离同机不同 agent
/// - `user_text`: session 传 first_user_text；conversation 传 last_user_text，
///   跟正常路径”一个 user_query 一个 conversation”的语义对齐。
/// - `user_message_count`: conversation 域传入真实用户消息计数；session 域传 0。
///
/// 输出 = `SHA256(“crash-{domain}|agent_name|pid|user_text|user_message_count”)[..32]`。
/// `crash-` 前缀与正常路径的 `session`/`conversation` 前缀做域分离，避免
/// crash 兑底 ID 与正常调用 ID 碰撞。
pub fn crash_fallback_id(
    domain: &str,
    agent_name: &str,
    pid: i32,
    user_text: &str,
    user_message_count: usize,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("crash-{domain}").as_bytes());
    hasher.update(b"|");
    hasher.update(agent_name.as_bytes());
    hasher.update(b"|");
    hasher.update(pid.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(user_text.as_bytes());
    hasher.update(b"|");
    hasher.update(user_message_count.to_string().as_bytes());
    let digest = hasher.finalize();
    let full = format!("{digest:x}");
    full[..32].to_string()
}

/// 组装 LRU key：SHA256(agent_name + "|" + pid + "|" + text + "|" + user_message_count)
/// 的 hex。
///
/// `user_message_count` 是请求中"真正的用户消息"条数（role=user 且含非空文本，
/// 排除仅含 tool_result 的消息）。同一轮工具循环内该值不变，用户发新消息时
/// 必然 +1，从而在不依赖 finish_reason 的前提下结构性避免不同轮次撞 key。
///
/// 使用哈希后的定长字符串作为 key，避免原始 user message 过长占用内存；
/// `|` 作为字段分隔符避免 "a"+"|b" 与 "a|"+"b" 这类拼接哈希冲突。
fn compose_key(agent_name: &str, pid: i32, text: &str, user_message_count: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(agent_name.as_bytes());
    hasher.update(b"|");
    hasher.update(pid.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(text.as_bytes());
    hasher.update(b"|");
    hasher.update(user_message_count.to_string().as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 计算 `SHA256(domain + first_response_id)` 的前 32 位 hex 表示。
fn domain_hash(domain: &str, first_response_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(first_response_id.as_bytes());
    let digest = hasher.finalize();
    let full = format!("{digest:x}");
    full[..32].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试辅助：默认 agent / pid。
    const A: &str = "openclaw";
    const P: i32 = 1001;

    #[test]
    fn resolve_session_id_is_stable_across_calls() {
        let resolver = IdResolver::new();
        let first = resolver
            .resolve_session_id(A, P, "user-A", "resp-1")
            .unwrap();
        // 即便后续 response_id 变了，同一 (agent, pid, first_user_text) 仍返回首次结果
        let again = resolver
            .resolve_session_id(A, P, "user-A", "resp-2")
            .unwrap();
        assert_eq!(first, again);
        assert_eq!(first.len(), 32);
    }

    #[test]
    fn resolve_session_id_changes_with_first_response_id() {
        // 不同 first_user_text 处于不同 LRU bucket，各自锁定首次看到的
        // response_id；只要首次看到的 response_id 不同，得到的 session_id
        // 就会不同。
        let resolver = IdResolver::new();
        let a = resolver
            .resolve_session_id(A, P, "user-A", "resp-1")
            .unwrap();
        let b = resolver
            .resolve_session_id(A, P, "user-B", "resp-2")
            .unwrap();
        assert_ne!(a, b, "不同首次 response_id 应产生不同 session_id");
    }

    #[test]
    fn resolve_conversation_id_changes_with_last_user_text() {
        let resolver = IdResolver::new();
        let a = resolver
            .resolve_conversation_id(A, P, "turn-1", "resp-1", 1)
            .unwrap();
        let b = resolver
            .resolve_conversation_id(A, P, "turn-2", "resp-2", 1)
            .unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn finish_conversation_lets_repeated_text_start_a_new_turn() {
        // 复现真实场景：固定模板文本（如系统 recap nudge）在同一 pid/agent 下
        // 被复用于两轮相隔很久的真实对话。turn 结束后应显式驱逐锚点，
        // 使下一次出现相同文本时锁定新的 response_id，得到不同的 conversation_id。
        let resolver = IdResolver::new();
        let recap_text = "The user stepped away and is coming back. Recap...";

        let turn1 = resolver
            .resolve_conversation_id(A, P, recap_text, "resp-turn1", 1)
            .unwrap();
        // 模拟同一轮内的后续 LLM 调用（工具调用循环），conversation_id 应保持稳定。
        let turn1_again = resolver
            .resolve_conversation_id(A, P, recap_text, "resp-turn1-followup", 1)
            .unwrap();
        assert_eq!(turn1, turn1_again, "同一轮内多次调用应共享 conversation_id");

        // 检测到本轮 finish_reason=stop（非 tool_calls），显式结束该轮锚定。
        resolver.finish_conversation(A, P, recap_text, 1);

        // 数十分钟后，同一段固定文本触发了一轮全新的真实对话。
        let turn2 = resolver
            .resolve_conversation_id(A, P, recap_text, "resp-turn2", 1)
            .unwrap();
        assert_ne!(
            turn1, turn2,
            "finish_conversation 后相同文本应开启新的一轮对话"
        );
    }

    #[test]
    fn finish_conversation_on_unseen_key_is_noop() {
        // 从未 resolve 过的 key 调用 finish_conversation 不应 panic 或产生副作用。
        let resolver = IdResolver::new();
        resolver.finish_conversation(A, P, "never-seen", 1);
        let id = resolver
            .resolve_conversation_id(A, P, "never-seen", "resp-1", 1)
            .unwrap();
        assert_eq!(id.len(), 32);
    }

    #[test]
    fn finish_conversation_does_not_affect_session_id() {
        // finish_conversation 仅驱逐 conversation anchor，不应影响 session_id
        // （session_id 代表整个进程会话，语义上不随单轮对话结束而重置）。
        let resolver = IdResolver::new();
        let text = "hello";
        let session1 = resolver.resolve_session_id(A, P, text, "resp-1").unwrap();
        resolver.finish_conversation(A, P, text, 1);
        let session2 = resolver.resolve_session_id(A, P, text, "resp-2").unwrap();
        assert_eq!(
            session1, session2,
            "finish_conversation 不应影响 session_id 锚点"
        );
    }

    #[test]
    fn resolve_returns_none_when_response_id_empty() {
        let resolver = IdResolver::new();
        assert!(resolver.resolve_session_id(A, P, "user-A", "").is_none());
        assert!(
            resolver
                .resolve_conversation_id(A, P, "turn-1", "", 1)
                .is_none()
        );
    }

    #[test]
    fn resolve_returns_none_when_text_empty() {
        let resolver = IdResolver::new();
        assert!(resolver.resolve_session_id(A, P, "", "resp-1").is_none());
        assert!(
            resolver
                .resolve_conversation_id(A, P, "", "resp-1", 1)
                .is_none()
        );
    }

    #[test]
    fn session_and_conversation_diverge_on_same_first_response_id() {
        let resolver = IdResolver::new();
        // 故意构造 session_key 与 conversation_key 相同（同一段文本既是首条
        // 又是最后一条 user message 时的真实场景），但仍应得到不同 ID。
        let text = "single-turn";
        let resp = "resp-1";
        let s = resolver.resolve_session_id(A, P, text, resp).unwrap();
        let c = resolver
            .resolve_conversation_id(A, P, text, resp, 1)
            .unwrap();
        assert_ne!(s, c, "域前缀应保证两类 ID 不会碰撞");
    }

    #[test]
    fn different_agents_with_same_user_text_get_different_ids() {
        // 同机不同 agent，用户输入完全相同。生产中两个 agent 的 LLM
        // 调用会各自拿到不同的 response_id，加入 (agent_name, pid) 作为 LRU
        // key 后两个调用会各自锁定自己的首个 response_id，不会被同机
        // 其他 agent “首访”串走。
        let resolver = IdResolver::new();
        let openclaw = resolver
            .resolve_session_id("openclaw", 1001, "今天天气", "chatcmpl-A")
            .unwrap();
        let cosh = resolver
            .resolve_session_id("cosh", 2002, "今天天气", "chatcmpl-B")
            .unwrap();
        assert_ne!(openclaw, cosh, "同机不同 agent 不可联合为同一 session");

        let openclaw_conv = resolver
            .resolve_conversation_id("openclaw", 1001, "今天天气", "chatcmpl-A", 1)
            .unwrap();
        let cosh_conv = resolver
            .resolve_conversation_id("cosh", 2002, "今天天气", "chatcmpl-B", 1)
            .unwrap();
        assert_ne!(openclaw_conv, cosh_conv);
    }

    #[test]
    fn same_user_text_does_not_leak_response_id_across_agents() {
        // 验证 LRU 分桶：同一 user_text，不同 agent 各自锁定自己首访的
        // response_id。即 OpenClaw 先到后，Cosh 后到不会被 OpenClaw 的
        // response_id “传染”。
        let resolver = IdResolver::new();
        let openclaw_first = resolver
            .resolve_session_id("openclaw", 1001, "hello", "chatcmpl-A")
            .unwrap();
        // Cosh 后到，拿到的应该是自己的 chatcmpl-B 作为 first_response_id，
        // 而不是复用 chatcmpl-A。
        let cosh_first = resolver
            .resolve_session_id("cosh", 2002, "hello", "chatcmpl-B")
            .unwrap();
        // OpenClaw 后续调用仍锁定 chatcmpl-A
        let openclaw_second = resolver
            .resolve_session_id("openclaw", 1001, "hello", "chatcmpl-X")
            .unwrap();
        assert_eq!(openclaw_first, openclaw_second, "OpenClaw 多轮调用应稳定");
        assert_ne!(openclaw_first, cosh_first, "不同 agent 不会撞库");
    }

    #[test]
    fn same_agent_different_pids_get_different_ids() {
        // 同 agent 两个进程实例（如重启后），同样 user query 应产生
        // 不同会话 ID，符合"一个进程 = 一个会话"语义。
        let resolver = IdResolver::new();
        let p1 = resolver
            .resolve_session_id("openclaw", 1001, "hello", "resp-1")
            .unwrap();
        let p2 = resolver
            .resolve_session_id("openclaw", 1002, "hello", "resp-2")
            .unwrap();
        assert_ne!(p1, p2);
    }

    // ── peek_* 只读查询接口测试 ──

    #[test]
    fn peek_session_id_returns_none_when_lru_empty() {
        let resolver = IdResolver::new();
        assert!(resolver.peek_session_id(A, P, "unseen").is_none());
        assert!(resolver.peek_conversation_id(A, P, "unseen", 1).is_none());
    }

    #[test]
    fn peek_session_id_matches_resolve_when_anchored() {
        // 验证 crash 路径与正常路径自动对齐：resolve 写入后，peek 返回
        // 与 resolve 完全相同的 ID。
        let resolver = IdResolver::new();
        let normal = resolver
            .resolve_session_id(A, P, "hello", "chatcmpl-A")
            .unwrap();
        let peeked = resolver.peek_session_id(A, P, "hello").unwrap();
        assert_eq!(normal, peeked, "peek 应返回与正常路径一致的 session_id");

        let normal_conv = resolver
            .resolve_conversation_id(A, P, "world", "chatcmpl-B", 1)
            .unwrap();
        let peeked_conv = resolver.peek_conversation_id(A, P, "world", 1).unwrap();
        assert_eq!(normal_conv, peeked_conv);
    }

    #[test]
    fn peek_returns_none_when_text_empty() {
        let resolver = IdResolver::new();
        // 即使 LRU 中有条目，空 text 仍返回 None
        let _ = resolver.resolve_session_id(A, P, "x", "resp");
        assert!(resolver.peek_session_id(A, P, "").is_none());
        assert!(resolver.peek_conversation_id(A, P, "", 1).is_none());
    }

    // ── crash_fallback_id 测试 ──

    #[test]
    fn crash_fallback_id_stable_for_same_inputs() {
        let a = crash_fallback_id("session", "openclaw", 1001, "hello", 0);
        let b = crash_fallback_id("session", "openclaw", 1001, "hello", 0);
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn crash_fallback_id_diverges_session_vs_conversation() {
        let s = crash_fallback_id("session", "openclaw", 1001, "hello", 0);
        let c = crash_fallback_id("conversation", "openclaw", 1001, "hello", 0);
        assert_ne!(s, c, "同输入下 session/conversation 域返回不同值");
    }

    #[test]
    fn crash_fallback_id_diverges_with_different_user_text() {
        // 验证 user_query 粒度分桶：同 PID 同 agent，但不同 user_text
        // 产生不同的 crash fallback ID。
        let a = crash_fallback_id("session", "openclaw", 1001, "query-A", 0);
        let b = crash_fallback_id("session", "openclaw", 1001, "query-B", 0);
        assert_ne!(a, b);
    }

    /// After intentionally poisoning the session LRU cache mutex,
    /// resolve should still operate via poison recovery.
    #[test]
    fn poison_recovery_cache_still_operational() {
        let resolver = IdResolver::new();

        // Poison the session_first_resp mutex
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = resolver.session_first_resp.lock().unwrap();
            panic!("intentional poison");
        }));
        assert!(result.is_err(), "Mutex should be poisoned");

        // Exercise the poison-recovery path
        let sid = resolver
            .resolve_session_id(A, P, "poison-test", "resp-1")
            .unwrap();
        assert_eq!(sid.len(), 32);

        // Same for conv cache — poison it, then exercise
        let result2 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = resolver.conv_first_resp.lock().unwrap();
            panic!("intentional poison");
        }));
        assert!(result2.is_err(), "Mutex should be poisoned");

        let cid = resolver
            .resolve_conversation_id(A, P, "poison-conv", "resp-2", 1)
            .unwrap();
        assert_eq!(cid.len(), 32);
    }

    #[test]
    fn crash_fallback_id_diverges_from_normal_id() {
        // 以同一 user_text 分别走正常路径与 crash fallback，两者应因为
        // 域前缀 ("session" vs "crash-session") 不同而不冲突。
        let resolver = IdResolver::new();
        let normal = resolver
            .resolve_session_id("openclaw", 1001, "hello", "chatcmpl-A")
            .unwrap();
        let crash = crash_fallback_id("session", "openclaw", 1001, "hello", 0);
        assert_ne!(
            normal, crash,
            "正常 ID 与 crash fallback 需严格隔离以避免下游聚合错乱"
        );
    }

    // ── 滑动窗口 TTL 测试 ──
    // 直接调用私有的 `resolve`/`peek` 并传入显式 `max_age`，而不是通过
    // 公开包装函数 + 环境变量，避免进程级环境变量在并发测试中产生竞态。

    #[test]
    fn conversation_ttl_expires_stale_anchor_and_creates_new_one() {
        let resolver = IdResolver::new();
        let ttl = Duration::from_millis(0);
        let turn1 = IdResolver::resolve(
            &resolver.conv_first_resp,
            CONVERSATION_DOMAIN,
            A,
            P,
            "recap nudge",
            "resp-1",
            Some(ttl),
            1,
        )
        .unwrap();
        // TTL=0：哪怕紧接着的下一次调用，也应视为已经超龄，重新锚定。
        std::thread::sleep(Duration::from_millis(2));
        let turn2 = IdResolver::resolve(
            &resolver.conv_first_resp,
            CONVERSATION_DOMAIN,
            A,
            P,
            "recap nudge",
            "resp-2",
            Some(ttl),
            1,
        )
        .unwrap();
        assert_ne!(
            turn1, turn2,
            "TTL 到期后应重新锚定，得到不同的 conversation_id"
        );
    }

    #[test]
    fn conversation_sliding_window_refreshes_on_hit() {
        // 设置一个相对宽松的 TTL，在 TTL 内持续访问，验证每次命中都会
        // 刷新续期、不会过期。
        let resolver = IdResolver::new();
        let ttl = Duration::from_secs(5);
        let turn1 = IdResolver::resolve(
            &resolver.conv_first_resp,
            CONVERSATION_DOMAIN,
            A,
            P,
            "hello",
            "resp-1",
            Some(ttl),
            1,
        )
        .unwrap();
        for _ in 0..3 {
            let hit = IdResolver::resolve(
                &resolver.conv_first_resp,
                CONVERSATION_DOMAIN,
                A,
                P,
                "hello",
                "resp-ignored",
                Some(ttl),
                1,
            )
            .unwrap();
            assert_eq!(turn1, hit, "TTL 内命中应保持稳定，并刷新续期");
        }
    }

    #[test]
    fn peek_conversation_id_respects_ttl_expiry() {
        let resolver = IdResolver::new();
        let ttl = Duration::from_millis(0);
        IdResolver::resolve(
            &resolver.conv_first_resp,
            CONVERSATION_DOMAIN,
            A,
            P,
            "hello",
            "resp-1",
            Some(ttl),
            1,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        assert!(
            IdResolver::peek(
                &resolver.conv_first_resp,
                CONVERSATION_DOMAIN,
                A,
                P,
                "hello",
                Some(ttl),
                1,
            )
            .is_none(),
            "过期锚点应被 peek 视为未命中"
        );
    }

    #[test]
    fn conversation_ttl_does_not_affect_session_id() {
        // session_id 走 resolve_session_id，内部固定传 max_age=None，不受
        // conversation 域的 TTL 配置影响，即便时间过去也应保持稳定。
        let resolver = IdResolver::new();
        let s1 = resolver
            .resolve_session_id(A, P, "hello", "resp-1")
            .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let s2 = resolver
            .resolve_session_id(A, P, "hello", "resp-2")
            .unwrap();
        assert_eq!(s1, s2, "session_id 不设 TTL，应保持稳定");
    }

    // ── user_message_count 维度测试 ──

    #[test]
    fn resolve_conversation_id_same_text_different_count_gives_different_ids() {
        // 同一文本、不同 user_message_count 应落入不同 LRU bucket，各自锚定
        // 独立的 first_response_id，从而产生不同的 conversation_id。
        let resolver = IdResolver::new();
        let id1 = resolver
            .resolve_conversation_id(A, P, "hello", "resp-1", 1)
            .unwrap();
        let id2 = resolver
            .resolve_conversation_id(A, P, "hello", "resp-2", 2)
            .unwrap();
        assert_ne!(
            id1, id2,
            "同文本不同 user_message_count 应产生不同的 conversation_id"
        );
    }

    #[test]
    fn resolve_conversation_id_same_text_same_count_stable() {
        // 同一文本、同一 user_message_count，即使 response_id 变化，
        // 仍应返回首次锚定的 conversation_id（LRU 命中续期）。
        let resolver = IdResolver::new();
        let first = resolver
            .resolve_conversation_id(A, P, "hello", "resp-1", 1)
            .unwrap();
        let again = resolver
            .resolve_conversation_id(A, P, "hello", "resp-2", 1)
            .unwrap();
        assert_eq!(
            first, again,
            "同文本同 count 多次调用应锁定同一 conversation_id"
        );
    }

    #[test]
    fn finish_conversation_with_count_evicts_correct_bucket() {
        // finish_conversation 应仅驱逐指定 count 的锚点，不影响其他 count 的
        // 锚点。验证 count=1 被驱逐后重新 resolve 得到新 ID，而 count=2 不受影响。
        let resolver = IdResolver::new();
        let text = "shared prompt";

        let id_count1 = resolver
            .resolve_conversation_id(A, P, text, "resp-1", 1)
            .unwrap();
        let id_count2 = resolver
            .resolve_conversation_id(A, P, text, "resp-2", 2)
            .unwrap();
        assert_ne!(id_count1, id_count2);

        // 仅结束 count=1 的轮次。
        resolver.finish_conversation(A, P, text, 1);

        // count=2 的锚点应不受影响。
        let id_count2_again = resolver
            .resolve_conversation_id(A, P, text, "resp-2-followup", 2)
            .unwrap();
        assert_eq!(
            id_count2, id_count2_again,
            "finish_conversation(count=1) 不应影响 count=2 的锚点"
        );

        // count=1 重新 resolve 应得到全新的 conversation_id。
        let id_count1_new = resolver
            .resolve_conversation_id(A, P, text, "resp-3", 1)
            .unwrap();
        assert_ne!(
            id_count1, id_count1_new,
            "finish_conversation 后 count=1 应重新锚定"
        );
    }

    #[test]
    fn peek_conversation_id_with_count_matches_resolve() {
        // peek 带匹配 count 应返回与 resolve 相同的 ID；
        // peek 带不同 count 应返回 None（该 bucket 不存在）。
        let resolver = IdResolver::new();
        let resolved = resolver
            .resolve_conversation_id(A, P, "query", "resp-1", 1)
            .unwrap();

        let peeked = resolver.peek_conversation_id(A, P, "query", 1).unwrap();
        assert_eq!(resolved, peeked, "peek(count=1) 应与 resolve(count=1) 一致");

        assert!(
            resolver.peek_conversation_id(A, P, "query", 2).is_none(),
            "peek(count=2) 在仅 resolve(count=1) 后应返回 None"
        );
    }

    #[test]
    fn crash_fallback_id_diverges_with_different_count() {
        // 同一文本、不同 user_message_count 应产生不同的 crash fallback ID，
        // 确保 crash 路径也按 count 维度隔离。
        let id1 = crash_fallback_id("conversation", A, P, "hello", 1);
        let id2 = crash_fallback_id("conversation", A, P, "hello", 2);
        assert_ne!(
            id1, id2,
            "不同 user_message_count 的 crash fallback ID 不可碰撞"
        );
    }
}
