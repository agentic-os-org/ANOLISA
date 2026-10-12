//! Version 1 Policy table contracts, rendered by the shared `SQLite` schema helpers.
//!
//! Keep released declarations stable; append upgrades in `migration.rs`.

use asc_sqlite_kernel::{ColumnSpec, IndexSpec, TableSpec};

/// Authoritative state tables initialized together in one transaction.
pub(crate) const POLICY_TABLES: &[TableSpec] = &[
    TableSpec {
        name: "policies",
        strict: true,
        constraints: &[],
        columns: POLICIES_COLUMNS,
        indexes: &[],
        extra_columns: &[],
    },
    TableSpec {
        name: "scopes",
        strict: true,
        constraints: &["CHECK (phase = 'DELETING' OR discovery_stopped = 0)"],
        columns: SCOPES_COLUMNS,
        indexes: &[],
        extra_columns: &[],
    },
    TableSpec {
        name: "bindings",
        strict: true,
        constraints: &[
            "CHECK ((last_write_id IS NULL AND last_write_digest IS NULL AND last_write_result_json IS NULL) \
                OR (last_write_id IS NOT NULL AND last_write_digest IS NOT NULL \
                AND length(last_write_digest) = 32 AND last_write_result_json IS NOT NULL))",
            "UNIQUE (scope_id, policy_id, boot_id, pid_namespace, pid, start_time)",
        ],
        columns: BINDINGS_COLUMNS,
        indexes: &[IndexSpec {
            name: "bindings_by_scope",
            columns: &["scope_id", "binding_id"],
        }],
        extra_columns: &[],
    },
];

const POLICIES_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "policy_id",
        definition: "TEXT COLLATE BINARY PRIMARY KEY NOT NULL",
    },
    ColumnSpec {
        name: "last_allocated_revision",
        definition: "INTEGER NOT NULL CHECK (last_allocated_revision BETWEEN 1 AND 4294967295)",
    },
    ColumnSpec {
        name: "current_json",
        definition: "TEXT CHECK (current_json IS NULL OR json_valid(current_json))",
    },
];

const SCOPES_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "scope_id",
        definition: "TEXT COLLATE BINARY PRIMARY KEY NOT NULL",
    },
    ColumnSpec {
        name: "assignment_json",
        definition: "TEXT NOT NULL CHECK (json_valid(assignment_json))",
    },
    ColumnSpec {
        name: "phase",
        definition: "TEXT NOT NULL CHECK (phase IN ('ACTIVE', 'DELETING'))",
    },
    ColumnSpec {
        name: "pinned_process_json",
        definition: "TEXT CHECK (pinned_process_json IS NULL OR json_valid(pinned_process_json))",
    },
    ColumnSpec {
        name: "discovery_stopped",
        definition: "INTEGER NOT NULL DEFAULT 0 CHECK (discovery_stopped IN (0, 1))",
    },
];

const BINDINGS_COLUMNS: &[ColumnSpec] = &[
    ColumnSpec {
        name: "binding_id",
        definition: "TEXT COLLATE BINARY PRIMARY KEY NOT NULL",
    },
    ColumnSpec {
        name: "scope_id",
        definition: "TEXT COLLATE BINARY NOT NULL REFERENCES scopes(scope_id) ON DELETE RESTRICT",
    },
    ColumnSpec {
        name: "policy_id",
        definition: "TEXT COLLATE BINARY NOT NULL",
    },
    ColumnSpec {
        name: "boot_id",
        definition: "TEXT NOT NULL",
    },
    ColumnSpec {
        name: "pid_namespace",
        definition: "TEXT NOT NULL",
    },
    ColumnSpec {
        name: "pid",
        definition: "INTEGER NOT NULL CHECK (pid BETWEEN 1 AND 4294967295)",
    },
    ColumnSpec {
        name: "start_time",
        definition: "TEXT NOT NULL CHECK (length(start_time) BETWEEN 1 AND 20 \
            AND start_time NOT GLOB '*[^0-9]*' AND substr(start_time, 1, 1) BETWEEN '1' AND '9')",
    },
    ColumnSpec {
        name: "binding_revision",
        definition: "INTEGER NOT NULL CHECK (binding_revision BETWEEN 1 AND 4294967295)",
    },
    ColumnSpec {
        name: "spec_json",
        definition: "TEXT NOT NULL CHECK (json_valid(spec_json))",
    },
    ColumnSpec {
        name: "phase",
        definition: "TEXT NOT NULL CHECK (phase IN ('PENDING_APPLY', 'APPLYING', 'READY', 'APPLY_FAILED', \
            'PENDING_DELETE', 'DELETING', 'DELETE_FAILED'))",
    },
    ColumnSpec {
        name: "error_json",
        definition: "TEXT CHECK (error_json IS NULL OR json_valid(error_json))",
    },
    ColumnSpec {
        name: "status_version",
        definition: "INTEGER NOT NULL CHECK (status_version >= 1)",
    },
    ColumnSpec {
        name: "deployments_json",
        definition: "TEXT NOT NULL CHECK (json_valid(deployments_json))",
    },
    ColumnSpec {
        name: "last_write_id",
        definition: "TEXT",
    },
    ColumnSpec {
        name: "last_write_digest",
        definition: "BLOB",
    },
    ColumnSpec {
        name: "last_write_result_json",
        definition: "TEXT CHECK (last_write_result_json IS NULL OR json_valid(last_write_result_json))",
    },
];
