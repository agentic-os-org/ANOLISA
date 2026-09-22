//! `anolisa list` — list available components from the component index.
//!
//! Reads the repo-side `components-v2.toml` (the component identity index),
//! merges install status from `installed.toml`, and renders as a human
//! table or `--json` envelope.

mod render;
mod state_view;

#[cfg(test)]
mod tests;

#[cfg(test)]
use anolisa_core::state_store::StateStore;
use anolisa_platform::pkg_query::PackageQuery;
use anolisa_platform::rpm_query::RpmPackageQuery;
use clap::Parser;
use serde::Serialize;

use crate::commands::common;
use crate::commands::common::RepoPersistPolicy;
use crate::commands::state_view::{StateScope, StateView, StateVisibility};
use crate::context::{CliContext, InstallMode};
use crate::resolution::{
    ComponentIndex, ComponentIndexEntry, ComponentTarget, load_component_index,
};
use crate::response::{CliError, render_json};

use self::render::render_human;
use self::state_view::{LocalProjection, project_component};

const COMMAND: &str = "list";

#[derive(Parser)]
pub struct ListArgs {
    /// Show only currently installed components
    #[arg(long, alias = "enabled")]
    pub installed: bool,
}

// ── Wire / JSON output types ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub display_name: String,
    pub summary: String,
    pub backends: Vec<String>,
    /// OS/architecture targets declared by the component index.
    pub targets: Vec<ComponentTarget>,
    /// Whether the component declares support for the current host target.
    pub target_available: bool,
    pub status: String,
    pub local_state: String,
    pub ownership: String,
    pub scope: String,
    pub active: bool,
    pub mutable_by_current_invocation: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadowed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_path: Option<String>,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_evr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_arch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_source_repo: Option<String>,
}

#[derive(Serialize)]
struct ListPayload {
    components: Vec<Row>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

// ── Handler ────────────────────────────────────────────────────────

pub fn handle(args: ListArgs, ctx: &CliContext) -> Result<(), CliError> {
    let layout = common::resolve_layout(ctx);
    let env = anolisa_env::EnvService::detect();
    let repo_config =
        common::load_repo_config(ctx, &layout, COMMAND, RepoPersistPolicy::BestEffort)?;

    let index =
        load_component_index(&layout, &env, &repo_config).map_err(|err| CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!("failed to load component index: {err}"),
        })?;

    let view = StateView::load(ctx, COMMAND, StateVisibility::UserPlusSystem)?;
    let rpm_query = match ctx.install_mode {
        InstallMode::System => Some(RpmPackageQuery::system()),
        InstallMode::User => None,
    };
    let rows = build_rows_from_view(
        &index,
        &args,
        &view,
        rpm_query.as_ref().map(|query| query as &dyn PackageQuery),
        &env.os,
        &env.arch,
    );

    if ctx.json {
        return render_json(
            COMMAND,
            ListPayload {
                components: rows,
                warnings: view.warnings,
            },
        );
    }

    if !ctx.quiet {
        render_warnings(&view.warnings);
        render_human(&rows, ctx.no_color, &env.os, &env.arch);
    }
    Ok(())
}

#[cfg(test)]
fn build_rows(
    index: &ComponentIndex,
    args: &ListArgs,
    state: &StateStore,
    rpm_query: Option<&dyn PackageQuery>,
) -> Vec<Row> {
    build_rows_for_target(index, args, state, rpm_query, "linux", "x86_64")
}

#[cfg(test)]
fn build_rows_for_target(
    index: &ComponentIndex,
    args: &ListArgs,
    state: &StateStore,
    rpm_query: Option<&dyn PackageQuery>,
    os: &str,
    arch: &str,
) -> Vec<Row> {
    index
        .components
        .iter()
        .filter_map(|entry| {
            let projection = project_component(entry, index, state, rpm_query);
            if args.installed && !projection.local_state.matches_installed_filter() {
                return None;
            }
            Some(entry_to_row(
                entry,
                projection,
                os,
                arch,
                RowScope {
                    scope: "none".to_string(),
                    active: false,
                    mutable_by_current_invocation: false,
                    shadowed_by: None,
                    state_path: None,
                },
            ))
        })
        .collect()
}

