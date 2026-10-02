use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::SessionTitle => "智能体会话",
        MessageId::SessionUnavailableBody => "会话恢复需要使用 cosh-core 后端。",
        MessageId::SessionBusyBody => "请先结束当前智能体任务或交互面板，再管理会话。",
        MessageId::SessionEmptyBody => "当前工作区没有已持久化的会话。",
        MessageId::SessionListFooter => "使用 /session 打开选择器，或使用 /session resume <id>。",
        MessageId::SessionStatusTitle => "会话恢复状态",
        MessageId::SessionShellIdLine => "终端会话：{id}",
        MessageId::SessionProviderIdLine => "活动模型会话：{active}\n已选择模型会话：{selected}",
        MessageId::SessionWorkspaceLine => "工作区：{workspace}",
        MessageId::SessionRecoveryLine => "恢复状态：{state}",
        MessageId::SessionErrorLine => "最近恢复错误：[{code}] {error}",
        MessageId::SessionEvidenceNotRestoredBody => {
            "历史终端证据不会恢复；仅恢复模型可见的对话上下文。"
        }
        MessageId::SessionPickerFooter => {
            "上/下或 j/k 移动 · Enter 恢复 · Space 切换清理标记 · d 打开清理确认 · Esc 取消"
        }
        MessageId::SessionPickerMarkedCount => "已标记 {count}",
        MessageId::SessionPickerMarkedFooter => {
            "上/下或 j/k 移动 · Enter 打开清理确认 · Space 切换清理标记 · d 打开清理确认 · Esc 取消"
        }
        MessageId::SessionClearConfirmTitle => "确认清理会话",
        MessageId::SessionClearConfirmCountLine => "将删除以下 {count} 个持久化会话：",
        MessageId::SessionClearConfirmFooter => "Enter 或 y 确认 · Esc、Ctrl+C 或 n 取消",
        MessageId::SessionSelectedTitle => "已选择会话",
        MessageId::SessionSelectedBody => "下次智能体请求将恢复模型会话 {id}。",
        MessageId::SessionErrorTitle => "会话恢复",
        MessageId::SessionClearedTitle => "会话已清理",
        MessageId::SessionClearedBody => "已删除 {count} 个持久化会话。",
        MessageId::SessionSkippedBody => "已跳过 {count} 个受保护或不可用的会话。",
        MessageId::SessionClearInterruptedBody => {
            "清理中断 [{code}]：{unknown} 个会话状态未知，{unattempted} 个尚未尝试。"
        }
        MessageId::SessionCancelledTitle => "会话管理已关闭",
        MessageId::SessionCancelledBody => "模型会话和持久化文件均未改变。",
        MessageId::SessionUsageBody => {
            "用法：/session [new|status|list [--all]|resume <id>|clear <id>...|clear --all|compact [status|cancel]]"
        }
        MessageId::SessionNewTitle => "全新会话",
        MessageId::SessionNewDetachedBody => {
            "已从模型会话 {id} 分离。\n下一次智能体请求将开启全新对话。\n终端工作目录、历史和设置均未改变。"
        }
        MessageId::SessionNewAlreadyFreshBody => {
            "当前没有绑定的模型会话。\n下一次智能体请求将开启全新对话。\n终端工作目录、历史和设置均未改变。"
        }
        MessageId::SessionNotReadyBody => "会话 {id} 的状态为 {health}，无法恢复，但仍可清理。",
        MessageId::SessionProtectedBody => "活动中或已选择的模型会话受保护，未被清理。",
        MessageId::SessionCompactTitle => "会话上下文压缩",
        MessageId::SessionCompactStartedBody => {
            "会话 {id} 的压缩已在后台运行。\n终端可继续使用；智能体请求已暂停。"
        }
        MessageId::SessionCompactFooter => {
            "/session compact status 查看进度 · /session compact cancel 取消"
        }
        MessageId::SessionCompactStatusTitle => "会话压缩状态",
        MessageId::SessionCompactStatusSessionLine => "会话：{id}",
        MessageId::SessionCompactStatusRunningLine => "状态：{state} · 已运行 {elapsed} 秒",
        MessageId::SessionCompactStatusIdleBody => "当前没有后台压缩任务。",
        MessageId::SessionCompactStatusRecommendedBody => {
            "已推荐自动压缩，将在下一个空闲边界启动。"
        }
        MessageId::SessionCompactStatusPendingRenderBody => {
            "压缩已完成，结果将在下一个安全提示符边界显示。"
        }
        MessageId::SessionCompactNoSessionBody => {
            "没有可恢复的活动 cosh-core 会话；请先发起智能体请求或使用 /session resume <id>。"
        }
        MessageId::SessionCompactDuplicateBody => {
            "已有压缩任务在运行；请使用 /session compact status 或 /session compact cancel。"
        }
        MessageId::SessionCompactNotRunningBody => "当前没有后台压缩任务。",
        MessageId::SessionCompactCancelRequestedBody => "已请求取消；后台压缩进程正在终止。",
        MessageId::SessionCompactPendingCancelledBody => {
            "已在启动前取消推荐的自动压缩；没有正在运行的压缩进程。\n该上下文修订不会再自动触发；上下文继续增长后仍可触发。"
        }
        MessageId::SessionCompactCompletedTitle => "上下文已在后台压缩完成",
        MessageId::SessionCompactCompletedBody => "{before} → 约 {after} tokens（{source}）",
        MessageId::SessionCompactCompletedRetainedBody => {
            "完整会话历史已保留；智能体对话已恢复可用。"
        }
        MessageId::SessionCompactFailedTitle => "会话压缩失败",
        MessageId::SessionCompactFailedBody => "[{code}] {message}",
        MessageId::SessionCompactCancelledTitle => "会话压缩已取消",
        MessageId::SessionCompactCancelledBody => "完整会话记录未改变；投影可能已在取消前提交，将使用最新的有效版本。",
        MessageId::SessionCompactAgentPausedTitle => "压缩期间智能体已暂停",
        MessageId::SessionCompactAgentPausedBody => {
            "会话压缩进行中；智能体请求已暂停。\n普通 Shell 命令仍可用。可使用 /session compact status 或 /session compact cancel。"
        }
        MessageId::SessionCompactAutoStartedBody => {
            "上下文已达可用窗口的 {percent}%；会话 {id} 的压缩已在后台运行。\n终端可继续使用；智能体请求已暂停。"
        }
        MessageId::SessionCompactSpawnFailedBody => "后台压缩进程启动失败：{error}",
        MessageId::SessionCompactFailedTranscriptBody => {
            "完整会话记录未改变；智能体对话已恢复可用。"
        }
        MessageId::SessionCompactQueueFullBody => {
            "已排队的智能体请求过多，本次请求未加入队列。请等待当前任务完成后重新发送。"
        }
        _ => return None,
    })
}
