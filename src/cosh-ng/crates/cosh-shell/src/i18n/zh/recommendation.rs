use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::RecommendationTitle => "推荐",
        MessageId::RecommendationNextStepTitle => "建议下一步",
        MessageId::AnalysisResultTitle => "分析结果",
        MessageId::RecommendationEmptyBody => "没有命令推荐",
        MessageId::RecommendationFooter => "仅展示：未执行任何命令",
        MessageId::RecommendationNoSelectableTitle => "没有可选择的推荐",
        MessageId::RecommendationNoSelectableBody => "当前还没有可选择的推荐",
        MessageId::RecommendationUnavailableTitle => "推荐不可用",
        MessageId::RecommendationUnavailableBody => "推荐 {index} 不可用；请选择 1..{total}",
        MessageId::RecommendationSelectedTitle => "已选择推荐",
        MessageId::RecommendationSelectedBody => "已选择推荐 {index}",
        MessageId::RecommendationCopiedTitle => "复制推荐",
        MessageId::RecommendationCopiedBody => "复制推荐 {index}",
        MessageId::RecommendationInsertTitle => "插入推荐",
        MessageId::RecommendationInsertBody => "已准备推荐 {index}，等待手动输入",
        MessageId::RecommendationDetailsTitle => "推荐详情",
        MessageId::RecommendationDetailsBody => "推荐 {index} 的详情",
        MessageId::RecommendationDisplayOnlyBody => "仅展示：命令未执行；复制或重新输入后才会运行",
        MessageId::RecommendationCopyOnlyBody => "仅复制：命令只展示给你复制，没有执行。",
        MessageId::RecommendationInsertOnlyBody => {
            "Insert 只会成为待编辑输入；没有提交，也没有写入子 shell。"
        }
        MessageId::RecommendationDetailsOnlyBody => "仅查看详情：决定输入或复制前先检查命令。",
        _ => return None,
    })
}