fn build_rows_from_view(
    index: &ComponentIndex,
    args: &ListArgs,
    view: &StateView,
    rpm_query: Option<&dyn PackageQuery>,
    os: &str,
    arch: &str,
) -> Vec<Row> {
    let visible_components = view.visible_components();
    index
        .components
        .iter()
        .flat_map(|entry| {
            let matching_records = visible_components
                .iter()
                .filter(|record| record.object.name == entry.name)
                .collect::<Vec<_>>();
            if !matching_records.is_empty() {
                return matching_records
                    .into_iter()
                    .filter_map(|record| {
                        let projection =
                            project_component(entry, index, &record.root.state, rpm_query);
                        if args.installed && !projection.local_state.matches_installed_filter() {
                            return None;
                        }
                        let row_scope = RowScope {
                            scope: record.scope().label().to_string(),
                            active: record.active,
                            mutable_by_current_invocation: record.mutable_by_current_invocation,
                            shadowed_by: record
                                .shadowed_by
                                .map(StateScope::label)
                                .map(str::to_string),
                            state_path: Some(record.root.state_path.display().to_string()),
                        };
                        Some(entry_to_row(entry, projection, os, arch, row_scope))
                    })
                    .collect::<Vec<_>>();
            }

            let projection = project_component(entry, index, &view.writable.state, rpm_query);
            if args.installed && !projection.local_state.matches_installed_filter() {
                return Vec::new();
            }
            let scope = match projection.local_state {
                self::state_view::LocalState::Observed => view.writable.scope.label(),
                self::state_view::LocalState::Tracked
                | self::state_view::LocalState::Installed
                | self::state_view::LocalState::Drifted
                | self::state_view::LocalState::Missing
                | self::state_view::LocalState::Failed
                | self::state_view::LocalState::Degraded
                | self::state_view::LocalState::Disabled
                | self::state_view::LocalState::NotInstalled => "none",
            };
            vec![entry_to_row(
                entry,
                projection,
                os,
                arch,
                RowScope {
                    scope: scope.to_string(),
                    active: false,
                    mutable_by_current_invocation: false,
                    shadowed_by: None,
                    state_path: None,
                },
            )]
        })
        .collect()
}

struct RowScope {
    scope: String,
    active: bool,
    mutable_by_current_invocation: bool,
    shadowed_by: Option<String>,
    state_path: Option<String>,
}

fn entry_to_row(
    entry: &ComponentIndexEntry,
    projection: LocalProjection,
    os: &str,
    arch: &str,
    row_scope: RowScope,
) -> Row {
    let backends: Vec<String> = entry.backends.iter().map(|b| b.kind.clone()).collect();
    let local_state = projection.local_state.label().to_string();
    let ownership = projection.ownership_label().to_string();
    let target_available = entry.supports_target(os, arch);
    let install_available = target_available && !backends.is_empty();
    let action = if install_available || projection.action_label() != "install" {
        projection.action_label().to_string()
    } else {
        "unavailable".to_string()
    };
    Row {
        name: entry.name.clone(),
        display_name: entry
            .display_name
            .clone()
            .unwrap_or_else(|| entry.name.clone()),
        summary: entry.summary.clone().unwrap_or_default(),
        backends,
        targets: entry.targets.clone(),
        target_available,
        status: projection.status,
        local_state,
        ownership,
        scope: row_scope.scope,
        active: row_scope.active,
        mutable_by_current_invocation: row_scope.mutable_by_current_invocation,
        shadowed_by: row_scope.shadowed_by,
        state_path: row_scope.state_path,
        action,
        rpm_package: projection.rpm_package,
        rpm_evr: projection.rpm_evr,
        rpm_arch: projection.rpm_arch,
        rpm_source_repo: projection.rpm_source_repo,
    }
}

fn render_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}
