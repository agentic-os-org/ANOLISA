use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 10,
    checksum: "cosh-gateway-provider-binding-v10-20260823-recoverable-execution",
    sql: r#"
ALTER TABLE brokered_requests ADD COLUMN provider_binding TEXT;
"#,
};
