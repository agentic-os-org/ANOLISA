//! Top-level `/auth` management menu: row model, ordering and SysOM placement.
//!
//! The management panel mixes three kinds of rows — the SysOM free-trial shortcut (offered
//! only on an ECS instance), the saved providers, and `+ Add new provider`. Rendering, input
//! capture and answer dispatch all derive their indices from [`management_entries`] so that
//! no call site has to offset by hand when the SysOM row appears.

use std::collections::HashMap;

use super::provider_management::ExistingProvider;

/// Label of the SysOM free-trial row, reused for a saved RAM-role provider promoted to the
/// first slot so both spellings of "already on ECS" read the same.
const SYSOM_ENTRY_LABEL: &str = "SysOM (free trial, uses this ECS instance's RAM role)";

const ADD_NEW_PROVIDER_LABEL: &str = "  + Add new provider";

// SysOM is not a provider type of its own: it is the `aliyun` provider authenticated through
// the instance RAM role instead of a manual AK/SK pair.
const ECS_RAM_ROLE_PROVIDER_TYPE: &str = "aliyun";
pub(super) const ECS_RAM_ROLE_AUTH_SOURCE: &str = "ecs_ram_role";

/// ECS RAM-role challenge prefetched by `/auth` from the core `auth.prepare` action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EcsRamRolePrepare {
    pub(super) instance_id: String,
    pub(super) console_url: String,
    /// Credential values the core wants persisted, e.g. `auth_source=ecs_ram_role`.
    pub(super) values: HashMap<String, String>,
}

/// Successful Aliyun prepare result cached for the lifetime of the `/auth` flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PrefetchedAliyunPrepare {
    Manual,
    EcsRamRole(EcsRamRolePrepare),
}

/// SysOM placement in the top-level menu plus the challenge the shortcut reuses.
///
/// `Default` is the non-ECS shape: no shortcut row, no promotion, and no prefetched
/// challenge, which keeps every menu index identical to the pre-SysOM behaviour.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SysomMenu {
    /// Successful `auth.prepare`, retained so manual Aliyun setup does not probe again.
    prepare: Option<PrefetchedAliyunPrepare>,
    /// A saved RAM-role provider holds the first slot, so no shortcut row is added.
    promoted: bool,
}

impl SysomMenu {
    /// Menu for a host whose `auth.prepare` reported `ecs_ram_role`.
    pub(super) fn on_ecs(prepare: EcsRamRolePrepare) -> Self {
        Self {
            prepare: Some(PrefetchedAliyunPrepare::EcsRamRole(prepare)),
            promoted: false,
        }
    }

    /// Menu for a host whose `auth.prepare` selected manual Aliyun credentials.
    pub(super) fn on_manual() -> Self {
        Self {
            prepare: Some(PrefetchedAliyunPrepare::Manual),
            promoted: false,
        }
    }

    /// The prefetched result, so Aliyun preparation runs once per `/auth`.
    pub(super) fn prefetched(&self) -> Option<&PrefetchedAliyunPrepare> {
        self.prepare.as_ref()
    }

    /// Whether the first saved provider is an already-configured SysOM setup.
    pub(super) fn promoted(&self) -> bool {
        self.promoted
    }

    fn shows_shortcut(&self) -> bool {
        matches!(
            self.prepare.as_ref(),
            Some(PrefetchedAliyunPrepare::EcsRamRole(_))
        ) && !self.promoted
    }

    /// Moves a saved `aliyun` + `ecs_ram_role` provider to the first slot instead of
    /// offering a shortcut that would configure a second copy of it.
    ///
    /// Idempotent, and cheap enough to re-run whenever the saved provider list is
    /// reloaded (for example after a delete) so the promoted slot never goes stale.
    pub(super) fn sync(&mut self, providers: &mut Vec<ExistingProvider>) {
        self.promoted = false;
        if !matches!(
            self.prepare.as_ref(),
            Some(PrefetchedAliyunPrepare::EcsRamRole(_))
        ) {
            return;
        }
        let Some(index) = providers.iter().position(is_ecs_ram_role_provider) else {
            return;
        };
        let provider = providers.remove(index);
        providers.insert(0, provider);
        self.promoted = true;
    }
}

fn is_ecs_ram_role_provider(provider: &ExistingProvider) -> bool {
    provider.provider_type == ECS_RAM_ROLE_PROVIDER_TYPE
        && provider.auth_source.as_deref() == Some(ECS_RAM_ROLE_AUTH_SOURCE)
}

/// A selectable row of the top-level `/auth` management panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuthManagementEntry {
    /// Configure SysOM through this instance's RAM role (ECS hosts without one saved).
    SysomShortcut,
    /// Act on `existing_providers[index]`.
    Existing(usize),
    /// Start the template picker for a brand-new provider.
    AddNew,
}

