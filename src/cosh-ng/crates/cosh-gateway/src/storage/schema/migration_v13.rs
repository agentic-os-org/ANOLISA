use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 13,
    checksum: "cosh-gateway-pre-runtime-binding-v13-20260826",
    sql: r#"
ALTER TABLE pre_runtime_baselines ADD COLUMN binding_json TEXT;
"#,
};
