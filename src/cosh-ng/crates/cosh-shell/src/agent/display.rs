use crate::i18n::{I18n, MessageId};

fn trial_error_message(code: &str, zh: bool) -> Option<&'static str> {
    match code {
        "ModelNotConfigured" => Some(if zh {
            "尚未配置模型，可用 /auth 配置。"
        } else {
            "No model configured. Use /auth to configure a model."
        }),
        "Sysom.CreditsExhausted" => Some(if zh {
            "当前可用额度不足。问题已保留，请前往控制台查看；也可用 /auth 切换模型服务。\nhttps://console.aliyun.com/alinux"
        } else {
            "Available quota is insufficient. Your question is retained. Check OS Console or use /auth to switch model services.\nhttps://console.aliyun.com/alinux"
        }),
        "trial_cancelled" => Some(if zh {
            "已取消，问题已保留。"
        } else {
            "Cancelled. Your question is retained."
        }),
        "EcsIdentityUnavailable"
        | "metadata_access_denied"
        | "invalid_metadata_response"
        | "metadata_unreachable"
        | "metadata_timeout"
        | "metadata_http_error" => Some(if zh {
            "无法识别或访问 ECS 实例身份。问题已保留，可用 /auth 配置模型。"
        } else {
            "Unable to access the ECS identity. Your question is retained; use /auth to configure a model."
        }),
        "EcsTrialDisabled" => Some(if zh {
            "自动接入已关闭。问题已保留，可用 /auth 配置模型或重新启用。"
        } else {
            "Automatic OS Console access is disabled. Your question is retained; use /auth to configure a model or enable automatic access."
        }),
        "trial_wait_timeout" => Some(if zh {
            "等待开通已超时。问题已保留，可用 /auth 配置模型。"
        } else {
            "Activation wait timed out. Your question is retained; use /auth to configure a model."
        }),
        "Sysom.ProviderFailure" => Some(if zh {
            "模型服务暂时不可用。问题已保留，可重新提问或用 /auth 配置模型。"
        } else {
            "Model service unavailable. Your question is retained; ask again or use /auth to configure a model."
        }),
        code if code.starts_with("trial_preferences_") => Some(if zh {
            "无法读取免配置接入设置，本次未自动接入。可用 /auth 重试或配置模型。"
        } else {
            "Unable to read automatic access preferences. Use /auth to retry or configure a model."
        }),
        _ => None,
    }
}

pub(crate) fn display_agent_error(error: &str, error_code: Option<&str>, i18n: &I18n) -> String {
    let zh = i18n.language() == crate::Language::ZhCn;
    let trial_message = error_code
        .and_then(|code| trial_error_message(code, zh))
        .or_else(|| {
            trial_error_message(error.strip_prefix("provider_access:").unwrap_or(error), zh)
        });
    if let Some(message) = trial_message {
        return message.to_string();
    }

    if error == "cosh-core-question-protocol:answer-write-failed" {
        format!(
            "{}\n{}",
            i18n.t(MessageId::AgentAnswerDeliveryUnknownTitle),
            i18n.t(MessageId::AgentAnswerDeliveryUnknownBody)
        )
    } else if error.starts_with("cosh-core-question-protocol:") {
        format!(
            "{}\n{}",
            i18n.t(MessageId::AgentQuestionUnavailableTitle),
            i18n.t(MessageId::AgentQuestionUnavailableBody)
        )
    } else if error == "analysis returned an error" {
        i18n.t(MessageId::AgentStatusAnalysisReturnedError)
            .to_string()
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_question_protocol_errors_do_not_expose_reason_codes() {
        let i18n = I18n::new(crate::Language::EnUs);
        let display =
            display_agent_error("cosh-core-question-protocol:missing-question", None, &i18n);
        assert_eq!(
            display,
            "Agent question unavailable\nThe Agent returned an incomplete question. Please retry."
        );
        assert!(!display.contains("missing-question"));

        let delivery = display_agent_error(
            "cosh-core-question-protocol:answer-write-failed",
            None,
            &i18n,
        );
        assert!(delivery.contains("Agent answer delivery uncertain"));
        assert!(!delivery.contains("answer-write-failed"));
    }
}
