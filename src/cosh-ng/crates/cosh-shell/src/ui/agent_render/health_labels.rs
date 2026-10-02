//! Localized labels for [`HealthCollector`] values used by the health banner.
//!
//! Extracted from the large `agent_render/health.rs` renderer so that the
//! on-demand doctor collectors (provider/config/hooks/PTY/permissions) can add
//! their labels here without growing that file further.

use crate::diagnostics::health::HealthCollector;

pub(super) fn collector_label(collector: HealthCollector, i18n: crate::I18n) -> &'static str {
    match collector {
        HealthCollector::Host => i18n.t(crate::MessageId::HealthMetricHost),
        HealthCollector::Cpu => i18n.t(crate::MessageId::HealthMetricCpu),
        HealthCollector::Memory => i18n.t(crate::MessageId::HealthMetricMemory),
        HealthCollector::Disk => i18n.t(crate::MessageId::HealthMetricDisk),
        HealthCollector::KernelSignal => i18n.t(crate::MessageId::HealthMetricSignal),
        HealthCollector::ConfiguredService => i18n.t(crate::MessageId::HealthMetricService),
        HealthCollector::Provider => i18n.t(crate::MessageId::HealthCollectorProvider),
        HealthCollector::Config => i18n.t(crate::MessageId::HealthCollectorConfig),
        HealthCollector::Hooks => i18n.t(crate::MessageId::HealthCollectorHooks),
        HealthCollector::Pty => i18n.t(crate::MessageId::HealthCollectorPty),
        HealthCollector::Permissions => i18n.t(crate::MessageId::HealthCollectorPermissions),
    }
}
