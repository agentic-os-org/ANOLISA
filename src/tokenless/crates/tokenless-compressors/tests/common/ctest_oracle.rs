const HEADER: &str =
    "CTest templates (kind<TAB>fields; SP/SK/SD=start+result, P/K/D=result, s/S=start, B=blank): ";

// Deliberately independent of the producer: no CTest recognizers, edit plans,
// or formatting helpers are shared with this decoder.
/// Reconstruct every compacted CTest line without using the producer's parsers.
pub fn decode(input: &str) -> String {
    let mut lines = input.split_inclusive('\n');
    let mut output = String::new();
    let mut fence: Option<(char, usize)> = None;
    while let Some(line) = lines.next() {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = body.trim_start();
        let marker = ['`', '~'].into_iter().find_map(|ch| {
            let count = trimmed.chars().take_while(|&c| c == ch).count();
            (count >= 3).then_some((ch, count, &trimmed[count..]))
        });
        if let Some((open_ch, open_len)) = fence {
            if let Some((ch, count, rest)) = marker
                && ch == open_ch
                && count >= open_len
                && rest.trim().is_empty()
            {
                fence = None;
            }
            output.push_str(line);
            continue;
        }
        if let Some((ch, count, _)) = marker {
            fence = Some((ch, count));
            output.push_str(line);
            continue;
        }
        let Some(header_json) = line.strip_prefix(HEADER) else {
            output.push_str(line);
            continue;
        };
        let header: serde_json::Value = serde_json::from_str(header_json).unwrap();
        let row_count = header["rows"].as_u64().unwrap() as usize;
        let total = header["total"].as_str().unwrap();
        let start_indent = header["start_indent"].as_u64().unwrap() as usize;
        let idx_width = header["idx_width"].as_u64().unwrap() as usize;
        let id_width = header["id_width"].as_u64().unwrap() as usize;
        let dom_col = header["col"].as_u64().unwrap() as usize;
        let prefixes = header["prefixes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();

        let mut rows = Vec::with_capacity(row_count);
        for _ in 0..row_count {
            rows.push(lines.next().unwrap().strip_suffix('\n').unwrap());
        }

        let mut tid_names = std::collections::HashMap::new();
        for row in &rows {
            let parts = row.split('\t').collect::<Vec<_>>();
            match parts[0] {
                "SP" | "SK" | "SD" | "P" | "K" | "D" => {
                    let tid = if parts[2].is_empty() {
                        parts[1]
                    } else {
                        parts[2]
                    };
                    tid_names.insert(tid, expand_name(parts[3], &prefixes));
                }
                "R" => {
                    tid_names.insert(parts[5], expand_name(parts[6], &prefixes));
                }
                _ => {}
            }
        }

        for row in rows {
            let parts = row.split('\t').collect::<Vec<_>>();
            match parts[0] {
                "B" => output.push('\n'),
                "s" => {
                    let tid = parts[1];
                    let name = &tid_names[tid];
                    format_start(&mut output, start_indent, id_width, tid, name);
                }
                "S" => {
                    let lead_w = parts[1].parse::<usize>().unwrap();
                    let id_w = parts[2].parse::<usize>().unwrap();
                    let name = expand_name(parts[4], &prefixes);
                    format_start(&mut output, lead_w, id_w, parts[3], &name);
                }
                kind @ ("SP" | "SK" | "SD") => {
                    let status_code = &kind[1..];
                    let idx = parts[1];
                    let tid = if parts[2].is_empty() { idx } else { parts[2] };
                    let name = expand_name(parts[3], &prefixes);
                    let dots_override = parts.get(5).map(|value| value.parse::<usize>().unwrap());
                    format_start(&mut output, start_indent, id_width, tid, &name);
                    format_result(
                        &mut output,
                        idx_width,
                        idx,
                        total,
                        id_width,
                        tid,
                        &name,
                        status_code,
                        parts[4],
                        dots_override,
                        dom_col,
                    );
                }
                status_code @ ("P" | "K" | "D") => {
                    let idx = parts[1];
                    let tid = if parts[2].is_empty() { idx } else { parts[2] };
                    let name = expand_name(parts[3], &prefixes);
                    let dots_override = parts.get(5).map(|value| value.parse::<usize>().unwrap());
                    format_result(
                        &mut output,
                        idx_width,
                        idx,
                        total,
                        id_width,
                        tid,
                        &name,
                        status_code,
                        parts[4],
                        dots_override,
                        dom_col,
                    );
                }
                "R" => {
                    let idx_w = parts[1].parse::<usize>().unwrap();
                    let id_w = parts[4].parse::<usize>().unwrap();
                    let name = expand_name(parts[6], &prefixes);
                    let dots = parts[7].parse::<usize>().unwrap();
                    let sec_pad = parts[9].parse::<usize>().unwrap();
                    let sec_field = format!("{}{}", " ".repeat(sec_pad), parts[10]);
                    format_result(
                        &mut output,
                        idx_w,
                        parts[2],
                        parts[3],
                        id_w,
                        parts[5],
                        &name,
                        parts[8],
                        &sec_field,
                        Some(dots),
                        dom_col,
                    );
                }
                other => panic!("unexpected CTest row tag: {other}"),
            }
        }
    }
    output
}

fn expand_name(token: &str, prefixes: &[String]) -> String {
    if let Some(rest) = token.strip_prefix('@') {
        if rest.starts_with('@') {
            return rest.to_owned();
        }
        let (idx, suffix) = rest.split_once(':').unwrap();
        let prefix = &prefixes[idx.parse::<usize>().unwrap()];
        return format!("{prefix}{suffix}");
    }
    token.to_owned()
}

fn format_start(output: &mut String, lead_w: usize, id_w: usize, tid: &str, name: &str) {
    let id_pad = id_w.saturating_sub(tid.len());
    output.push_str(&" ".repeat(lead_w));
    output.push_str("Start ");
    output.push_str(&" ".repeat(id_pad));
    output.push_str(tid);
    output.push_str(": ");
    output.push_str(name);
    output.push('\n');
}

#[allow(clippy::too_many_arguments)]
fn format_result(
    output: &mut String,
    idx_w: usize,
    idx: &str,
    total: &str,
    id_w: usize,
    tid: &str,
    name: &str,
    status_code: &str,
    sec_tok: &str,
    dots_override: Option<usize>,
    dom_col: usize,
) {
    let idx_pad = idx_w.saturating_sub(idx.len());
    let id_pad = id_w.saturating_sub(tid.len());
    let head = format!(
        "{}{idx}/{total} Test {}#{tid}: {name} ",
        " ".repeat(idx_pad),
        " ".repeat(id_pad),
    );
    let dots = dots_override.unwrap_or_else(|| dom_col.saturating_sub(head.chars().count()).max(3));
    let (status_lit, exp_sec_w) = match status_code {
        "P" => ("   Passed", 8usize),
        "K" => ("***Skipped", 7usize),
        "D" => ("***Not Run (Disabled)", 7usize),
        other => panic!("unexpected CTest status code: {other}"),
    };
    output.push_str(&head);
    output.push_str(&".".repeat(dots));
    output.push_str(status_lit);
    if sec_tok.starts_with(' ') {
        output.push_str(sec_tok);
    } else {
        let pad = exp_sec_w.saturating_sub(sec_tok.len()).max(1);
        output.push_str(&" ".repeat(pad));
        output.push_str(sec_tok);
    }
    output.push_str(" sec\n");
}

/// Remove only the independently specified SGR grammar before comparison.
pub fn strip_sgr(input: &str) -> String {
    regex::Regex::new(r"\x1b\[[0-9;]*m")
        .unwrap()
        .replace_all(input, "")
        .into_owned()
}
