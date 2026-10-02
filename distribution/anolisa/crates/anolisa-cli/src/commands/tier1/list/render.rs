use crate::color::{Palette, pad_right};
use crate::commands::tier1::list::Row;

pub(super) fn human_header(rows: &[Row]) -> String {
    let widths = HumanWidths::for_rows(rows);
    format!(
        "{:<name_width$}{:<availability_width$}{:<scope_width$}{:<local_state_width$}{}",
        "NAME",
        "AVAILABILITY",
        "SCOPE",
        "LOCAL STATE",
        "ACTION",
        name_width = widths.name,
        availability_width = widths.availability,
        scope_width = widths.scope,
        local_state_width = widths.local_state,
    )
}

struct HumanWidths {
    name: usize,
    availability: usize,
    scope: usize,
    local_state: usize,
}

impl HumanWidths {
    fn for_rows(rows: &[Row]) -> Self {
        Self {
            name: rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4) + 4,
            availability: rows
                .iter()
                .map(|r| availability_label(r).len())
                .max()
                .unwrap_or(12)
                .max(12)
                + 4,
            scope: rows.iter().map(|r| r.scope.len()).max().unwrap_or(5).max(5) + 4,
            local_state: rows
                .iter()
                .map(|r| r.local_state.len())
                .max()
                .unwrap_or(11)
                .max(11)
                + 4,
        }
    }
}

pub(super) fn render_human(rows: &[Row], no_color: bool, os: &str, arch: &str) {
    let color = Palette::new(no_color);
    println!(
        "{}",
        color.header(format!("Components for {}/{arch}", display_os(os)))
    );
    println!();
    if rows.is_empty() {
        println!("{}", color.muted("no components found"));
        return;
    }

    let widths = HumanWidths::for_rows(rows);

    println!("{}", color.header(human_header(rows)));
    for row in rows {
        let availability = availability_label(row);
        let availability = if row.target_available {
            color.ok(availability)
        } else {
            color.err(availability)
        };
        let action = if row.action == "unavailable" {
            "-"
        } else {
            &row.action
        };
        println!(
            "{:<name_width$}{}{:<scope_width$}{}{}",
            row.name,
            pad_right(availability, widths.availability),
            row.scope,
            pad_right(color.status(&row.local_state), widths.local_state),
            action,
            name_width = widths.name,
            scope_width = widths.scope,
        );
    }
}

pub(super) fn availability_label(row: &Row) -> String {
    if row.target_available {
        return "available".to_string();
    }
    let supported = row
        .targets
        .iter()
        .map(|target| format!("{}/{}", display_os(&target.os), target.arch))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{supported} only")
}

fn display_os(os: &str) -> String {
    match os {
        "linux" => "Linux".to_string(),
        "macos" => "macOS".to_string(),
        other => other.to_string(),
    }
}
