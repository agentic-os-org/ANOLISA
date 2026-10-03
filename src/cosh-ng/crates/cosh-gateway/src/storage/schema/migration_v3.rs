use super::Migration;

pub(super) const MIGRATION: Migration = Migration {
    version: 3,
    checksum: "cosh-gateway-identity-schema-v3-20260813",
    sql: r#"
CREATE TABLE gateway_identity (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    installation_id TEXT NOT NULL UNIQUE
) STRICT;
"#,
};
