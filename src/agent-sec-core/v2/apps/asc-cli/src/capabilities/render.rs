//! Table rendering for the capability view.
//!
//! The layout is byte-compatible with the V1 renderer, including the trailing
//! padding of the last column, so operators and scripted diffs cannot tell the
//! two CLIs apart.

use super::CapabilityRecord;

const COLUMNS: [&str; 6] = [
    "CAPABILITY",
    "ENABLED",
    "MODE",
    "SCAN_MODE",
    "TIMEOUT(s)",
    "DIAGNOSTICS",
];

/// Renders one `[agent]` block per agent, in record order.
pub(crate) fn table(records: &[CapabilityRecord]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for agent in ordered_agents(records) {
        let rows: Vec<[String; 6]> = records
            .iter()
            .filter(|record| record.agent() == agent)
            .map(row)
            .collect();
        let mut widths = COLUMNS.map(str::chars).map(Iterator::count);
        for row in &rows {
            for (width, value) in widths.iter_mut().zip(row) {
                *width = (*width).max(value.chars().count());
            }
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(format!("[{agent}]"));
        lines.push(format_row(&COLUMNS.map(String::from), &widths));
        lines.push(format_row(&widths.map(|width| "-".repeat(width)), &widths));
        lines.extend(rows.iter().map(|row| format_row(row, &widths)));
    }
    lines.join("\n")
}

fn row(record: &CapabilityRecord) -> [String; 6] {
    [
        record.capability().to_owned(),
        record.enabled_label().to_owned(),
        record.mode().to_owned(),
        record.scan_mode().to_owned(),
        record.timeout().to_owned(),
        if record.diagnostics().is_empty() {
            "-".to_owned()
        } else {
            record.diagnostics().join(";")
        },
    ]
}

fn ordered_agents(records: &[CapabilityRecord]) -> Vec<&'static str> {
    let mut ordered: Vec<&'static str> = Vec::new();
    for record in records {
        if !ordered.contains(&record.agent()) {
            ordered.push(record.agent());
        }
    }
    ordered
}

fn format_row(row: &[String; 6], widths: &[usize; 6]) -> String {
    row.iter()
        .zip(widths)
        .map(|(value, width)| {
            let padding = width.saturating_sub(value.chars().count());
            format!("{value}{}", " ".repeat(padding))
        })
        .collect::<Vec<_>>()
        .join("  ")
}

#[cfg(test)]
mod tests {
    use crate::capabilities::{Environment, query, render_table};

    #[test]
    fn every_agent_gets_one_block_with_the_full_column_set() {
        let rendered = render_table(&query(&Environment::new(), None, None).expect("no filters"));
        assert_eq!(rendered.matches("CAPABILITY").count(), 6);
        for agent in ["qoder", "qwen", "codex", "cosh", "openclaw", "hermes"] {
            assert_eq!(rendered.matches(&format!("[{agent}]")).count(), 1);
        }
        for capability in [
            "code-scan",
            "prompt-scan",
            "pii-check",
            "skill-ledger",
            "observability",
        ] {
            assert_eq!(rendered.matches(capability).count(), 6, "{capability}");
        }
        assert!(!rendered.contains("HOOKS"));
        assert!(!rendered.contains("SOURCE"));
    }

    #[test]
    fn columns_keep_the_v1_order_and_the_agent_blocks_are_separated() {
        let rendered = render_table(
            &query(&Environment::new(), Some("cosh"), Some("prompt-scan")).expect("filters"),
        );
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "[cosh]");
        assert_eq!(
            lines[1].split_whitespace().collect::<Vec<_>>(),
            vec![
                "CAPABILITY",
                "ENABLED",
                "MODE",
                "SCAN_MODE",
                "TIMEOUT(s)",
                "DIAGNOSTICS"
            ]
        );
        assert!(lines[2].starts_with("-----------"));
        assert_eq!(
            lines[3].split_whitespace().collect::<Vec<_>>(),
            vec!["prompt-scan", "enabled", "ask", "standard", "10", "-"]
        );
    }

    #[test]
    fn diagnostics_are_joined_with_semicolons_in_one_cell() {
        let mut env = Environment::new();
        env.insert("CODE_SCANNER_HOOK_ENABLED".to_owned(), "maybe".to_owned());
        env.insert("CODE_SCANNER_MODE".to_owned(), "warn".to_owned());
        let rendered =
            render_table(&query(&env, Some("codex"), Some("code-scan")).expect("filters"));
        let row = rendered.lines().last().expect("one data row");
        assert!(row.contains("CODE_SCANNER_HOOK_ENABLED has an invalid value; using True;CODE_SCANNER_MODE has an invalid value; using 'observe'"), "{row}");
        assert!(row.contains("enabled"), "{row}");
    }
}
