const HEADER: &str = "Gradle templates (id<TAB>fields; interleave with literals): ";

// Deliberately independent of the producer: no Gradle recognizers, edit plans,
// regexes or formatting helpers are shared with this decoder.
/// Reconstruct every line without using the producer's recognizers.
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
        let Some(header) = line.strip_prefix(HEADER) else {
            output.push_str(line);
            continue;
        };
        let header: serde_json::Value = serde_json::from_str(header).unwrap();
        let literals = header["literals"].as_array().unwrap();
        for _ in 0..header["rows"].as_u64().unwrap() {
            let row = lines.next().unwrap().strip_suffix('\n').unwrap();
            let (id_str, fields) = match row.split_once('\t') {
                Some((id_str, rest)) => (id_str, rest.split('\t').collect::<Vec<_>>()),
                None => (row, Vec::new()),
            };
            let id: usize = id_str.parse().unwrap();
            let parts = literals[id].as_array().unwrap();
            assert_eq!(fields.len() + 1, parts.len());
            output.push_str(parts[0].as_str().unwrap());
            for (field, literal) in fields.iter().zip(&parts[1..]) {
                output.push_str(field);
                output.push_str(literal.as_str().unwrap());
            }
            output.push('\n');
        }
    }
    output
}

/// Remove only the independently specified SGR grammar before comparison.
pub fn strip_sgr(input: &str) -> String {
    regex::Regex::new(r"\x1b\[[0-9;]*m")
        .unwrap()
        .replace_all(input, "")
        .into_owned()
}