/// Rows of the management panel, in display order.
pub(super) fn management_entries(
    sysom: &SysomMenu,
    existing_count: usize,
) -> Vec<AuthManagementEntry> {
    let mut entries = Vec::with_capacity(existing_count + 2);
    if sysom.shows_shortcut() {
        entries.push(AuthManagementEntry::SysomShortcut);
    }
    entries.extend((0..existing_count).map(AuthManagementEntry::Existing));
    entries.push(AuthManagementEntry::AddNew);
    entries
}

pub(super) fn management_entry_count(sysom: &SysomMenu, existing_count: usize) -> usize {
    management_entries(sysom, existing_count).len()
}

/// The row at `selected`, falling back to `AddNew` for an out-of-range selection.
pub(super) fn management_entry(
    sysom: &SysomMenu,
    existing_count: usize,
    selected: usize,
) -> AuthManagementEntry {
    management_entries(sysom, existing_count)
        .get(selected)
        .copied()
        .unwrap_or(AuthManagementEntry::AddNew)
}

/// Selection index of `entry`, used when returning to the panel from a nested phase.
pub(super) fn management_entry_index(
    sysom: &SysomMenu,
    existing_count: usize,
    entry: AuthManagementEntry,
) -> usize {
    management_entries(sysom, existing_count)
        .iter()
        .position(|candidate| *candidate == entry)
        .unwrap_or(0)
}

/// Whether the panel has anything to manage besides `+ Add new provider`.
///
/// `/auth` skips straight to the template picker when it does not, which is what a
/// non-ECS host with no saved provider still gets.
pub(super) fn has_manageable_entries(sysom: &SysomMenu, existing_count: usize) -> bool {
    management_entry_count(sysom, existing_count) > 1
}

/// Rendered option strings for the management panel, aligned with [`management_entries`].
pub(super) fn management_options(sysom: &SysomMenu, providers: &[ExistingProvider]) -> Vec<String> {
    management_entries(sysom, providers.len())
        .into_iter()
        .map(|entry| match entry {
            AuthManagementEntry::SysomShortcut => format!("  {SYSOM_ENTRY_LABEL}"),
            AuthManagementEntry::Existing(index) => {
                existing_provider_option(sysom, providers, index)
            }
            AuthManagementEntry::AddNew => ADD_NEW_PROVIDER_LABEL.to_string(),
        })
        .collect()
}

/// Display label of a saved provider; the promoted RAM-role entry reads as SysOM.
pub(super) fn existing_provider_label(
    sysom: &SysomMenu,
    providers: &[ExistingProvider],
    index: usize,
) -> String {
    if sysom.promoted() && index == 0 {
        return SYSOM_ENTRY_LABEL.to_string();
    }
    providers
        .get(index)
        .map(|provider| provider.label.clone())
        .unwrap_or_default()
}

fn existing_provider_option(
    sysom: &SysomMenu,
    providers: &[ExistingProvider],
    index: usize,
) -> String {
    let Some(provider) = providers.get(index) else {
        return String::new();
    };
    let active_mark = if provider.is_active {
        "* [active] "
    } else {
        "  "
    };
    let model_info = if provider.model.is_empty() {
        String::new()
    } else {
        format!(" - {}", provider.model)
    };
    let source_info = if provider.source == "system" {
        " [system]"
    } else {
        ""
    };
    format!(
        "{}{} - \"{}\"{}{}",
        active_mark,
        existing_provider_label(sysom, providers, index),
        provider.name,
        model_info,
        source_info
    )
}

#[cfg(test)]
mod tests {
    use super::{
        existing_provider_label, has_manageable_entries, management_entries, management_entry,
        management_entry_count, management_entry_index, management_options, AuthManagementEntry,
        EcsRamRolePrepare, SysomMenu, SYSOM_ENTRY_LABEL,
    };
    use crate::auth::provider_management::ExistingProvider;
    use std::collections::HashMap;

    fn saved(name: &str, provider_type: &str, auth_source: Option<&str>) -> ExistingProvider {
        ExistingProvider {
            name: name.to_string(),
            provider_type: provider_type.to_string(),
            label: match provider_type {
                "dashscope" => "DashScope (\u{767e}\u{70bc})".to_string(),
                "aliyun" => "Aliyun Authentication".to_string(),
                _ => "OpenAI Compatible".to_string(),
            },
            model: "qwen3.7-plus".to_string(),
            is_active: true,
            editable: true,
            source: "user".to_string(),
            base_url: None,
            api_key_mask: None,
            access_key_id_mask: None,
            access_key_secret_mask: None,
            security_token_mask: None,
            auth_source: auth_source.map(str::to_string),
        }
    }

