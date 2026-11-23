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
            "自动接入已关闭，可用 /auth 重新启用免配置接入或配置模型。"
        } else {
            "Automatic model access is disabled. Use /auth to enable automatic access or configure a model."
        }),
        "Sysom.Identity.PermissionDenied" | "Forbidden" | "AccessDenied" => Some(if zh {
            "当前账号或实例无权访问模型服务，请联系管理员检查权限，或用 /auth 切换模型服务。"
        } else {
            "This account or instance cannot access the model service. Ask an administrator to check permissions, or use /auth to switch model services."
        }),
        "Sysom.Identity.SysomNotActivated" => Some(if zh {
            "需要开通或恢复当前 ECS 所属账号的操作系统控制台授权，也可用 /auth 配置其他模型。\nhttps://console.aliyun.com/alinux/welcome?source=cosh"
        } else {
            "Activate or restore OS Console authorization with the account owning this ECS, or use /auth to configure another model.\nhttps://console.aliyun.com/alinux/welcome?source=cosh"
        }),
        "Sysom.Identity.InternalError"
        | "Sysom.Identity.ProviderNotReady"
        | "Sysom.Identity.StsUnavailable" => Some(if zh {
            "ECS 身份服务暂时不可用，请稍后重试，或用 /auth 配置其他模型。"
        } else {
            "ECS identity service is temporarily unavailable. Retry later or use /auth to configure another model."
        }),
        "Sysom.Identity.InvalidResponse" | "Sysom.Identity.StsInvalidResponse" => Some(if zh {
            "ECS 身份服务返回了无效响应，请联系管理员排查，或用 /auth 配置其他模型。"
        } else {
            "ECS identity service returned an invalid response. Contact an administrator or use /auth to configure another model."
        }),
        "Sysom.Identity.InvalidInstanceProof"
        | "Sysom.Identity.InvalidParameter"
        | "Sysom.Identity.UntrustedEndpoint" => Some(if zh {
            "ECS 身份验证未通过，请联系管理员检查实例证明或接入设置，也可用 /auth 配置其他模型。"
        } else {
            "ECS identity verification failed. Ask an administrator to check the instance proof or access settings, or use /auth to configure another model."
        }),
        "Sysom.Identity.MetadataUnavailable" => Some(if zh {
            "无法访问 ECS 实例元数据，请检查实例元数据访问设置，或用 /auth 配置其他模型。"
        } else {
            "Unable to access ECS instance metadata. Check metadata access settings or use /auth to configure another model."
        }),
        "Sysom.Identity.Throttled" => Some(if zh {
            "ECS 身份请求被限流，请稍后重试，或用 /auth 配置其他模型。"
        } else {
            "ECS identity request was throttled. Retry later or use /auth to configure another model."
        }),
        "Sysom.Identity.Unavailable"
        | "Sysom.Identity.Timeout"
        | "Sysom.Identity.LeaseExpired"
        | "Sysom.Identity.ChallengeNotRedeemable" => Some(if zh {
            "本次 ECS 身份连接或凭据兑换未完成，请重新提问，或用 /auth 配置其他模型。"
        } else {
            "ECS identity connection or credential exchange did not complete. Ask again or use /auth to configure another model."
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
