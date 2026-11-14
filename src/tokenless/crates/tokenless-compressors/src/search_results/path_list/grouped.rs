//! Consecutive directory sharing with an explicit byte reconstruction contract.

/// Identifies the directory-grouped reconstruction contract.
pub(super) const HEADER: &str =
    "[Paths grouped by directory; all received paths retained; full path = base + Dir + entry]\n";

/// Shares consecutive parent directories without normalizing or reordering paths.
pub(super) fn encode(
    base: &str,
    paths: &[&str],
    line_ending: &str,
    trailing_newline: bool,
) -> String {
    let metadata = serde_json::json!({
        "base": base,
        "line_ending": line_ending,
        "trailing_newline": trailing_newline,
    });
    let mut output = format!("{HEADER}{metadata}\n");
    let mut previous = None;
    for path in paths {
        let (directory, entry) = path
            .rfind('/')
            .map_or(("", *path), |index| path.split_at(index + 1));
        if previous != Some(directory) {
            output.push_str("Dir=");
            output.push_str(&serde_json::Value::String(directory.to_owned()).to_string());
            output.push('\n');
            previous = Some(directory);
        }
        // A filename must never become a directory instruction or a quoted
        // entry accidentally. Empty basenames represent paths ending in '/'.
        if entry.is_empty()
            || entry.starts_with('"')
            || entry.starts_with("Dir=")
            || entry.starts_with("Base=")
        {
            output.push_str(&serde_json::Value::String(entry.to_owned()).to_string());
        } else {
            output.push_str(entry);
        }
        output.push('\n');
    }
    output
}

/// Reconstructs a generated candidate before it may replace the tool output.
pub(super) fn restore(view: &str) -> Option<String> {
    let (metadata, records) = view.strip_prefix(HEADER)?.split_once('\n')?;
    let data: serde_json::Value = serde_json::from_str(metadata).ok()?;
    let base = data["base"].as_str()?;
    let ending = data["line_ending"].as_str()?;
    if !matches!(ending, "\n" | "\r\n") {
        return None;
    }
    let trailing = data["trailing_newline"].as_bool()?;
    let mut directory: Option<String> = None;
    let mut output = String::new();
    let mut count = 0;
    for record in records.lines() {
        if let Some(quoted) = record.strip_prefix("Dir=") {
            let next: String = serde_json::from_str(quoted).ok()?;
            if !next.is_empty() && !next.ends_with('/') {
                return None;
            }
            directory = Some(next);
            continue;
        }
        let decoded;
        let entry = if record.starts_with('"') {
            decoded = serde_json::from_str::<String>(record).ok()?;
            decoded.as_str()
        } else {
            if record.is_empty() {
                return None;
            }
            record
        };
        if count > 0 {
            output.push_str(ending);
        }
        output.push_str(base);
        output.push_str(directory.as_deref()?);
        output.push_str(entry);
        count += 1;
    }
    if count == 0 {
        return None;
    }
    if trailing {
        output.push_str(ending);
    }
    Some(output)
}
