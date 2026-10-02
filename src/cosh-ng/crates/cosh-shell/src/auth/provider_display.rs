use crate::config::Language;
use crate::runtime::prelude::AuthProviderInfo;
use crate::ui::OPTION_DETAIL_SEPARATOR;

pub(crate) fn auth_providers_for_display(providers: &[AuthProviderInfo]) -> Vec<AuthProviderInfo> {
    let mut providers = providers.to_vec();
    providers.sort_by_key(|provider| if provider.id == "aliyun" { 0 } else { 1 });
    providers
}

pub(crate) fn provider_option(provider: &AuthProviderInfo, language: Language) -> String {
    let description = match language {
        Language::EnUs => provider.description.as_deref(),
        Language::ZhCn => provider
            .description_zh_cn
            .as_deref()
            .or(provider.description.as_deref()),
    };
    match description {
        Some(description) => {
            format!("{}{OPTION_DETAIL_SEPARATOR}{description}", provider.label)
        }
        None => provider.label.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, description: Option<&str>) -> AuthProviderInfo {
        AuthProviderInfo {
            id: id.to_string(),
            label: id.to_string(),
            description: description.map(str::to_string),
            description_zh_cn: None,
            builtin_base_url: None,
            fields: Vec::new(),
        }
    }

    #[test]
    fn normalizes_legacy_provider_order() {
        let providers = vec![
            provider("dashscope", None),
            provider("openai_compat", None),
            provider("aliyun", None),
        ];

        let ids = auth_providers_for_display(&providers)
            .into_iter()
            .map(|provider| provider.id)
            .collect::<Vec<_>>();

        assert_eq!(ids, ["aliyun", "dashscope", "openai_compat"]);
    }

    #[test]
    fn renders_description_on_an_indented_line() {
        assert_eq!(
            provider_option(
                &provider(
                    "Coding Plan",
                    Some("For individual developers • Weekly quota included")
                ),
                Language::EnUs
            ),
            "Coding Plan\n    For individual developers • Weekly quota included"
        );
    }

    #[test]
    fn prefers_chinese_description_for_zh_cn() {
        let mut provider = provider(
            "Token Plan",
            Some("For teams and companies • Usage-based billing with dedicated capacity"),
        );
        provider.description_zh_cn = Some("面向团队和企业 • 按用量计费，提供专属容量".to_string());

        assert_eq!(
            provider_option(&provider, Language::ZhCn),
            "Token Plan\n    面向团队和企业 • 按用量计费，提供专属容量"
        );
    }
}