    fn on_ecs() -> SysomMenu {
        SysomMenu::on_ecs(EcsRamRolePrepare {
            instance_id: "i-test".to_string(),
            console_url: "https://example.invalid/guide".to_string(),
            values: HashMap::from([("auth_source".to_string(), "ecs_ram_role".to_string())]),
        })
    }

    #[test]
    fn ecs_without_saved_providers_offers_sysom_first() {
        let sysom = on_ecs();

        assert!(has_manageable_entries(&sysom, 0));
        assert_eq!(
            management_entries(&sysom, 0),
            vec![
                AuthManagementEntry::SysomShortcut,
                AuthManagementEntry::AddNew
            ]
        );
        let options = management_options(&sysom, &[]);
        assert!(options[0].contains(SYSOM_ENTRY_LABEL), "{options:?}");
        assert!(options[1].contains("+ Add new provider"), "{options:?}");
    }

    #[test]
    fn ecs_with_saved_provider_shifts_indices_by_the_sysom_row() {
        let sysom = on_ecs();
        let providers = vec![saved("qwen-prod", "dashscope", None)];

        assert_eq!(
            management_entries(&sysom, providers.len()),
            vec![
                AuthManagementEntry::SysomShortcut,
                AuthManagementEntry::Existing(0),
                AuthManagementEntry::AddNew,
            ]
        );
        assert_eq!(
            management_entry(&sysom, providers.len(), 0),
            AuthManagementEntry::SysomShortcut
        );
        assert_eq!(
            management_entry(&sysom, providers.len(), 1),
            AuthManagementEntry::Existing(0)
        );
        assert_eq!(
            management_entry(&sysom, providers.len(), 2),
            AuthManagementEntry::AddNew
        );
        // Returning from the action menu must land back on the same saved provider.
        assert_eq!(
            management_entry_index(&sysom, providers.len(), AuthManagementEntry::Existing(0)),
            1
        );
        let options = management_options(&sysom, &providers);
        assert!(options[1].contains("qwen-prod"), "{options:?}");
    }

    #[test]
    fn saved_ecs_ram_role_provider_is_promoted_instead_of_duplicated() {
        let mut sysom = on_ecs();
        let mut providers = vec![
            saved("qwen-prod", "dashscope", None),
            saved("sysom-trial", "aliyun", Some("ecs_ram_role")),
        ];

        sysom.sync(&mut providers);

        assert!(sysom.promoted());
        assert_eq!(providers[0].name, "sysom-trial");
        assert_eq!(
            management_entries(&sysom, providers.len()),
            vec![
                AuthManagementEntry::Existing(0),
                AuthManagementEntry::Existing(1),
                AuthManagementEntry::AddNew,
            ]
        );
        let options = management_options(&sysom, &providers);
        assert!(options[0].contains(SYSOM_ENTRY_LABEL), "{options:?}");
        assert!(options[0].contains("sysom-trial"), "{options:?}");
        assert!(
            !options
                .iter()
                .skip(1)
                .any(|o| o.contains(SYSOM_ENTRY_LABEL)),
            "{options:?}"
        );
        assert_eq!(
            existing_provider_label(&sysom, &providers, 0),
            SYSOM_ENTRY_LABEL
        );
    }

    #[test]
    fn non_ecs_menu_keeps_saved_providers_then_add_new() {
        let mut sysom = SysomMenu::default();
        let mut providers = vec![saved("qwen-prod", "dashscope", None)];

        sysom.sync(&mut providers);

        assert!(!sysom.promoted());
        assert_eq!(
            management_entries(&sysom, providers.len()),
            vec![
                AuthManagementEntry::Existing(0),
                AuthManagementEntry::AddNew
            ]
        );
        assert_eq!(management_entry_count(&sysom, providers.len()), 2);
        let options = management_options(&sysom, &providers);
        assert!(!options.iter().any(|o| o.contains("SysOM")), "{options:?}");
        assert_eq!(
            options[0],
            "* [active] DashScope (\u{767e}\u{70bc}) - \"qwen-prod\" - qwen3.7-plus"
        );
    }

    #[test]
    fn non_ecs_leaves_a_saved_ram_role_provider_where_it_was() {
        let mut sysom = SysomMenu::default();
        let mut providers = vec![
            saved("qwen-prod", "dashscope", None),
            saved("sysom-trial", "aliyun", Some("ecs_ram_role")),
        ];

        sysom.sync(&mut providers);

        assert!(!sysom.promoted());
        assert_eq!(providers[0].name, "qwen-prod");
        let options = management_options(&sysom, &providers);
        assert!(!options.iter().any(|o| o.contains("SysOM")), "{options:?}");
    }

    #[test]
    fn non_ecs_without_saved_providers_has_nothing_to_manage() {
        assert!(!has_manageable_entries(&SysomMenu::default(), 0));
    }
}
